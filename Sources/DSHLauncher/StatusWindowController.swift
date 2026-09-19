import AppKit

/// Small progress/failure window shown while the runtime installs or the service
/// starts after a manual launch.
final class StatusWindowController: NSWindowController {
    var onRetry: (() -> Void)?
    var onOpenLogs: (() -> Void)?

    private let titleLabel = NSTextField(labelWithString: "")
    private let detailLabel = NSTextField(wrappingLabelWithString: "")
    private let spinner = NSProgressIndicator()
    private let buttonRow = NSStackView()

    init() {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 420, height: 150),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: true
        )
        window.isReleasedWhenClosed = false
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.level = .floating
        super.init(window: window)
        buildContent()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    /// Progress state: spinner, no buttons.
    func showProgress(title: String, detail: String) {
        titleLabel.stringValue = title
        detailLabel.stringValue = detail
        spinner.isHidden = false
        spinner.startAnimation(nil)
        buttonRow.isHidden = true
        present()
    }

    /// Failure state: selectable detail plus Open Logs / Retry / Close.
    func showFailure(title: String, detail: String) {
        titleLabel.stringValue = title
        detailLabel.stringValue = detail
        spinner.stopAnimation(nil)
        spinner.isHidden = true
        buttonRow.isHidden = false
        present()
    }

    var isVisible: Bool { window?.isVisible ?? false }

    private func present() {
        guard let window else { return }
        if !window.isVisible { window.center() }
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
    }

    private func buildContent() {
        guard let content = window?.contentView else { return }
        let icon = NSImageView(image: NSApp.applicationIconImage ?? NSImage())
        icon.imageScaling = .scaleProportionallyUpOrDown
        icon.translatesAutoresizingMaskIntoConstraints = false
        icon.widthAnchor.constraint(equalToConstant: 56).isActive = true
        icon.heightAnchor.constraint(equalToConstant: 56).isActive = true

        titleLabel.font = .boldSystemFont(ofSize: 14)
        detailLabel.font = .systemFont(ofSize: 12)
        detailLabel.textColor = .secondaryLabelColor
        detailLabel.isSelectable = true
        detailLabel.preferredMaxLayoutWidth = 300
        spinner.style = .spinning
        spinner.controlSize = .small
        spinner.isIndeterminate = true

        let retry = NSButton(title: L10n.tr("action.retry"), target: self, action: #selector(retryTapped))
        retry.keyEquivalent = "\r"
        let logs = NSButton(title: L10n.tr("action.openLogs"), target: self, action: #selector(logsTapped))
        let close = NSButton(title: L10n.tr("action.close"), target: self, action: #selector(closeTapped))
        buttonRow.orientation = .horizontal
        buttonRow.spacing = 8
        buttonRow.addArrangedSubview(logs)
        buttonRow.addArrangedSubview(close)
        buttonRow.addArrangedSubview(retry)

        let titleRow = NSStackView(views: [titleLabel, spinner])
        titleRow.orientation = .horizontal
        titleRow.spacing = 8
        let textColumn = NSStackView(views: [titleRow, detailLabel, buttonRow])
        textColumn.orientation = .vertical
        textColumn.alignment = .leading
        textColumn.spacing = 8

        let root = NSStackView(views: [icon, textColumn])
        root.orientation = .horizontal
        root.alignment = .top
        root.spacing = 16
        root.edgeInsets = NSEdgeInsets(top: 28, left: 20, bottom: 20, right: 20)
        root.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(root)
        NSLayoutConstraint.activate([
            root.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            root.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            root.topAnchor.constraint(equalTo: content.topAnchor),
            root.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            content.widthAnchor.constraint(equalToConstant: 420),
        ])
    }

    @objc private func retryTapped() {
        close()
        onRetry?()
    }

    @objc private func logsTapped() {
        onOpenLogs?()
    }

    @objc private func closeTapped() {
        close()
    }
}
