import Foundation

public struct CommandResult: Sendable {
    public let status: Int32
    public let stdout: Data
    public let stderr: Data
    public let timedOut: Bool

    public var succeeded: Bool { !timedOut && status == 0 }
    public var stdoutText: String { String(decoding: stdout, as: UTF8.self) }
    public var stderrText: String { String(decoding: stderr, as: UTF8.self) }
}

public struct CommandError: LocalizedError, Sendable {
    public let message: String

    public init(_ message: String) {
        self.message = message
    }

    public var errorDescription: String? { message }
}

/// Short-lived helper commands (tar, xattr, login shell, node --version).
public enum CommandRunner {
    public static func run(
        _ executable: String,
        _ arguments: [String],
        environment: [String: String]? = nil,
        currentDirectory: URL? = nil,
        timeout: TimeInterval = 120
    ) throws -> CommandResult {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        if let environment { process.environment = environment }
        if let currentDirectory { process.currentDirectoryURL = currentDirectory }
        process.standardInput = FileHandle.nullDevice
        let outPipe = Pipe()
        let errPipe = Pipe()
        process.standardOutput = outPipe
        process.standardError = errPipe

        let exited = DispatchSemaphore(value: 0)
        process.terminationHandler = { _ in exited.signal() }
        try process.run()

        // Drain both pipes concurrently so a chatty child never blocks on a full pipe.
        let group = DispatchGroup()
        var outData = Data()
        var errData = Data()
        DispatchQueue.global().async(group: group) { outData = outPipe.fileHandleForReading.readDataToEndOfFile() }
        DispatchQueue.global().async(group: group) { errData = errPipe.fileHandleForReading.readDataToEndOfFile() }

        var timedOut = false
        if exited.wait(timeout: .now() + timeout) == .timedOut {
            timedOut = true
            process.terminate()
            if exited.wait(timeout: .now() + 3) == .timedOut {
                kill(process.processIdentifier, SIGKILL)
                exited.wait()
            }
        }
        // A grandchild holding the pipe open must not hang the caller forever.
        _ = group.wait(timeout: .now() + 5)
        return CommandResult(status: process.terminationStatus, stdout: outData, stderr: errData, timedOut: timedOut)
    }

    /// Run and throw with the command's stderr on failure.
    @discardableResult
    public static func check(
        _ executable: String,
        _ arguments: [String],
        environment: [String: String]? = nil,
        currentDirectory: URL? = nil,
        timeout: TimeInterval = 120
    ) throws -> CommandResult {
        let result = try run(executable, arguments, environment: environment, currentDirectory: currentDirectory, timeout: timeout)
        guard result.succeeded else {
            let name = URL(fileURLWithPath: executable).lastPathComponent
            let detail = result.stderrText.trimmingCharacters(in: .whitespacesAndNewlines)
            let reason = result.timedOut ? "timed out after \(Int(timeout))s" : "exited with status \(result.status)"
            throw CommandError("\(name) \(reason)\(detail.isEmpty ? "" : ": \(detail.suffix(600))")")
        }
        return result
    }
}
