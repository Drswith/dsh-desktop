import AppKit
import DSHLauncherCore

enum MenuCommand: String {
    case open, copyURL, restart, stop, start
    case toggleLaunchAtLogin, openLoginItems, toggleDock
    case openLogs, openDshHome, editConfig, repair, about, quit
}

protocol StatusMenuDelegate: AnyObject {
    func perform(_ command: MenuCommand)
}

struct MenuModel {
    var appName: String
    var state: DaemonState = .idle
    var installing = false
    var runtimeSummary: String?
    var launchAtLogin: LaunchAtLoginStatus = .disabled
    var showsDockIcon = false
    var canRepair = false
}

/// The status item and its menu, rebuilt from `MenuModel` whenever it opens or changes.
final class StatusMenuController: NSObject, NSMenuDelegate {
    weak var delegate: StatusMenuDelegate?
    var model: MenuModel {
        didSet {
            refreshButton()
            if menuIsOpen { rebuild() }
        }
    }

    private let statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    private let menu = NSMenu()
    private var menuIsOpen = false

    init(model: MenuModel) {
        self.model = model
        super.init()
        menu.delegate = self
        menu.autoenablesItems = false
        statusItem.menu = menu
        if let button = statusItem.button {
            let image = NSImage(named: "MenuBarIconTemplate") ?? NSImage(systemSymbolName: "terminal", accessibilityDescription: nil)
            image?.isTemplate = true
            image?.size = NSSize(width: 16, height: 16)
            button.image = image
            button.imagePosition = .imageOnly
        }
        refreshButton()
    }

    var statusText: String {
        if model.installing { return L10n.tr("status.installing") }
        switch model.state {
        case .idle: return L10n.tr("status.idle")
        case .starting(let attempt): return attempt <= 1 ? L10n.tr("status.starting") : L10n.tr("status.restarting", attempt)
        case .running(let info): return L10n.tr("status.running", "127.0.0.1:\(info.port)")
        case .stopping: return L10n.tr("status.stopping")
        case .stopped: return L10n.tr("status.stopped")
        case .failed: return L10n.tr("status.failed")
        }
    }

    // MARK: NSMenuDelegate

    func menuNeedsUpdate(_ menu: NSMenu) {
        model.launchAtLogin = LaunchAtLogin.status
        model.showsDockIcon = NSApp.activationPolicy() == .regular
        rebuild()
    }

    func menuWillOpen(_ menu: NSMenu) { menuIsOpen = true }
    func menuDidClose(_ menu: NSMenu) { menuIsOpen = false }

    // MARK: Building

    private func refreshButton() {
        guard let button = statusItem.button else { return }
        let running = model.state.info != nil
        button.appearsDisabled = !running
        button.toolTip = "\(model.appName) — \(statusText)"
    }

    private func rebuild() {
        menu.removeAllItems()
        addStatus(to: menu, detailed: true)
        menu.addItem(.separator())
        addOpenItems(to: menu, shortcuts: true)
        menu.addItem(.separator())
        addServiceItems(to: menu, shortcuts: true)
        menu.addItem(.separator())

        let login = action(
            model.launchAtLogin == .requiresApproval ? L10n.tr("menu.launchAtLoginNeedsApproval") : L10n.tr("menu.launchAtLogin"),
            .toggleLaunchAtLogin
        )
        login.state = model.launchAtLogin == .disabled ? .off : (model.launchAtLogin == .enabled ? .on : .mixed)
        menu.addItem(login)
        if model.launchAtLogin == .requiresApproval {
            menu.addItem(action(L10n.tr("menu.openLoginItemsSettings"), .openLoginItems))
        }
        menu.addItem(action(L10n.tr(model.showsDockIcon ? "menu.hideDock" : "menu.showDock"), .toggleDock))
        menu.addItem(.separator())

        menu.addItem(action(L10n.tr("menu.openLogs"), .openLogs))
        menu.addItem(action(L10n.tr("menu.openDshHome"), .openDshHome))
        menu.addItem(action(L10n.tr("menu.editConfig"), .editConfig))
        if model.canRepair {
            menu.addItem(action(L10n.tr("menu.repair"), .repair, enabled: !isBusy))
        }
        menu.addItem(.separator())
        menu.addItem(action(L10n.tr("menu.about", model.appName), .about))
        menu.addItem(action(L10n.tr("menu.quit", model.appName), .quit, key: "q"))
    }

    /// Right-click menu of the Dock icon: status plus the service actions. macOS
    /// appends its own Options, Show All Windows, Hide and Quit items below.
    func makeDockMenu() -> NSMenu {
        let dock = NSMenu()
        dock.autoenablesItems = false
        addStatus(to: dock, detailed: false)
        dock.addItem(.separator())
        addOpenItems(to: dock, shortcuts: false)
        dock.addItem(.separator())
        addServiceItems(to: dock, shortcuts: false)
        return dock
    }

    private var isBusy: Bool { model.installing || model.state == .stopping }

    private func addStatus(to menu: NSMenu, detailed: Bool) {
        menu.addItem(disabled(statusText))
        guard detailed else { return }
        if case .failed(let message) = model.state {
            let item = disabled(L10n.tr("menu.error", Self.truncate(message, 72)))
            item.toolTip = message
            menu.addItem(item)
        }
        if let summary = model.runtimeSummary, !summary.isEmpty {
            menu.addItem(disabled(L10n.tr("menu.runtime", summary)))
        }
    }

    private func addOpenItems(to menu: NSMenu, shortcuts: Bool) {
        menu.addItem(action(L10n.tr("menu.open"), .open, key: shortcuts ? "o" : "", enabled: !model.installing))
        menu.addItem(action(L10n.tr("menu.copyURL"), .copyURL, enabled: model.state.info != nil))
    }

    private func addServiceItems(to menu: NSMenu, shortcuts: Bool) {
        menu.addItem(action(L10n.tr("menu.restart"), .restart, key: shortcuts ? "r" : "", enabled: !isBusy && model.state != .idle))
        switch model.state {
        case .running, .starting:
            menu.addItem(action(L10n.tr("menu.stop"), .stop, enabled: !isBusy))
        case .idle, .stopped, .failed, .stopping:
            menu.addItem(action(L10n.tr("menu.start"), .start, enabled: !isBusy))
        }
    }

    private func disabled(_ title: String) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.isEnabled = false
        return item
    }

    private func action(_ title: String, _ command: MenuCommand, key: String = "", enabled: Bool = true) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: #selector(performCommand(_:)), keyEquivalent: key)
        item.target = self
        item.representedObject = command.rawValue
        item.isEnabled = enabled
        return item
    }

    @objc private func performCommand(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String, let command = MenuCommand(rawValue: raw) else { return }
        delegate?.perform(command)
    }

    static func truncate(_ text: String, _ limit: Int) -> String {
        let single = text.replacingOccurrences(of: "\n", with: " ")
        return single.count <= limit ? single : String(single.prefix(limit - 1)) + "…"
    }
}
