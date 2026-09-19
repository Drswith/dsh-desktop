import Foundation

/// GUI apps start with launchd's minimal environment. dsh runs the user's tools
/// (git, package managers, language runtimes), so the daemon receives the
/// environment of the user's login shell instead, resolved like VS Code does.
public enum ShellEnvironment {
    static let beginMarker = "__DSH_LAUNCHER_ENV_BEGIN__"
    static let endMarker = "__DSH_LAUNCHER_ENV_END__"

    /// Launch-time variables that describe this app rather than the user's session.
    static let droppedKeys: Set<String> = [
        "__CFBundleIdentifier", "XPC_SERVICE_NAME", "XPC_FLAGS", "OLDPWD", "PWD", "SHLVL", "_",
        "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "TERM_SESSION_ID", "ITERM_SESSION_ID",
    ]

    public static let fallbackPath = [
        "/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin", "/usr/local/sbin",
        "/usr/bin", "/bin", "/usr/sbin", "/sbin",
    ].joined(separator: ":")

    /// Run `$SHELL -l -i -c 'env -0'` and parse the environment between markers.
    public static func resolveLoginEnvironment(shell: String?, timeout: TimeInterval = 10) -> Result<[String: String], Error> {
        let shellPath = shell.flatMap { $0.isEmpty ? nil : $0 } ?? "/bin/zsh"
        let script = "printf '%s' '\(beginMarker)'; /usr/bin/env -0; printf '%s' '\(endMarker)'"
        var base = ProcessInfo.processInfo.environment
        base["DSH_LAUNCHER_RESOLVING_SHELL_ENV"] = "1"
        do {
            let result = try CommandRunner.run(shellPath, ["-l", "-i", "-c", script], environment: base, timeout: timeout)
            if result.timedOut { return .failure(CommandError("login shell timed out after \(Int(timeout))s")) }
            guard let parsed = parse(result.stdout) else {
                return .failure(CommandError("login shell printed no environment (status \(result.status))"))
            }
            return .success(parsed)
        } catch {
            return .failure(error)
        }
    }

    /// Extract `KEY=VALUE` records (NUL separated) between the markers.
    public static func parse(_ data: Data) -> [String: String]? {
        let begin = Data(beginMarker.utf8)
        let end = Data(endMarker.utf8)
        guard let beginRange = data.range(of: begin),
              let endRange = data.range(of: end, options: [], in: beginRange.upperBound..<data.endIndex) else { return nil }
        let body = data.subdata(in: beginRange.upperBound..<endRange.lowerBound)
        var environment: [String: String] = [:]
        for record in body.split(separator: 0) {
            let text = String(decoding: record, as: UTF8.self)
            guard let equals = text.firstIndex(of: "="), equals != text.startIndex else { continue }
            environment[String(text[..<equals])] = String(text[text.index(after: equals)...])
        }
        return environment.isEmpty ? nil : environment
    }

    /// Compose the daemon environment: login shell (or current) values, minus
    /// launch artifacts, with a usable PATH, then explicit overrides.
    public static func daemonEnvironment(
        login: [String: String]?,
        current: [String: String] = ProcessInfo.processInfo.environment,
        overrides: [String: String] = [:]
    ) -> [String: String] {
        var environment = login ?? current
        for key in droppedKeys { environment.removeValue(forKey: key) }
        environment.removeValue(forKey: "DSH_LAUNCHER_RESOLVING_SHELL_ENV")
        if login == nil {
            // Without a login shell, the common tool directories come first, as a login PATH would put them.
            environment["PATH"] = mergePath(fallbackPath, extra: environment["PATH"] ?? "")
        } else if (environment["PATH"] ?? "").isEmpty {
            environment["PATH"] = fallbackPath
        }
        if environment["HOME"] == nil { environment["HOME"] = NSHomeDirectory() }
        if environment["LANG"] == nil { environment["LANG"] = "en_US.UTF-8" }
        for (key, value) in overrides { environment[key] = value }
        return environment
    }

    static func mergePath(_ path: String?, extra: String) -> String {
        var seen = Set<String>()
        var result: [String] = []
        for entry in ((path ?? "") + ":" + extra).split(separator: ":").map(String.init) where !entry.isEmpty {
            if seen.insert(entry).inserted { result.append(entry) }
        }
        return result.joined(separator: ":")
    }
}
