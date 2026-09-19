import Foundation

/// Owns one `dsh` Web daemon: spawn, readiness via the `dsh web:` line, health
/// watchdog, crash restarts with backoff, and graceful stop. All mutable state
/// lives on `queue`; `onStateChange` is delivered on the main queue.
public final class DaemonSupervisor: @unchecked Sendable {
    public var onStateChange: ((DaemonState) -> Void)?

    private let paths: AppPaths
    private let log: FileLogger
    private let output: FileLogger
    private let policy: SupervisorPolicy
    private let queue = DispatchQueue(label: "dsh-launcher.supervisor")

    private var state: DaemonState = .idle
    private var plan: DaemonLaunchPlan?
    private var process: SpawnedProcess?
    private var exitSource: DispatchSourceProcess?
    private var generation = 0
    private var desired = false
    private var restartAfterExit = false
    private var stopCompletions: [() -> Void] = []
    private var startupTimer: DispatchSourceTimer?
    private var killTimer: DispatchSourceTimer?
    private var healthTimer: DispatchSourceTimer?
    private var restartWork: DispatchWorkItem?
    private var healthCheckInFlight = false
    private var healthFailures = 0
    private var healthGraceUntil = Date.distantPast
    private var crashTimes: [Date] = []
    private var attempt = 0
    private var currentPort: Int?
    private var avoidPort: Int?
    private var recentErrors: [String] = []
    private var spawnedAt = Date()

    public init(paths: AppPaths, log: FileLogger, output: FileLogger, policy: SupervisorPolicy = SupervisorPolicy()) {
        self.paths = paths
        self.log = log
        self.output = output
        self.policy = policy
    }

    public var currentState: DaemonState { queue.sync { state } }

    public func recentErrorLines() -> [String] { queue.sync { recentErrors } }

    // MARK: Commands

    /// Start (or keep) the daemon with `plan`; clears a previous failure.
    public func start(_ plan: DaemonLaunchPlan) {
        queue.async { [self] in
            self.plan = plan
            desired = true
            crashTimes.removeAll()
            attempt = 0
            guard process == nil else { return }
            cancelRestart()
            spawn()
        }
    }

    /// Stop without restarting; `completion` runs once the process has exited.
    public func stop(completion: (() -> Void)? = nil) {
        queue.async { [self] in
            desired = false
            restartAfterExit = false
            cancelRestart()
            if let completion { stopCompletions.append(completion) }
            guard process != nil else {
                if state != .idle { transition(.stopped) }
                flushStopCompletions()
                return
            }
            requestTermination(reason: "stop requested")
        }
    }

    /// Stop the running daemon (if any) and start it again, optionally with a new plan.
    public func restart(_ newPlan: DaemonLaunchPlan? = nil) {
        queue.async { [self] in
            if let newPlan { plan = newPlan }
            guard plan != nil else { return }
            desired = true
            crashTimes.removeAll()
            attempt = 0
            cancelRestart()
            guard process != nil else { spawn(); return }
            restartAfterExit = true
            requestTermination(reason: "restart requested")
        }
    }

    public func systemWillSleep() {
        queue.async { [self] in
            healthFailures = 0
            healthGraceUntil = .distantFuture
        }
    }

    public func systemDidWake() {
        queue.async { [self] in
            healthFailures = 0
            healthGraceUntil = Date().addingTimeInterval(policy.wakeGrace)
            log.log("health watchdog: wake grace window \(Int(policy.wakeGrace))s")
        }
    }

    /// Stop a daemon left behind by a crashed shell. Blocking; call before `start`.
    public func terminateOrphan() {
        guard let data = try? Data(contentsOf: paths.daemonStateFile),
              let record = try? JSONDecoder().decode(DaemonRecord.self, from: data) else { return }
        defer { try? FileManager.default.removeItem(at: paths.daemonStateFile) }
        guard record.pid > 0, record.shellPid != getpid(), ProcessSpawner.isAlive(record.pid) else { return }
        guard let command = ProcessSpawner.commandLine(of: record.pid), command.contains(record.entry) else {
            log.log("orphan check: pid \(record.pid) is no longer a dsh daemon; leaving it alone")
            return
        }
        log.log("orphan daemon pid=\(record.pid) port=\(record.port) from shell pid=\(record.shellPid); stopping it")
        kill(record.pid, SIGTERM)
        let deadline = Date().addingTimeInterval(policy.stopGrace)
        while ProcessSpawner.isAlive(record.pid), Date() < deadline { usleep(100_000) }
        if ProcessSpawner.isAlive(record.pid) {
            kill(-record.pid, SIGKILL)
            kill(record.pid, SIGKILL)
        }
    }

    // MARK: Spawning

    private func spawn() {
        guard let plan else { return }
        attempt += 1
        generation += 1
        let gen = generation
        healthFailures = 0
        recentErrors.removeAll()
        transition(.starting(attempt: attempt))

        let startPort = avoidPort.map { max(plan.preferredPort, $0 + 1) } ?? plan.preferredPort
        guard let port = PortProbe.firstAvailable(startingAt: startPort, attempts: policy.portAttempts) else {
            fail("no free loopback port in \(startPort)..<\(startPort + policy.portAttempts)")
            return
        }
        let initialize = plan.needsProfileInitialization()
        if initialize, let entries = try? FileManager.default.contentsOfDirectory(atPath: plan.profileDirectory.path) {
            // The CLI refuses to initialize over an existing directory; an empty one is an interrupted init.
            guard entries.isEmpty else {
                fail("profile directory exists without package.json: \(plan.profileDirectory.path)")
                return
            }
            try? FileManager.default.removeItem(at: plan.profileDirectory)
        }
        log.log("daemon start attempt=\(attempt) port=\(port) profile=\(plan.profile) initialize=\(initialize) \(plan.runtimeSummary)")
        do {
            let child = try ProcessSpawner.spawn(
                executable: plan.node.path,
                arguments: plan.arguments(port: port, initializeProfile: initialize),
                environment: plan.environment,
                workingDirectory: plan.workingDirectory.path
            )
            process = child
            currentPort = port
            spawnedAt = Date()
            writeRecord(pid: child.pid, port: port, entry: plan.entry.path)
            attachReader(child.stdout, isStdout: true, generation: gen)
            attachReader(child.stderr, isStdout: false, generation: gen)
            watchExit(child, generation: gen)
            armStartupTimer(generation: gen)
        } catch {
            fail("could not start dsh: \(error.localizedDescription)")
        }
    }

    private func attachReader(_ handle: FileHandle, isStdout: Bool, generation gen: Int) {
        let buffer = LineBuffer()
        handle.readabilityHandler = { [weak self] _ in
            // Capturing `handle` strongly keeps it (and its descriptor) alive until EOF,
            // where clearing the handler breaks the cycle.
            let data = handle.availableData
            guard let self else {
                handle.readabilityHandler = nil
                return
            }
            if data.isEmpty {
                handle.readabilityHandler = nil
                self.queue.async {
                    if let rest = buffer.flush() { self.handleLine(rest, isStdout: isStdout, generation: gen) }
                }
                return
            }
            self.queue.async {
                for line in buffer.append(data) { self.handleLine(line, isStdout: isStdout, generation: gen) }
            }
        }
    }

    private func handleLine(_ raw: String, isStdout: Bool, generation gen: Int) {
        let line = ReadyLine.redact(raw)
        output.log("\(isStdout ? "out" : "err") | \(line)")
        guard gen == generation else { return }
        if !isStdout {
            recentErrors.append(line)
            if recentErrors.count > 30 { recentErrors.removeFirst(recentErrors.count - 30) }
        }
        guard isStdout, case .starting = state, let process, let url = ReadyLine.authenticatedURL(in: raw) else { return }
        startupTimer?.cancel()
        startupTimer = nil
        avoidPort = nil
        let info = DaemonInfo(pid: process.pid, port: url.port ?? currentPort ?? 0, authenticatedURL: url, startedAt: spawnedAt)
        log.log(String(format: "daemon ready pid=%d url=%@ after %.1fs", process.pid, info.cleanURL.absoluteString, Date().timeIntervalSince(spawnedAt)))
        transition(.running(info))
        armHealthTimer(generation: gen)
    }

    private func watchExit(_ child: SpawnedProcess, generation gen: Int) {
        let source = DispatchSource.makeProcessSource(identifier: child.pid, eventMask: .exit, queue: queue)
        source.setEventHandler { [weak self, weak source] in
            source?.cancel()
            let code = ProcessSpawner.reap(child.pid)
            self?.handleExit(child: child, code: code, generation: gen)
        }
        exitSource = source
        source.resume()
    }

    private func handleExit(child: SpawnedProcess, code: Int32?, generation gen: Int) {
        guard gen == generation, process === child else { return }
        process = nil
        exitSource = nil
        cancelTimers()
        removeRecord()
        // Anything the daemon left in its process group goes with it.
        if kill(-child.pid, SIGTERM) == 0 {
            queue.asyncAfter(deadline: .now() + 3) {
                if kill(-child.pid, 0) == 0 { kill(-child.pid, SIGKILL) }
            }
        }
        let uptime = Int(Date().timeIntervalSince(spawnedAt))
        log.log("daemon exited pid=\(child.pid) code=\(code.map { String($0) } ?? "?") uptime=\(uptime)s")

        if restartAfterExit {
            restartAfterExit = false
            spawn()
            return
        }
        guard desired else {
            transition(.stopped)
            flushStopCompletions()
            return
        }
        if recentErrors.contains(where: { $0.contains("EADDRINUSE") }) { avoidPort = currentPort }
        let now = Date()
        crashTimes = crashTimes.filter { now.timeIntervalSince($0) < policy.crashWindow } + [now]
        if crashTimes.count >= policy.maxCrashesInWindow {
            fail(failureSummary(code: code))
            return
        }
        let delay = policy.restartBackoff[min(crashTimes.count - 1, policy.restartBackoff.count - 1)]
        log.log("daemon restart in \(delay)s (unexpected exit \(crashTimes.count)/\(policy.maxCrashesInWindow))")
        transition(.starting(attempt: attempt + 1))
        let work = DispatchWorkItem { [weak self] in
            guard let self, self.desired, self.process == nil else { return }
            self.spawn()
        }
        restartWork = work
        queue.asyncAfter(deadline: .now() + delay, execute: work)
    }

    private func requestTermination(reason: String) {
        guard let process else { return }
        cancelTimers()
        transition(.stopping)
        log.log("daemon stop pid=\(process.pid): \(reason)")
        process.signal(SIGTERM)
        let gen = generation
        killTimer = oneShot(after: policy.stopGrace) { [weak self] in
            guard let self, gen == self.generation, let process = self.process else { return }
            self.log.log("daemon still running after \(Int(self.policy.stopGrace))s; killing its process group")
            process.signalGroup(SIGKILL)
            process.signal(SIGKILL)
        }
    }

    // MARK: Watchdogs

    private func armStartupTimer(generation gen: Int) {
        startupTimer = oneShot(after: policy.startupTimeout) { [weak self] in
            guard let self, gen == self.generation, case .starting = self.state, let process = self.process else { return }
            self.log.log("daemon not ready after \(Int(self.policy.startupTimeout))s; terminating pid=\(process.pid)")
            self.recentErrors.append("startup timed out after \(Int(self.policy.startupTimeout))s")
            process.signal(SIGTERM)
            self.killTimer = self.oneShot(after: self.policy.stopGrace) {
                guard gen == self.generation, let process = self.process else { return }
                process.signalGroup(SIGKILL)
                process.signal(SIGKILL)
            }
        }
    }

    private func armHealthTimer(generation gen: Int) {
        healthTimer?.cancel()
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + policy.healthInterval, repeating: policy.healthInterval, leeway: .seconds(1))
        timer.setEventHandler { [weak self] in self?.runHealthCheck(generation: gen) }
        timer.resume()
        healthTimer = timer
    }

    private func runHealthCheck(generation gen: Int) {
        guard gen == generation, !healthCheckInFlight, case let .running(info) = state else { return }
        healthCheckInFlight = true
        let timeout = policy.healthTimeout
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let result = HealthProbe.check(port: info.port, timeout: timeout)
            self?.queue.async { self?.evaluateHealth(result, generation: gen) }
        }
    }

    private func evaluateHealth(_ result: HealthResult, generation gen: Int) {
        healthCheckInFlight = false
        guard gen == generation, case .running = state else { return }
        if case .unhealthy(let reason) = result {
            if Date() < healthGraceUntil {
                log.log("health check failed during grace window; not counted: \(reason)")
                return
            }
            healthFailures += 1
            log.log("health check failed (\(healthFailures)/\(policy.maxHealthFailures)): \(reason)")
            guard healthFailures >= policy.maxHealthFailures else { return }
            healthFailures = 0
            restartAfterExit = true
            requestTermination(reason: "unresponsive to \(policy.maxHealthFailures) health checks")
        } else if healthFailures > 0 {
            log.log("health check recovered")
            healthFailures = 0
        }
    }

    // MARK: Helpers

    private func fail(_ message: String) {
        log.log("daemon failed: \(message)")
        desired = false
        transition(.failed(message))
        flushStopCompletions()
    }

    private func failureSummary(code: Int32?) -> String {
        let last = recentErrors.last(where: { !$0.trimmingCharacters(in: .whitespaces).isEmpty })
        let base = "dsh exited \(policy.maxCrashesInWindow) times in \(Int(policy.crashWindow / 60)) min (last code \(code.map { String($0) } ?? "?"))"
        return last.map { "\(base): \($0)" } ?? base
    }

    private func transition(_ next: DaemonState) {
        guard next != state else { return }
        state = next
        let callback = onStateChange
        DispatchQueue.main.async { callback?(next) }
    }

    private func flushStopCompletions() {
        let completions = stopCompletions
        stopCompletions.removeAll()
        for completion in completions { DispatchQueue.main.async(execute: completion) }
    }

    private func cancelRestart() {
        restartWork?.cancel()
        restartWork = nil
    }

    private func cancelTimers() {
        startupTimer?.cancel()
        startupTimer = nil
        killTimer?.cancel()
        killTimer = nil
        healthTimer?.cancel()
        healthTimer = nil
        healthCheckInFlight = false
    }

    private func oneShot(after interval: TimeInterval, _ handler: @escaping () -> Void) -> DispatchSourceTimer {
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + interval)
        timer.setEventHandler(handler: handler)
        timer.resume()
        return timer
    }

    private func writeRecord(pid: Int32, port: Int, entry: String) {
        let record = DaemonRecord(pid: pid, port: port, entry: entry, shellPid: getpid(),
                                  startedAt: ISO8601DateFormatter().string(from: Date()))
        guard let data = try? JSONEncoder().encode(record) else { return }
        FileManager.default.createFile(atPath: paths.daemonStateFile.path, contents: data, attributes: [.posixPermissions: 0o600])
    }

    private func removeRecord() {
        try? FileManager.default.removeItem(at: paths.daemonStateFile)
    }
}

/// Splits a byte stream into lines; only touched on the supervisor queue.
final class LineBuffer {
    private var pending = Data()

    func append(_ data: Data) -> [String] {
        pending.append(data)
        var lines: [String] = []
        while let newline = pending.firstIndex(of: 0x0A) {
            var line = pending[pending.startIndex..<newline]
            if line.last == 0x0D { line = line.dropLast() }
            lines.append(String(decoding: line, as: UTF8.self))
            pending.removeSubrange(pending.startIndex...newline)
        }
        // A runaway line without newline is emitted in chunks rather than buffered forever.
        if pending.count > 64 * 1024 {
            lines.append(String(decoding: pending, as: UTF8.self))
            pending.removeAll()
        }
        return lines
    }

    func flush() -> String? {
        guard !pending.isEmpty else { return nil }
        defer { pending.removeAll() }
        return String(decoding: pending, as: UTF8.self)
    }
}
