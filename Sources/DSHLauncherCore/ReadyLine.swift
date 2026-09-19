import Foundation

/// The `dsh web:` stdout line is the Web profile's readiness signal: it is printed
/// only after the Loader tree settles, and carries the process launch token
/// (`/?token=…`) that mints the browser cookie.
public enum ReadyLine {
    public static let prefix = "dsh web: "

    /// Extract the authenticated loopback URL from one stdout line.
    public static func authenticatedURL(in line: String) -> URL? {
        let clean = stripANSI(line).trimmingCharacters(in: .whitespacesAndNewlines)
        guard clean.hasPrefix(prefix) else { return nil }
        let rest = clean.dropFirst(prefix.count)
        // The optional ` (LAN: …)` suffix follows the first space.
        guard let token = rest.split(separator: " ", maxSplits: 1).first,
              let url = URL(string: String(token)),
              url.scheme == "http",
              let host = url.host, host == "127.0.0.1" || host == "localhost",
              let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              components.queryItems?.contains(where: { $0.name == "token" && !($0.value ?? "").isEmpty }) == true
        else { return nil }
        return url
    }

    /// The same origin without credentials, safe to display or log.
    public static func cleanURL(_ url: URL) -> URL {
        var components = URLComponents(url: url, resolvingAgainstBaseURL: false) ?? URLComponents()
        components.query = nil
        components.fragment = nil
        return components.url ?? url
    }

    /// Replace every `token=` query value so launch tokens never reach log files.
    public static func redact(_ text: String) -> String {
        text.replacingOccurrences(
            of: #"token=[A-Za-z0-9_\-]+"#,
            with: "token=<redacted>",
            options: .regularExpression
        )
    }

    static func stripANSI(_ text: String) -> String {
        // ESC is spelled as an ICU escape so the source stays printable ASCII.
        text.replacingOccurrences(of: #"\x{1B}\[[0-9;?]*[ -/]*[@-~]"#, with: "", options: .regularExpression)
    }
}
