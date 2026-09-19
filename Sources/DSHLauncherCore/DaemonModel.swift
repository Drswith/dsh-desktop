import Foundation

/// Everything needed to boot `dsh --profile <name> … --no-open` once.
public struct DaemonLaunchPlan: Equatable, Sendable {
    public var node: URL
    public var entry: URL
    public var dshVersion: String?
    public var nodeVersion: String?
    public var profile: String
    public var dshHome: URL
    public var preferredPort: Int
    public var extraArgs: [String]
    public var environment: [String: String]
    public var workingDirectory: URL

    public init(node: URL, entry: URL, dshVersion: String?, nodeVersion: String?, profile: String, dshHome: URL,
                preferredPort: Int, extraArgs: [String], environment: [String: String], workingDirectory: URL) {
        self.node = node
        self.entry = entry
        self.dshVersion = dshVersion
        self.nodeVersion = nodeVersion
        self.profile = profile
        self.dshHome = dshHome
        self.preferredPort = preferredPort
        self.extraArgs = extraArgs
        self.environment = environment
        self.workingDirectory = workingDirectory
    }

    /// Profiles the CLI ships; they initialize themselves and reject `--from-default-profile`.
    public static let shippedProfiles: Set<String> = ["acp", "web", "headless", "sdk", "sdk-minimal"]

    /// Resolve `$DSH_HOME` the way `@deepseek-ai/dsh-home-paths` does.
    public static func resolveDshHome(configured: String?, environment: [String: String], home: URL) -> URL {
        let raw = configured.flatMap { $0.trimmingCharacters(in: .whitespaces).isEmpty ? nil : $0 }
            ?? environment["DSH_HOME"].flatMap { $0.trimmingCharacters(in: .whitespaces).isEmpty ? nil : $0 }
        guard let raw else { return home.appendingPathComponent(".dsh", isDirectory: true) }
        if raw == "~" { return home }
        if raw.hasPrefix("~/") { return home.appendingPathComponent(String(raw.dropFirst(2)), isDirectory: true) }
        return URL(fileURLWithPath: raw, isDirectory: true).standardizedFileURL
    }

    public var profileDirectory: URL {
        dshHome.appendingPathComponent("profiles", isDirectory: true).appendingPathComponent(profile, isDirectory: true)
    }

    /// Custom profiles are created from the shipped `web` template exactly once.
    public func needsProfileInitialization(fileManager: FileManager = .default) -> Bool {
        guard !Self.shippedProfiles.contains(profile) else { return false }
        return !fileManager.fileExists(atPath: profileDirectory.appendingPathComponent("package.json").path)
    }

    /// Launcher flags first, then the Web app's own flags, then user extras.
    public func arguments(port: Int, initializeProfile: Bool) -> [String] {
        var args = [entry.path, "--profile", profile]
        if initializeProfile { args += ["--from-default-profile", "web"] }
        args += ["--no-open", "--host", "127.0.0.1", "--port", String(port)]
        return args + extraArgs
    }

    public var runtimeSummary: String {
        [dshVersion.map { "DSH \($0)" }, nodeVersion.map { "Node.js \($0)" }].compactMap { $0 }.joined(separator: " · ")
    }
}

public struct DaemonInfo: Equatable, Sendable {
    public let pid: Int32
    public let port: Int
    /// Loopback root URL carrying this process's launch token.
    public let authenticatedURL: URL
    public let startedAt: Date

    public init(pid: Int32, port: Int, authenticatedURL: URL, startedAt: Date) {
        self.pid = pid
        self.port = port
        self.authenticatedURL = authenticatedURL
        self.startedAt = startedAt
    }

    public var cleanURL: URL { ReadyLine.cleanURL(authenticatedURL) }
}

public enum DaemonState: Equatable, Sendable {
    case idle
    case starting(attempt: Int)
    case running(DaemonInfo)
    case stopping
    case stopped
    case failed(String)

    public var info: DaemonInfo? {
        if case let .running(info) = self { return info }
        return nil
    }

    public var isActive: Bool {
        switch self {
        case .starting, .running, .stopping: return true
        case .idle, .stopped, .failed: return false
        }
    }
}

public struct SupervisorPolicy: Sendable {
    public var startupTimeout: TimeInterval = 180
    public var stopGrace: TimeInterval = 8
    public var healthInterval: TimeInterval = 15
    public var healthTimeout: TimeInterval = 5
    public var maxHealthFailures = 3
    public var wakeGrace: TimeInterval = 60
    public var restartBackoff: [TimeInterval] = [1, 2, 5, 10, 30]
    public var crashWindow: TimeInterval = 600
    public var maxCrashesInWindow = 5
    public var portAttempts = 20

    public init() {}
}

/// `run/daemon.json`: lets the next launch recognize and stop an orphaned daemon.
struct DaemonRecord: Codable {
    var pid: Int32
    var port: Int
    var entry: String
    var shellPid: Int32
    var startedAt: String
}
