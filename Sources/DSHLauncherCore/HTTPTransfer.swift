import Foundation

/// Blocking transfers for update checks, through the system proxy settings.
/// Call them off the main thread.
public enum HTTPTransfer {
    private static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 60
        configuration.timeoutIntervalForResource = 30 * 60
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.httpAdditionalHeaders = ["User-Agent": "DSH-Launcher"]
        return URLSession(configuration: configuration)
    }()

    /// The body of `url`, refused when it exceeds `limit` bytes.
    public static func data(from url: URL, limit: Int = 1 << 20) throws -> Data {
        let outcome = Outcome<Data>()
        session.dataTask(with: url) { data, response, error in
            if let error {
                outcome.finish(.failure(error))
            } else if let failure = statusFailure(response, url: url) {
                outcome.finish(.failure(failure))
            } else {
                outcome.finish(.success(data ?? Data()))
            }
        }.resume()
        let data = try outcome.wait()
        guard data.count <= limit else { throw CommandError("\(url.lastPathComponent) is larger than \(limit) bytes") }
        return data
    }

    /// Download `url` to `destination`, which only appears once the size and SHA-256 match.
    public static func download(_ url: URL, to destination: URL, expectedSize: Int, sha256: String) throws {
        let fm = FileManager.default
        let part = destination.appendingPathExtension("part")
        try? fm.removeItem(at: part)
        let outcome = Outcome<Void>()
        session.downloadTask(with: url) { location, response, error in
            if let error {
                outcome.finish(.failure(error))
            } else if let failure = statusFailure(response, url: url) {
                outcome.finish(.failure(failure))
            } else if let location {
                // The system deletes `location` when this handler returns.
                outcome.finish(Result { try fm.moveItem(at: location, to: part) })
            } else {
                outcome.finish(.failure(CommandError("downloading \(url.lastPathComponent) produced no file")))
            }
        }.resume()
        try outcome.wait()
        defer { try? fm.removeItem(at: part) }

        let size = (try fm.attributesOfItem(atPath: part.path)[.size] as? NSNumber)?.intValue ?? -1
        guard size == expectedSize else {
            throw CommandError("\(url.lastPathComponent) is \(size) bytes, expected \(expectedSize)")
        }
        let digest = try RuntimeInstaller.sha256(of: part)
        guard digest == sha256 else {
            throw CommandError("\(url.lastPathComponent) checksum mismatch: expected \(sha256), got \(digest)")
        }
        try? fm.removeItem(at: destination)
        try fm.moveItem(at: part, to: destination)
    }

    /// Non-2xx HTTP responses are failures; file URLs (tests, local feeds) have no status.
    private static func statusFailure(_ response: URLResponse?, url: URL) -> Error? {
        guard let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) else { return nil }
        return CommandError("\(url.absoluteString) answered HTTP \(http.statusCode)")
    }
}

/// A result handed from a URLSession callback to the thread waiting for it.
private final class Outcome<Value>: @unchecked Sendable {
    private let done = DispatchSemaphore(value: 0)
    private var result: Result<Value, Error>?

    func finish(_ result: Result<Value, Error>) {
        self.result = result
        done.signal()
    }

    func wait() throws -> Value {
        done.wait()
        return try result!.get()
    }
}
