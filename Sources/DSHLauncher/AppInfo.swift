import AppKit
import ServiceManagement

/// Build-time identity and defaults, injected into Info.plist (the `DSHLauncher*`
/// keys) by `scripts/build-app.sh`.
struct AppInfo {
    let bundle: Bundle

    private func string(_ key: String) -> String? {
        (bundle.object(forInfoDictionaryKey: key) as? String).flatMap { $0.isEmpty ? nil : $0 }
    }

    var displayName: String { string("CFBundleDisplayName") ?? string("CFBundleName") ?? "DSH Launcher" }
    var version: String { string("CFBundleShortVersionString") ?? "0.0.0" }
    var build: String { string("CFBundleVersion") ?? "0" }
    var bundleIdentifier: String { bundle.bundleIdentifier ?? "io.github.drswith.dsh-launcher" }
    var homeDirName: String { string("DSHLauncherHomeDirName") ?? ".dsh-launcher" }
    var profile: String { string("DSHLauncherProfile") ?? "launcher" }
    var urlScheme: String { string("DSHLauncherURLScheme") ?? "dsh-launcher" }

    /// Commit of the launcher source, `-dirty` when built from uncommitted changes.
    var gitCommit: String? { string("DSHLauncherGitCommit").flatMap { $0 == "unknown" ? nil : $0 } }
    /// ISO 8601 build time with offset, e.g. `2026-09-19T10:05:10+08:00`.
    var buildDate: String? { string("DSHLauncherBuildDate") }
    /// Source repository as an https URL, e.g. `https://github.com/Drswith/dsh-launcher`.
    var repoURL: String? { string("DSHLauncherRepoURL") }

    /// About-dialog line for the repository: `GitHub: Drswith/dsh-launcher`, or
    /// `Repo: host/path` for other hosts so a GitLab repo is never labeled GitHub.
    var repoEntry: (key: String, value: String)? {
        guard let repoURL else { return nil }
        guard let components = URLComponents(string: repoURL), let host = components.host else { return ("Repo", repoURL) }
        let path = components.path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        if host == "github.com", !path.isEmpty { return ("GitHub", path) }
        return ("Repo", path.isEmpty ? host : "\(host)/\(path)")
    }

    var defaultPort: Int {
        if let number = bundle.object(forInfoDictionaryKey: "DSHLauncherDefaultPort") as? NSNumber { return number.intValue }
        return string("DSHLauncherDefaultPort").flatMap(Int.init) ?? 31080
    }

    /// `Contents/Resources/payload`, or `DSH_LAUNCHER_PAYLOAD_DIR` for unbundled development runs.
    var payloadDirectory: URL? {
        let candidates = [
            ProcessInfo.processInfo.environment["DSH_LAUNCHER_PAYLOAD_DIR"].map { URL(fileURLWithPath: $0, isDirectory: true) },
            bundle.resourceURL?.appendingPathComponent("payload", isDirectory: true),
        ]
        return candidates.compactMap { $0 }.first {
            FileManager.default.fileExists(atPath: $0.appendingPathComponent("manifest.json").path)
        }
    }

    /// Login items registered from a build folder break once the app moves.
    var isInStableLocation: Bool {
        let path = bundle.bundleURL.resolvingSymlinksInPath().path
        let userApplications = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Applications").path
        return path.hasPrefix("/Applications/") || path.hasPrefix(userApplications + "/")
    }
}

enum L10n {
    private static let missing = "\u{0}missing"
    private static let english: Bundle? = Bundle.main.path(forResource: "en", ofType: "lproj").flatMap(Bundle.init(path:))

    /// Localized string for `key`, falling back to English, then to the key itself.
    static func tr(_ key: String, _ arguments: CVarArg...) -> String {
        var format = Bundle.main.localizedString(forKey: key, value: missing, table: nil)
        if format == missing {
            format = english?.localizedString(forKey: key, value: key, table: nil) ?? key
        }
        return arguments.isEmpty ? format : String(format: format, locale: Locale.current, arguments: arguments)
    }
}

enum LaunchAtLoginStatus: Equatable {
    case enabled
    case requiresApproval
    case disabled
}

/// Launch at login through `SMAppService.mainApp` (macOS 13+).
enum LaunchAtLogin {
    static var status: LaunchAtLoginStatus {
        switch SMAppService.mainApp.status {
        case .enabled: return .enabled
        case .requiresApproval: return .requiresApproval
        case .notRegistered, .notFound: return .disabled
        @unknown default: return .disabled
        }
    }

    static func setEnabled(_ enabled: Bool) throws {
        if enabled {
            try SMAppService.mainApp.register()
        } else {
            try SMAppService.mainApp.unregister()
        }
    }

    static func openSystemSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }
}
