import Foundation

/// A long-lived child started with posix_spawn in its own process group.
public final class SpawnedProcess: @unchecked Sendable {
    public let pid: pid_t
    public let stdout: FileHandle
    public let stderr: FileHandle

    init(pid: pid_t, stdout: FileHandle, stderr: FileHandle) {
        self.pid = pid
        self.stdout = stdout
        self.stderr = stderr
    }

    /// Signal the child itself.
    @discardableResult
    public func signal(_ signal: Int32) -> Bool {
        kill(pid, signal) == 0
    }

    /// Signal every process still in the child's group (the group id is the pid).
    @discardableResult
    public func signalGroup(_ signal: Int32) -> Bool {
        kill(-pid, signal) == 0
    }
}

/// posix_spawn gives what Foundation.Process does not: a dedicated process group
/// (so stragglers can be reaped with one signal), default signal dispositions,
/// and no inherited descriptors beyond stdio.
public enum ProcessSpawner {
    public static func spawn(
        executable: String,
        arguments: [String],
        environment: [String: String],
        workingDirectory: String
    ) throws -> SpawnedProcess {
        var outPipe: [Int32] = [-1, -1]
        var errPipe: [Int32] = [-1, -1]
        guard pipe(&outPipe) == 0 else { throw posixError("pipe") }
        guard pipe(&errPipe) == 0 else {
            close(outPipe[0]); close(outPipe[1])
            throw posixError("pipe")
        }
        for fd in [outPipe[0], errPipe[0]] { _ = fcntl(fd, F_SETFD, FD_CLOEXEC) }

        var actions: posix_spawn_file_actions_t?
        posix_spawn_file_actions_init(&actions)
        defer { posix_spawn_file_actions_destroy(&actions) }
        posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0)
        posix_spawn_file_actions_adddup2(&actions, outPipe[1], 1)
        posix_spawn_file_actions_adddup2(&actions, errPipe[1], 2)
        posix_spawn_file_actions_addchdir_np(&actions, workingDirectory)

        var attributes: posix_spawnattr_t?
        posix_spawnattr_init(&attributes)
        defer { posix_spawnattr_destroy(&attributes) }
        var defaultSignals = sigset_t()
        sigfillset(&defaultSignals)
        var emptyMask = sigset_t()
        sigemptyset(&emptyMask)
        posix_spawnattr_setsigdefault(&attributes, &defaultSignals)
        posix_spawnattr_setsigmask(&attributes, &emptyMask)
        posix_spawnattr_setpgroup(&attributes, 0)
        let flags = POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_SETSIGMASK | POSIX_SPAWN_CLOEXEC_DEFAULT
        posix_spawnattr_setflags(&attributes, Int16(flags))

        let argv = ([executable] + arguments).map { strdup($0) } + [nil]
        let envp = environment.map { strdup("\($0.key)=\($0.value)") } + [nil]
        defer {
            argv.forEach { free($0) }
            envp.forEach { free($0) }
        }

        var pid: pid_t = 0
        let status = posix_spawn(&pid, executable, &actions, &attributes, argv, envp)
        close(outPipe[1])
        close(errPipe[1])
        guard status == 0 else {
            close(outPipe[0]); close(errPipe[0])
            throw CommandError("posix_spawn \(executable) failed: \(String(cString: strerror(status)))")
        }
        return SpawnedProcess(
            pid: pid,
            stdout: FileHandle(fileDescriptor: outPipe[0], closeOnDealloc: true),
            stderr: FileHandle(fileDescriptor: errPipe[0], closeOnDealloc: true)
        )
    }

    /// Reap a finished child; returns the exit code (or 128 + signal).
    public static func reap(_ pid: pid_t) -> Int32? {
        var status: Int32 = 0
        let result = waitpid(pid, &status, WNOHANG)
        guard result == pid else { return nil }
        let signal = status & 0x7f
        if signal == 0 { return (status >> 8) & 0xff }
        return 128 + signal
    }

    /// Whether a process with this pid exists (and is visible to us).
    public static func isAlive(_ pid: pid_t) -> Bool {
        pid > 0 && (kill(pid, 0) == 0 || errno == EPERM)
    }

    /// The full command line of a process, used to recognize an orphaned daemon.
    public static func commandLine(of pid: pid_t) -> String? {
        guard let result = try? CommandRunner.run("/bin/ps", ["-ww", "-o", "command=", "-p", String(pid)], timeout: 5),
              result.succeeded else { return nil }
        let text = result.stdoutText.trimmingCharacters(in: .whitespacesAndNewlines)
        return text.isEmpty ? nil : text
    }

    private static func posixError(_ call: String) -> CommandError {
        CommandError("\(call) failed: \(String(cString: strerror(errno)))")
    }
}
