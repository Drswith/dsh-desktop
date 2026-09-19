import Foundation

/// Append-only log file with size-based rotation (`name.log`, `name.log.1`, …).
public final class FileLogger: @unchecked Sendable {
    public let url: URL
    private let maxBytes: UInt64
    private let keep: Int
    private let echoToStderr: Bool
    private let queue = DispatchQueue(label: "dsh-launcher.file-logger")
    private var handle: FileHandle?
    private var size: UInt64 = 0
    private let formatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd HH:mm:ss.SSS Z"
        return formatter
    }()

    public init(url: URL, maxBytes: UInt64 = 8 * 1024 * 1024, keep: Int = 3, echoToStderr: Bool = isatty(STDERR_FILENO) != 0) {
        self.url = url
        self.maxBytes = maxBytes
        self.keep = max(1, keep)
        self.echoToStderr = echoToStderr
    }

    deinit {
        try? handle?.close()
    }

    /// Append one timestamped line; secrets must already be redacted by the caller.
    public func log(_ message: String) {
        let stamp = formatter.string(from: Date())
        append("[\(stamp)] \(message)\n")
    }

    /// Block until queued writes reach the file.
    public func flush() {
        queue.sync { try? handle?.synchronize() }
    }

    private func append(_ line: String) {
        let data = Data(line.utf8)
        queue.async { [self] in
            if echoToStderr { FileHandle.standardError.write(data) }
            if size + UInt64(data.count) > maxBytes { rotate() }
            guard let handle = openIfNeeded() else { return }
            do {
                try handle.write(contentsOf: data)
                size += UInt64(data.count)
            } catch {
                try? handle.close()
                self.handle = nil
            }
        }
    }

    private func openIfNeeded() -> FileHandle? {
        if let handle { return handle }
        let fm = FileManager.default
        try? fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        if !fm.fileExists(atPath: url.path) {
            fm.createFile(atPath: url.path, contents: nil, attributes: [.posixPermissions: 0o600])
        }
        guard let opened = try? FileHandle(forWritingTo: url) else { return nil }
        size = (try? opened.seekToEnd()) ?? 0
        handle = opened
        return opened
    }

    private func rotate() {
        try? handle?.close()
        handle = nil
        size = 0
        let fm = FileManager.default
        let oldest = url.appendingPathExtension("\(keep)")
        try? fm.removeItem(at: oldest)
        if keep > 1 {
            for index in stride(from: keep - 1, through: 1, by: -1) {
                let from = url.appendingPathExtension("\(index)")
                if fm.fileExists(atPath: from.path) {
                    try? fm.moveItem(at: from, to: url.appendingPathExtension("\(index + 1)"))
                }
            }
        }
        try? fm.moveItem(at: url, to: url.appendingPathExtension("1"))
    }
}
