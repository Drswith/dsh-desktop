import Foundation

/// Shell-owned state directory (`~/.dsh-launcher` by default). dsh product data
/// (sessions, settings, credentials, profiles) stays in `$DSH_HOME`; this tree
/// holds only the executable runtime, logs, and supervisor state.
public struct AppPaths: Sendable {
    public let home: URL

    public init(home: URL) {
        self.home = home
    }

    public static func standard(dirName: String) -> AppPaths {
        AppPaths(home: FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(dirName, isDirectory: true))
    }

    public var runtimeRoot: URL { home.appendingPathComponent("runtime", isDirectory: true) }
    /// Symlink naming the active runtime directory (relative target).
    public var currentRuntimeLink: URL { runtimeRoot.appendingPathComponent("current") }
    public var logsDir: URL { home.appendingPathComponent("logs", isDirectory: true) }
    public var runDir: URL { home.appendingPathComponent("run", isDirectory: true) }
    public var shellLog: URL { logsDir.appendingPathComponent("launcher.log") }
    public var daemonLog: URL { logsDir.appendingPathComponent("dsh.log") }
    public var daemonStateFile: URL { runDir.appendingPathComponent("daemon.json") }
    public var lockFile: URL { runDir.appendingPathComponent("launcher.lock") }
    public var configFile: URL { home.appendingPathComponent("config.json") }

    /// Create the tree with owner-only permissions.
    public func prepare() throws {
        let fm = FileManager.default
        for dir in [home, runtimeRoot, logsDir, runDir] {
            try fm.createDirectory(at: dir, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        }
    }
}

/// Optional user overrides read from `config.json`; every field may be omitted.
public struct ShellConfig: Codable, Equatable, Sendable {
    /// Preferred loopback port; the next free port is used when it is taken.
    public var port: Int?
    /// dsh profile under `$DSH_HOME/profiles`; initialized from the shipped `web` template.
    public var profile: String?
    /// Explicit `DSH_HOME`; otherwise the login-shell value or `~/.dsh`.
    public var dshHome: String?
    /// Extra arguments appended after the Web app flags.
    public var extraArgs: [String]?
    /// Extra environment variables for the dsh process.
    public var environment: [String: String]?
    /// Development override: run an existing Node binary and dsh entry instead of the bundled runtime.
    public var runtime: ExternalRuntime?

    public struct ExternalRuntime: Codable, Equatable, Sendable {
        public var node: String
        public var entry: String

        public init(node: String, entry: String) {
            self.node = node
            self.entry = entry
        }
    }

    public init() {}

    /// Load `config.json`; a missing file yields the empty configuration.
    public static func load(from url: URL) throws -> ShellConfig {
        guard FileManager.default.fileExists(atPath: url.path) else { return ShellConfig() }
        let data = try Data(contentsOf: url)
        if data.allSatisfy({ $0 == 0x20 || $0 == 0x0A || $0 == 0x0D || $0 == 0x09 }) { return ShellConfig() }
        return try JSONDecoder().decode(ShellConfig.self, from: data)
    }
}
