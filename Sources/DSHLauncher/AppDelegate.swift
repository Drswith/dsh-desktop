import AppKit
import DSHLauncherCore

/// Menu bar shell lifecycle: install the bundled runtime, supervise `dsh`, open the
/// Web UI in the default browser, and stop the service when the app quits.
final class AppDelegate: NSObject, NSApplicationDelegate {
    enum Keys {
        static let configuredLaunchAtLogin = "didConfigureLaunchAtLogin"
        static let skipQuitConfirmation = "skipQuitConfirmation"
    }

    enum RunAction {
        case start
        case restart
    }

    let info = AppInfo(bundle: .main)
    let defaults = UserDefaults.standard
    let bootstrapQueue = DispatchQueue(label: "dsh-launcher.bootstrap")
    private(set) lazy var paths = AppPaths.standard(dirName: info.homeDirName)
    private(set) var log: FileLogger!
    private(set) var supervisor: DaemonSupervisor!
    private(set) var installer: RuntimeInstaller!
    private(set) var menu: StatusMenuController!
    private(set) lazy var statusWindow: StatusWindowController = {
        let controller = StatusWindowController()
        controller.onRetry = { [weak self] in self?.prepareAndRun(.restart) }
        controller.onOpenLogs = { [weak self] in self?.perform(.openLogs) }
        return controller
    }()

    var config = ShellConfig()
    var plan: DaemonLaunchPlan?
    /// Receipt of the bundled runtime in use; nil for a config.json runtime override.
    var runtimeReceipt: RuntimeReceipt?
    var openWhenReady = false
    private var showProgressUntilReady = false
    private var launchedAtLogin = false
    private var terminating = false
    private var lastBrowserOpen = Date.distantPast
    private var lockDescriptor: Int32 = -1
    /// Only touched on `bootstrapQueue`.
    private var orphanChecked = false
    private var signalSources: [DispatchSourceSignal] = []
    /// Set for SIGTERM/SIGINT: nobody is at the screen to answer the quit confirmation.
    private var quitWithoutConfirmation = false

    private var openRequestNotification: Notification.Name { Notification.Name("\(info.bundleIdentifier).open") }

    // MARK: Lifecycle

    func applicationWillFinishLaunching(_ notification: Notification) {
        launchedAtLogin = Self.isLoginItemLaunch()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        try? paths.prepare()
        guard acquireSingleInstanceLock() else {
            // Another copy owns the daemon; ask it to open the UI instead.
            DistributedNotificationCenter.default().postNotificationName(openRequestNotification, object: nil, userInfo: nil, deliverImmediately: true)
            exit(0)
        }
        installSignalHandlers()
        log = FileLogger(url: paths.shellLog)
        let output = FileLogger(url: paths.daemonLog, echoToStderr: false)
        log.log("\(info.displayName) \(info.versionLabel) (\(info.build)) commit=\(info.gitCommit ?? "unknown") launched pid=\(getpid()) loginItem=\(launchedAtLogin) bundle=\(Bundle.main.bundlePath)")
        reloadConfig()
        installer = RuntimeInstaller(paths: paths, logger: log, payloadDirectory: info.payloadDirectory)
        supervisor = DaemonSupervisor(paths: paths, log: log, output: output)
        supervisor.onStateChange = { [weak self] state in self?.daemonStateChanged(state) }

        NSApp.mainMenu = makeMainMenu()
        var model = MenuModel(appName: info.displayName)
        model.canRepair = installer.bundledManifest() != nil
        model.showsDockIcon = NSApp.activationPolicy() == .regular
        model.launchAtLogin = LaunchAtLogin.status
        menu = StatusMenuController(model: model)
        menu.delegate = self
        observeSystem()
        configureLaunchAtLoginOnFirstRun()

        // Login launches stay silent; DSH_LAUNCHER_NO_OPEN=1 does the same for development runs.
        let quietLaunch = launchedAtLogin || ProcessInfo.processInfo.environment["DSH_LAUNCHER_NO_OPEN"] == "1"
        openWhenReady = !quietLaunch
        showProgressUntilReady = !quietLaunch
        prepareAndRun(.start)
    }

    /// Opening the app again (Finder, Launchpad, Dock) brings back a Dock icon hidden
    /// for this session, then opens DSH.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        if NSApp.activationPolicy() != .regular {
            setDockIconVisible(true)
            log.log("restoring Dock icon from app reopen")
        }
        perform(.open)
        return false
    }

    func applicationDockMenu(_ sender: NSApplication) -> NSMenu? {
        menu.makeDockMenu()
    }

    func application(_ application: NSApplication, open urls: [URL]) {
        urls.forEach(handleDeepLink)
    }

    /// Every quit path lands here: the status menu, ⌘Q, the Dock's Quit, Activity Monitor
    /// and AppleScript. Each one confirms while the service runs, except logout, restart,
    /// shutdown and signals, which must never wait for a click.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let supervisor, !terminating, supervisor.currentState.isActive else { return .terminateNow }
        if !quitWithoutConfirmation, !QuitRequest.isSystemInitiated(NSAppleEventManager.shared().currentAppleEvent), !confirmQuit() {
            log.log("quit cancelled")
            return .terminateCancel
        }
        terminating = true
        log.log("quitting: stopping the dsh service")
        var replied = false
        let reply = {
            guard !replied else { return }
            replied = true
            NSApp.reply(toApplicationShouldTerminate: true)
        }
        supervisor.stop(completion: reply)
        // A run loop timer, not GCD, so the fallback fires inside the modal termination loop.
        RunLoop.main.add(Timer(timeInterval: 15, repeats: false) { _ in reply() }, forMode: .common)
        return .terminateLater
    }

    func applicationWillTerminate(_ notification: Notification) {
        log?.log("\(info.displayName) exiting")
        log?.flush()
    }

    // MARK: Runtime and service

    /// Resolve the runtime (installing the bundled payload when needed), rebuild the
    /// launch plan from the current login-shell environment, then start or restart.
    func prepareAndRun(_ action: RunAction, reinstall: Bool = false) {
        bootstrapQueue.async { [self] in
            do {
                reloadConfig()
                var runtime: InstalledRuntime?
                if config.runtime == nil {
                    if reinstall {
                        DispatchQueue.main.async { self.setInstalling(true) }
                        runtime = try installer.installBundled()
                    } else {
                        runtime = try installer.ensureRuntime { _ in
                            DispatchQueue.main.async { self.setInstalling(true) }
                        }
                    }
                }
                DispatchQueue.main.async { self.setInstalling(false) }
                if !orphanChecked {
                    orphanChecked = true
                    supervisor.terminateOrphan()
                }
                let plan = try makePlan(runtime: runtime)
                DispatchQueue.main.async {
                    self.plan = plan
                    self.runtimeReceipt = runtime?.receipt
                    self.menu.model.runtimeSummary = plan.runtimeSummary
                    if self.showProgressUntilReady {
                        self.statusWindow.showProgress(title: L10n.tr("window.title"), detail: L10n.tr("window.starting"))
                    }
                    switch action {
                    case .start: self.supervisor.start(plan)
                    case .restart: self.supervisor.restart(plan)
                    }
                }
            } catch {
                log.log("prepare failed: \(error.localizedDescription)")
                DispatchQueue.main.async {
                    self.setInstalling(false)
                    self.menu.model.state = .failed(error.localizedDescription)
                    self.presentFailure(error.localizedDescription)
                }
            }
        }
    }

    private func setInstalling(_ installing: Bool) {
        guard menu.model.installing != installing else { return }
        menu.model.installing = installing
        if installing, !launchedAtLogin || showProgressUntilReady {
            statusWindow.showProgress(title: L10n.tr("window.title"), detail: L10n.tr("window.installing"))
        }
    }

    private func makePlan(runtime: InstalledRuntime?) throws -> DaemonLaunchPlan {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let shell = Self.loginShell()
        var login: [String: String]?
        switch ShellEnvironment.resolveLoginEnvironment(shell: shell) {
        case .success(let environment):
            login = environment
            log.log("login shell environment resolved shell=\(shell) variables=\(environment.count)")
        case .failure(let error):
            log.log("login shell environment unavailable (\(error.localizedDescription)); using the fallback PATH")
        }
        let dshHome = DaemonLaunchPlan.resolveDshHome(
            configured: config.dshHome,
            environment: login ?? ProcessInfo.processInfo.environment,
            home: home
        )
        var overrides = config.environment ?? [:]
        if config.dshHome != nil { overrides["DSH_HOME"] = dshHome.path }
        let environment = ShellEnvironment.daemonEnvironment(login: login, overrides: overrides)

        let node: URL
        let entry: URL
        var dshVersion = runtime?.receipt.dshVersion
        if let external = config.runtime {
            node = URL(fileURLWithPath: (external.node as NSString).expandingTildeInPath)
            entry = URL(fileURLWithPath: (external.entry as NSString).expandingTildeInPath)
            guard FileManager.default.isExecutableFile(atPath: node.path) else {
                throw CommandError("config.json runtime.node is not executable: \(node.path)")
            }
            guard FileManager.default.fileExists(atPath: entry.path) else {
                throw CommandError("config.json runtime.entry does not exist: \(entry.path)")
            }
            dshVersion = Self.packageVersion(nearEntry: entry)
        } else if let runtime {
            node = runtime.nodeExecutable
            entry = runtime.dshEntry
        } else {
            throw CommandError("no dsh runtime is available")
        }
        return DaemonLaunchPlan(
            node: node,
            entry: entry,
            dshVersion: dshVersion,
            nodeVersion: runtime?.receipt.nodeVersion,
            profile: config.profile ?? info.profile,
            dshHome: dshHome,
            preferredPort: config.port ?? info.defaultPort,
            extraArgs: config.extraArgs ?? [],
            environment: environment,
            workingDirectory: home
        )
    }

    private func reloadConfig() {
        do {
            config = try ShellConfig.load(from: paths.configFile)
        } catch {
            log?.log("config.json ignored: \(error.localizedDescription)")
            config = ShellConfig()
        }
    }

    private func daemonStateChanged(_ state: DaemonState) {
        menu.model.state = state
        switch state {
        case .running(let daemon):
            if openWhenReady {
                openWhenReady = false
                openBrowser(daemon)
            }
            if showProgressUntilReady || statusWindow.isVisible {
                showProgressUntilReady = false
                statusWindow.showProgress(title: L10n.tr("window.title"), detail: L10n.tr("window.ready"))
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) { [weak self] in
                    guard let self, self.supervisor.currentState.info != nil else { return }
                    self.statusWindow.close()
                }
            }
        case .failed(let message):
            openWhenReady = false
            showProgressUntilReady = false
            presentFailure(message)
        case .idle, .starting, .stopping, .stopped:
            break
        }
    }

    func openBrowser(_ daemon: DaemonInfo) {
        // Menu, Dock and deep-link requests can arrive together; open one tab.
        guard Date().timeIntervalSince(lastBrowserOpen) > 2 else { return }
        lastBrowserOpen = Date()
        log.log("opening \(daemon.cleanURL.absoluteString) in the default browser")
        NSWorkspace.shared.open(daemon.authenticatedURL)
    }

    func presentFailure(_ message: String) {
        let tail = supervisor?.recentErrorLines().suffix(4).joined(separator: "\n") ?? ""
        let detail = tail.isEmpty || message.contains(tail) ? message : "\(message)\n\n\(tail)"
        statusWindow.showFailure(title: L10n.tr("dialog.failed.title"), detail: detail)
    }

    // MARK: System integration

    private func observeSystem() {
        let center = NSWorkspace.shared.notificationCenter
        center.addObserver(forName: NSWorkspace.willSleepNotification, object: nil, queue: .main) { [weak self] _ in
            self?.log.log("system will sleep")
            self?.supervisor.systemWillSleep()
        }
        center.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { [weak self] _ in
            self?.log.log("system did wake")
            self?.supervisor.systemDidWake()
        }
        DistributedNotificationCenter.default().addObserver(
            forName: openRequestNotification, object: nil, queue: .main
        ) { [weak self] _ in
            self?.perform(.open)
        }
    }

    private func configureLaunchAtLoginOnFirstRun() {
        guard !defaults.bool(forKey: Keys.configuredLaunchAtLogin) else { return }
        guard info.isInStableLocation else {
            log.log("launch at login left unchanged: the app is not in an Applications folder")
            return
        }
        defaults.set(true, forKey: Keys.configuredLaunchAtLogin)
        do {
            try LaunchAtLogin.setEnabled(true)
            log.log("launch at login enabled on first run status=\(LaunchAtLogin.status)")
        } catch {
            log.log("launch at login could not be enabled: \(error.localizedDescription)")
        }
        menu.model.launchAtLogin = LaunchAtLogin.status
    }

    /// Show or hide the Dock icon for the current app session only; every launch starts with it shown.
    func setDockIconVisible(_ visible: Bool) {
        NSApp.setActivationPolicy(visible ? .regular : .accessory)
        menu.model.showsDockIcon = visible
        if visible { NSApp.activate(ignoringOtherApps: true) }
    }

    /// `kill`/`launchctl stop` send SIGTERM, which would otherwise end the app at once
    /// and orphan the daemon; route it (and Ctrl-C in a terminal) through a normal quit.
    private func installSignalHandlers() {
        for signalNumber in [SIGTERM, SIGINT] {
            signal(signalNumber, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: signalNumber, queue: .main)
            source.setEventHandler { [weak self] in
                self?.log?.log("received signal \(signalNumber); quitting")
                self?.quitWithoutConfirmation = true
                // Leave this main-queue block first: `.terminateLater` spins a nested run
                // loop that cannot drain the main queue while the block is still running.
                RunLoop.main.perform { NSApp.terminate(nil) }
            }
            source.resume()
            signalSources.append(source)
        }
    }

    private func acquireSingleInstanceLock() -> Bool {
        let descriptor = open(paths.lockFile.path, O_CREAT | O_RDWR | O_CLOEXEC, 0o600)
        guard descriptor >= 0 else { return true }
        guard flock(descriptor, LOCK_EX | LOCK_NB) == 0 else {
            close(descriptor)
            return false
        }
        lockDescriptor = descriptor
        return true
    }

    private static func isLoginItemLaunch() -> Bool {
        guard let event = NSAppleEventManager.shared().currentAppleEvent,
              event.eventID == AEEventID(kAEOpenApplication) else { return false }
        return event.paramDescriptor(forKeyword: AEKeyword(keyAEPropData))?.enumCodeValue == OSType(keyAELaunchedAsLogInItem)
    }

    private static func loginShell() -> String {
        if let entry = getpwuid(getuid()), let shell = entry.pointee.pw_shell {
            let path = String(cString: shell)
            if !path.isEmpty { return path }
        }
        return ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh"
    }

    private static func packageVersion(nearEntry entry: URL) -> String? {
        let manifest = entry.deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("package.json")
        guard let data = try? Data(contentsOf: manifest),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        return object["version"] as? String
    }
}
