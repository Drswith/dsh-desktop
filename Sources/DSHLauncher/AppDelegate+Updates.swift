import AppKit
import DSHLauncherCore

/// Runtime updates: check the channel feed, download and stage a newer runtime in
/// the background, restart onto it once the service is idle, and confirm the
/// switch when the service starts on it, going back when it cannot.
extension AppDelegate {
    private static let checkInterval: TimeInterval = 6 * 3600
    private static let idlePollInterval: TimeInterval = 60
    /// How long session logs must stay unwritten before the service counts as idle.
    private static let sessionQuietPeriod: TimeInterval = 10 * 60

    var updateChannel: UpdateChannel {
        defaults.string(forKey: Keys.updateChannel).flatMap(UpdateChannel.init(rawValue:)) ?? .stable
    }

    func configureUpdates() {
        guard let feed = info.updateFeed else {
            log.log("runtime updates off: this build carries no update feed key")
            return
        }
        updater = RuntimeUpdater(feed: feed, arch: AppInfo.architecture, shellVersion: info.version, downloadsDirectory: paths.downloadsDir)
        menu.model.updatesEnabled = true
        menu.model.updateChannel = updateChannel
        log.log("runtime updates on: channel=\(updateChannel.rawValue) feed=\(feed.baseURL.absoluteString)")
    }

    /// Check shortly after the service first comes up, then every six hours.
    func scheduleUpdateChecks() {
        guard updater != nil, updateTimer == nil else { return }
        let delay = Self.developmentInterval("DSH_LAUNCHER_UPDATE_DELAY") ?? 30
        let timer = DispatchSource.makeTimerSource(queue: .main)
        timer.schedule(deadline: .now() + delay, repeating: Self.checkInterval, leeway: .seconds(60))
        timer.setEventHandler { [weak self] in self?.checkForUpdates(userInitiated: false) }
        timer.resume()
        updateTimer = timer
    }

    func checkForUpdates(userInitiated: Bool) {
        guard let updater, !updateCheckInFlight else { return }
        guard config.runtime == nil else {
            if userInitiated { alert(title: L10n.tr("dialog.update.external.title"), body: L10n.tr("dialog.update.external.body")) }
            return
        }
        updateCheckInFlight = true
        let before = menu.model.updateStatus
        menu.model.updateStatus = .checking
        let channel = updateChannel
        let failed = Set(UpdateState.load(from: paths.updateStateFile).failedIdentities)
        updateQueue.async { [self] in
            do {
                let decision = try updater.decide(channel: channel, current: installer.currentRuntime()?.receipt,
                                                  pending: installer.pendingRuntime()?.receipt, failedIdentities: failed)
                switch decision {
                case .stage(let manifest, let artifact):
                    log.log("runtime update: downloading dsh=\(manifest.dshVersion) (\(artifact.size) bytes) from channel=\(channel.rawValue)")
                    DispatchQueue.main.async { self.menu.model.updateStatus = .downloading(manifest.dshVersion) }
                    let archive = try updater.download(artifact, for: manifest)
                    // Installs belong to the bootstrap queue, so staging never races a restart.
                    bootstrapQueue.async {
                        defer { try? FileManager.default.removeItem(at: archive) }
                        do {
                            try self.installer.setPending(self.installer.stage(archive: archive, manifest: manifest, source: .update))
                            DispatchQueue.main.async { self.updateStaged(manifest.dshVersion, userInitiated: userInitiated) }
                        } catch {
                            DispatchQueue.main.async { self.updateCheckFailed(error, userInitiated: userInitiated) }
                        }
                    }
                case .staged(let version):
                    DispatchQueue.main.async { self.updateStaged(version, userInitiated: userInitiated) }
                case .upToDate(let version):
                    log.log("runtime update: dsh=\(version) is the newest on channel=\(channel.rawValue)")
                    DispatchQueue.main.async {
                        self.finishCheck(restoring: before)
                        if userInitiated {
                            self.alert(title: L10n.tr("dialog.update.upToDate.title"),
                                       body: L10n.tr("dialog.update.upToDate.body", version, Self.channelName(channel)))
                        }
                    }
                case .skip(let reason):
                    log.log("runtime update skipped: \(reason)")
                    DispatchQueue.main.async {
                        self.finishCheck(restoring: before)
                        if userInitiated {
                            self.alert(title: L10n.tr("dialog.update.none.title"), body: L10n.tr("dialog.update.none.body", reason))
                        }
                    }
                }
            } catch {
                DispatchQueue.main.async { self.updateCheckFailed(error, userInitiated: userInitiated) }
            }
        }
    }

    /// Restart the service onto the staged runtime: at once when `force`, otherwise
    /// only while the service runs and looks idle.
    func switchToPendingRuntime(force: Bool) {
        guard let pending = installer.pendingRuntime() else {
            disarmIdleSwitch()
            return
        }
        if !force {
            guard case .running(let daemon) = supervisor.currentState, let dshHome = plan?.dshHome else { return }
            let sessions = dshHome.appendingPathComponent("sessions", isDirectory: true)
            if let reason = ActivityProbe.busyReason(daemonPid: daemon.pid, sessionsRoot: sessions, quietPeriod: Self.sessionQuietPeriod) {
                if Date().timeIntervalSince(lastBusyLog) > 30 * 60 {
                    lastBusyLog = Date()
                    log.log("runtime update: waiting for the service to go idle: \(reason)")
                }
                return
            }
        }
        disarmIdleSwitch()
        log.log("runtime update: restarting onto dsh=\(pending.receipt.dshVersion)\(force ? " on request" : " while idle")")
        menu.model.updateStatus = .applying(pending.receipt.dshVersion)
        prepareAndRun(.restart)
    }

    /// On the bootstrap queue, before the runtime is resolved: switch to a staged
    /// runtime on probation.
    func beginPendingRuntimeSwitch() {
        do {
            guard let activation = try installer.beginPendingActivation() else { return }
            DispatchQueue.main.async { self.menu.model.updateStatus = .applying(activation.dshVersion) }
        } catch {
            log.log("runtime update: could not switch to the pending runtime: \(error.localizedDescription)")
        }
    }

    /// Settle a switch on probation from the service state: the first start confirms
    /// it; a crash before that, or a failure, goes back. Returns true when the state
    /// is handled by going back and must not be reported as a failure.
    func settleRuntimeSwitch(_ state: DaemonState) -> Bool {
        guard let activation = installer.currentActivation() else { return false }
        switch state {
        case .running:
            log.log("runtime update: dsh=\(activation.dshVersion) passed its start confirmation")
            menu.model.updateStatus = .updated(activation.dshVersion)
            bootstrapQueue.async { [self] in installer.confirmActivation() }
            return false
        case .starting(let attempt) where attempt > 1:
            rollBackRuntimeSwitch(activation, reason: "the service exited before it was ready")
            return true
        case .failed(let message):
            rollBackRuntimeSwitch(activation, reason: message)
            return true
        case .idle, .starting, .stopping, .stopped:
            return false
        }
    }

    func setUpdateChannel(_ channel: UpdateChannel) {
        guard channel != updateChannel else { return }
        defaults.set(channel.rawValue, forKey: Keys.updateChannel)
        menu.model.updateChannel = channel
        log.log("update channel=\(channel.rawValue)")
        // A runtime staged from the other channel is not wanted any more.
        disarmIdleSwitch()
        if case .staged = menu.model.updateStatus { menu.model.updateStatus = .idle }
        bootstrapQueue.async { [self] in
            try? installer.setPending(nil)
            DispatchQueue.main.async { self.checkForUpdates(userInitiated: false) }
        }
    }

    // MARK: Helpers

    private func updateStaged(_ version: String, userInitiated: Bool) {
        updateCheckInFlight = false
        menu.model.updateStatus = .staged(version)
        log.log("runtime update: dsh=\(version) staged; switching when the service is idle")
        armIdleSwitch()
        guard userInitiated else {
            switchToPendingRuntime(force: false)
            return
        }
        let choice = alert(title: L10n.tr("dialog.update.staged.title", version), body: L10n.tr("dialog.update.staged.body"),
                           buttons: [L10n.tr("dialog.update.staged.now"), L10n.tr("dialog.update.staged.later")])
        if choice == .alertFirstButtonReturn { switchToPendingRuntime(force: true) }
    }

    private func updateCheckFailed(_ error: Error, userInitiated: Bool) {
        updateCheckInFlight = false
        menu.model.updateStatus = .failed
        log.log("runtime update check failed: \(error.localizedDescription)")
        if userInitiated {
            alert(title: L10n.tr("dialog.update.failed.title"), body: error.localizedDescription)
        }
    }

    /// Keep what the menu said before an uneventful check, except transient states.
    private func finishCheck(restoring before: UpdateStatus) {
        updateCheckInFlight = false
        switch before {
        case .updated, .rolledBack: menu.model.updateStatus = before
        default: menu.model.updateStatus = .idle
        }
    }

    private func rollBackRuntimeSwitch(_ activation: RuntimeActivation, reason: String) {
        log.log("runtime update: dsh=\(activation.dshVersion) failed its start confirmation (\(reason)); going back")
        var state = UpdateState.load(from: paths.updateStateFile)
        if !state.failedIdentities.contains(activation.identity) {
            state.failedIdentities.append(activation.identity)
            try? state.save(to: paths.updateStateFile)
        }
        // Stop first: the supervisor's crash restarts would otherwise start the failed
        // runtime again while it is being removed.
        supervisor.stop { [weak self] in
            guard let self else { return }
            self.bootstrapQueue.async {
                let restored = self.installer.rollBackActivation()
                DispatchQueue.main.async {
                    guard let restored else {
                        self.menu.model.updateStatus = .failed
                        self.presentFailure(reason)
                        return
                    }
                    self.menu.model.updateStatus = .rolledBack(failed: activation.dshVersion, current: restored.receipt.dshVersion)
                    self.prepareAndRun(.restart)
                }
            }
        }
    }

    private func armIdleSwitch() {
        guard idleSwitchTimer == nil else { return }
        let interval = Self.developmentInterval("DSH_LAUNCHER_IDLE_POLL") ?? Self.idlePollInterval
        let timer = DispatchSource.makeTimerSource(queue: .main)
        timer.schedule(deadline: .now() + interval, repeating: interval, leeway: .seconds(5))
        timer.setEventHandler { [weak self] in self?.switchToPendingRuntime(force: false) }
        timer.resume()
        idleSwitchTimer = timer
    }

    private func disarmIdleSwitch() {
        idleSwitchTimer?.cancel()
        idleSwitchTimer = nil
    }

    private static func channelName(_ channel: UpdateChannel) -> String {
        L10n.tr("menu.channel.\(channel.rawValue)")
    }

    /// Development overrides in seconds, such as DSH_LAUNCHER_UPDATE_DELAY=5.
    private static func developmentInterval(_ name: String) -> TimeInterval? {
        ProcessInfo.processInfo.environment[name].flatMap(TimeInterval.init)
    }
}
