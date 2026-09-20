import AppKit
import DSHLauncherCore
import UniformTypeIdentifiers

extension AppDelegate: StatusMenuDelegate {
    func perform(_ command: MenuCommand) {
        switch command {
        case .open: openUI()
        case .copyURL: copyAccessLink()
        case .restart: prepareAndRun(.restart)
        case .stop: supervisor.stop()
        case .start: prepareAndRun(.start)
        case .toggleLaunchAtLogin: toggleLaunchAtLogin()
        case .openLoginItems: LaunchAtLogin.openSystemSettings()
        case .toggleDock: toggleDockIcon()
        case .toggleKeepPreviousRuntime: toggleKeepPreviousRuntime()
        case .openLogs: NSWorkspace.shared.activateFileViewerSelecting([paths.shellLog, paths.daemonLog])
        case .openDshHome: openDshHome()
        case .editConfig: editConfig()
        case .repair: confirmRepair()
        case .checkForUpdates: checkForUpdates(userInitiated: true)
        case .applyUpdate: switchToPendingRuntime(force: true)
        case .channelStable: setUpdateChannel(.stable)
        case .channelPreview: setUpdateChannel(.preview)
        case .about: showAboutPanel(nil)
        case .quit: NSApp.terminate(nil)
        }
    }

    /// `dsh-launcher://open|start|stop|restart|logs`
    func handleDeepLink(_ url: URL) {
        guard url.scheme?.lowercased() == info.urlScheme.lowercased() else { return }
        let action = (url.host ?? url.path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))).lowercased()
        log.log("deep link action=\(action.isEmpty ? "open" : action)")
        switch action {
        case "", "open": perform(.open)
        case "start": perform(.start)
        case "stop": perform(.stop)
        case "restart": perform(.restart)
        case "logs": perform(.openLogs)
        default: log.log("deep link ignored: unknown action")
        }
    }

    private func openUI() {
        let state = supervisor.currentState
        if let daemon = state.info {
            openBrowser(daemon)
            return
        }
        openWhenReady = true
        switch state {
        case .starting, .stopping:
            statusWindow.showProgress(title: L10n.tr("window.title"), detail: L10n.tr("window.starting"))
        case .idle where menu.model.installing:
            break
        case .idle, .stopped, .failed, .running:
            prepareAndRun(.start)
        }
    }

    private func copyAccessLink() {
        guard let daemon = supervisor.currentState.info else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(daemon.authenticatedURL.absoluteString, forType: .string)
    }

    private func toggleLaunchAtLogin() {
        let enable = LaunchAtLogin.status == .disabled
        do {
            try LaunchAtLogin.setEnabled(enable)
            log.log("launch at login \(enable ? "enabled" : "disabled") status=\(LaunchAtLogin.status)")
        } catch {
            log.log("launch at login change failed: \(error.localizedDescription)")
            alert(title: L10n.tr("dialog.launchAtLogin.failed", error.localizedDescription), body: "")
        }
        menu.model.launchAtLogin = LaunchAtLogin.status
        if menu.model.launchAtLogin == .requiresApproval {
            let open = alert(
                title: L10n.tr("dialog.launchAtLogin.title"),
                body: L10n.tr("dialog.launchAtLogin.approval", info.displayName),
                buttons: [L10n.tr("menu.openLoginItemsSettings"), L10n.tr("action.close")]
            )
            if open == .alertFirstButtonReturn { LaunchAtLogin.openSystemSettings() }
        }
    }

    private func toggleDockIcon() {
        let visible = NSApp.activationPolicy() != .regular
        setDockIconVisible(visible)
        log.log("\(visible ? "showing" : "hiding") Dock icon for current app session")
    }

    /// Turning it off removes the kept runtime now; turning it on takes effect at the next upgrade.
    private func toggleKeepPreviousRuntime() {
        let keep = !keepsPreviousRuntime
        defaults.set(keep, forKey: Keys.keepPreviousRuntime)
        menu.model.keepsPreviousRuntime = keep
        log.log("keep previous runtime after upgrades=\(keep)")
        // The bootstrap queue also runs installs, so the two never overlap.
        bootstrapQueue.async { [self] in
            installer.keepsPreviousRuntime = keep
            installer.applyRetention()
        }
    }

    private func openDshHome() {
        let home = plan?.dshHome ?? FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".dsh", isDirectory: true)
        NSWorkspace.shared.open(home)
    }

    /// Create a documented config.json on first use, then open it in the default text editor.
    private func editConfig() {
        let file = paths.configFile
        if !FileManager.default.fileExists(atPath: file.path) {
            let template = """
            {
              "port": \(info.defaultPort),
              "profile": "\(info.profile)",
              "extraArgs": [],
              "environment": {}
            }

            """
            FileManager.default.createFile(atPath: file.path, contents: Data(template.utf8), attributes: [.posixPermissions: 0o600])
        }
        if let editor = NSWorkspace.shared.urlForApplication(toOpen: .plainText) {
            NSWorkspace.shared.open([file], withApplicationAt: editor, configuration: NSWorkspace.OpenConfiguration())
        } else {
            NSWorkspace.shared.activateFileViewerSelecting([file])
        }
    }

    private func confirmRepair() {
        let version = installer.bundledManifest()?.dshVersion ?? "?"
        var body = L10n.tr("dialog.repair.body", version)
        // After an update the bundled runtime can be older than the one in use.
        if let installed = installer.currentRuntime()?.receipt.dshVersion,
           let current = SemVer(installed), let bundled = SemVer(version), bundled < current {
            body += "\n\n" + L10n.tr("dialog.repair.downgrade", installed, version)
        }
        let choice = alert(
            title: L10n.tr("dialog.repair.title"),
            body: body,
            buttons: [L10n.tr("dialog.repair.action"), L10n.tr("dialog.cancel")]
        )
        guard choice == .alertFirstButtonReturn else { return }
        log.log("runtime repair requested")
        supervisor.stop { [weak self] in
            self?.prepareAndRun(.start, reinstall: true)
        }
    }

    /// VS Code-style About dialog, shared by the status menu and the app menu:
    /// icon, name, one "Key: value" line per fact, and a Copy button for bug reports.
    @objc func showAboutPanel(_ sender: Any?) {
        let details = aboutDetails().joined(separator: "\n")
        let alert = NSAlert()
        alert.icon = NSApp.applicationIconImage
        alert.messageText = info.displayName
        alert.informativeText = details
        // The first button is the default one, drawn on the right as in VS Code.
        alert.addButton(withTitle: L10n.tr("about.copy"))
        alert.addButton(withTitle: L10n.tr("about.ok")).keyEquivalent = "\u{1b}"
        NSApp.activate(ignoringOtherApps: true)
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(details, forType: .string)
    }

    func aboutDetails() -> [String] {
        // CFBundleVersion stays out: Commit and Date identify a build more precisely.
        var lines = ["Version: \(info.versionLabel)"]
        if let repo = info.repoEntry { lines.append("\(repo.key): \(repo.value)") }
        if let commit = info.gitCommit { lines.append("Commit: \(commit)") }
        if let date = info.buildDate {
            lines.append("Date: \(date)" + (Self.relativeAge(ofISODate: date).map { " (\($0))" } ?? ""))
        }
        if config.runtime != nil, let plan {
            lines.append("DSH: \(plan.dshVersion ?? "unknown") (config.json)")
        } else if let receipt = runtimeReceipt {
            lines += ["DSH: \(receipt.dshVersion)", "Node.js: \(receipt.nodeVersion)", "pnpm: \(receipt.pnpmVersion)"]
        }
        lines.append("OS: \(Self.operatingSystem())")
        return lines
    }

    /// "1 周前" / "1 week ago", in the user's language.
    static func relativeAge(ofISODate text: String, now: Date = Date()) -> String? {
        guard let date = ISO8601DateFormatter().date(from: text) else { return nil }
        let formatter = RelativeDateTimeFormatter()
        formatter.dateTimeStyle = .named
        return formatter.localizedString(for: date, relativeTo: now)
    }

    /// `Darwin arm64 24.6.0`, the same shape as VS Code's `os.type() os.arch() os.release()`.
    static func operatingSystem() -> String {
        var name = utsname()
        uname(&name)
        func text<T>(_ field: T) -> String {
            withUnsafeBytes(of: field) { String(decoding: $0.prefix(while: { $0 != 0 }), as: UTF8.self) }
        }
        return "\(text(name.sysname)) \(text(name.machine)) \(text(name.release))"
    }

    /// Ask before stopping the running service; "Don't ask again" skips it from then on.
    /// - Returns: `true` to go on quitting.
    func confirmQuit() -> Bool {
        guard !defaults.bool(forKey: Keys.skipQuitConfirmation) else { return true }
        let dialog = NSAlert()
        dialog.messageText = L10n.tr("dialog.quit.title", info.displayName)
        dialog.informativeText = L10n.tr("dialog.quit.body")
        dialog.addButton(withTitle: L10n.tr("dialog.quit.action"))
        dialog.addButton(withTitle: L10n.tr("dialog.cancel"))
        dialog.showsSuppressionButton = true
        NSApp.activate(ignoringOtherApps: true)
        guard dialog.runModal() == .alertFirstButtonReturn else { return false }
        if dialog.suppressionButton?.state == .on { defaults.set(true, forKey: Keys.skipQuitConfirmation) }
        return true
    }

    @discardableResult
    func alert(title: String, body: String, buttons: [String] = []) -> NSApplication.ModalResponse {
        let dialog = NSAlert()
        dialog.messageText = title
        dialog.informativeText = body
        (buttons.isEmpty ? [L10n.tr("action.close")] : buttons).forEach { dialog.addButton(withTitle: $0) }
        NSApp.activate(ignoringOtherApps: true)
        return dialog.runModal()
    }

    /// App and Edit menus: shown when the Dock icon is on, and they give the
    /// status window's selectable text the standard copy shortcuts.
    func makeMainMenu() -> NSMenu {
        let appName = info.displayName
        let main = NSMenu()
        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        let about = appMenu.addItem(withTitle: L10n.tr("menu.about", appName), action: #selector(showAboutPanel(_:)), keyEquivalent: "")
        about.target = self
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: L10n.tr("menu.hide", appName), action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: L10n.tr("menu.quit", appName), action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        main.addItem(appItem)

        let editItem = NSMenuItem()
        let editMenu = NSMenu(title: L10n.tr("menu.edit"))
        editMenu.addItem(withTitle: L10n.tr("menu.copy"), action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        editMenu.addItem(withTitle: L10n.tr("menu.selectAll"), action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = editMenu
        main.addItem(editItem)
        return main
    }
}
