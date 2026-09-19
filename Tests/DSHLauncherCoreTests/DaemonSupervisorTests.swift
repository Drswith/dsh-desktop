import XCTest
@testable import DSHLauncherCore

/// Drives the real supervisor against a Python stand-in for `dsh web` that prints
/// the readiness line and answers HTTP 401, like the Web profile does.
final class DaemonSupervisorTests: XCTestCase {
    private var root: URL!
    private var script: URL!
    private let python = "/usr/bin/python3"

    private static let fakeDaemon = """
    import http.server, os, signal, sys, time
    args = sys.argv[1:]
    port = int(args[args.index("--port") + 1])
    mode = os.environ.get("DSH_FAKE_MODE", "ok")
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    if mode == "crash":
        print("boom: simulated failure", file=sys.stderr, flush=True)
        sys.exit(3)
    print("booting fake dsh", flush=True)
    if mode == "nohttp":
        print(f"dsh web: http://127.0.0.1:{port}/?token=faketoken", flush=True)
        while True:
            time.sleep(1)
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_HEAD(self):
            self.send_response(401)
            self.end_headers()
        do_GET = do_HEAD
        def log_message(self, *_):
            pass
    server = http.server.HTTPServer(("127.0.0.1", port), Handler)
    print(f"dsh web: http://127.0.0.1:{port}/?token=faketoken", flush=True)
    server.serve_forever()
    """

    override func setUpWithError() throws {
        try XCTSkipUnless(FileManager.default.isExecutableFile(atPath: python), "python3 is required for the fake daemon")
        root = FileManager.default.temporaryDirectory.appendingPathComponent("dsh-launcher-supervisor-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        script = root.appendingPathComponent("fake_dsh.py")
        try Data(Self.fakeDaemon.utf8).write(to: script)
    }

    override func tearDownWithError() throws {
        if let root { try? FileManager.default.removeItem(at: root) }
    }

    private func makeSupervisor(_ configure: (inout SupervisorPolicy) -> Void = { _ in }) throws -> (DaemonSupervisor, AppPaths) {
        let paths = AppPaths(home: root.appendingPathComponent("home"))
        try paths.prepare()
        var policy = SupervisorPolicy()
        policy.startupTimeout = 20
        policy.stopGrace = 3
        policy.restartBackoff = [0.05]
        configure(&policy)
        let supervisor = DaemonSupervisor(
            paths: paths,
            log: FileLogger(url: paths.shellLog, echoToStderr: false),
            output: FileLogger(url: paths.daemonLog, echoToStderr: false),
            policy: policy
        )
        return (supervisor, paths)
    }

    private func plan(mode: String, port: Int) -> DaemonLaunchPlan {
        DaemonLaunchPlan(
            node: URL(fileURLWithPath: python), entry: script, dshVersion: "test", nodeVersion: nil,
            profile: "web", dshHome: root.appendingPathComponent("dsh-home"), preferredPort: port, extraArgs: [],
            environment: ["PATH": "/usr/bin:/bin", "HOME": NSHomeDirectory(), "DSH_FAKE_MODE": mode],
            workingDirectory: root
        )
    }

    private func freePort() throws -> Int {
        try XCTUnwrap(PortProbe.firstAvailable(startingAt: 44_000 + Int.random(in: 0..<5_000), attempts: 50))
    }

    /// Collects every published state; `wait` returns once `predicate` matches one.
    private final class StateRecorder {
        private(set) var states: [DaemonState] = []

        func attach(_ supervisor: DaemonSupervisor) {
            supervisor.onStateChange = { [weak self] in self?.states.append($0) }
        }

        func wait(timeout: TimeInterval, _ predicate: (DaemonState) -> Bool) -> DaemonState? {
            let deadline = Date().addingTimeInterval(timeout)
            while Date() < deadline {
                if let match = states.last(where: predicate) { return match }
                RunLoop.main.run(until: Date().addingTimeInterval(0.05))
            }
            return nil
        }
    }

    func testStartsServesAndStopsGracefully() throws {
        let (supervisor, paths) = try makeSupervisor()
        let recorder = StateRecorder()
        recorder.attach(supervisor)
        let port = try freePort()
        supervisor.start(plan(mode: "ok", port: port))

        let running = try XCTUnwrap(recorder.wait(timeout: 20) { $0.info != nil }?.info, "states: \(recorder.states)")
        XCTAssertEqual(running.port, port)
        XCTAssertEqual(running.authenticatedURL.absoluteString, "http://127.0.0.1:\(port)/?token=faketoken")
        XCTAssertEqual(HealthProbe.check(port: port), .healthy(status: 401))
        XCTAssertTrue(FileManager.default.fileExists(atPath: paths.daemonStateFile.path))

        let stopped = expectation(description: "stopped")
        supervisor.stop { stopped.fulfill() }
        wait(for: [stopped], timeout: 10)
        XCTAssertEqual(supervisor.currentState, .stopped)
        XCTAssertFalse(ProcessSpawner.isAlive(running.pid))
        XCTAssertFalse(FileManager.default.fileExists(atPath: paths.daemonStateFile.path))

        let log = try String(contentsOf: paths.daemonLog, encoding: .utf8)
        XCTAssertTrue(log.contains("token=<redacted>"), log)
        XCTAssertFalse(log.contains("faketoken"), "launch tokens never reach the log")
    }

    func testRestartReplacesTheProcess() throws {
        let (supervisor, _) = try makeSupervisor()
        let recorder = StateRecorder()
        recorder.attach(supervisor)
        supervisor.start(plan(mode: "ok", port: try freePort()))
        let first = try XCTUnwrap(recorder.wait(timeout: 20) { $0.info != nil }?.info)
        supervisor.restart()
        let second = try XCTUnwrap(recorder.wait(timeout: 20) { ($0.info?.pid ?? first.pid) != first.pid }?.info)
        XCTAssertFalse(ProcessSpawner.isAlive(first.pid))
        XCTAssertEqual(second.port, first.port, "the port is free again after a clean stop")
        let stopped = expectation(description: "stopped")
        supervisor.stop { stopped.fulfill() }
        wait(for: [stopped], timeout: 10)
    }

    func testFallsBackToTheNextPortWhenTaken() throws {
        let (supervisor, _) = try makeSupervisor()
        let recorder = StateRecorder()
        recorder.attach(supervisor)
        let taken = try freePort()
        let blocker = socket(AF_INET, SOCK_STREAM, 0)
        defer { close(blocker) }
        var address = PortProbe.loopback(port: taken)
        _ = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(blocker, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        listen(blocker, 1)
        supervisor.start(plan(mode: "ok", port: taken))
        let running = try XCTUnwrap(recorder.wait(timeout: 20) { $0.info != nil }?.info)
        XCTAssertGreaterThan(running.port, taken)
        let stopped = expectation(description: "stopped")
        supervisor.stop { stopped.fulfill() }
        wait(for: [stopped], timeout: 10)
    }

    func testCrashLoopEndsInFailedStateWithStderr() throws {
        let (supervisor, _) = try makeSupervisor { $0.maxCrashesInWindow = 3 }
        let recorder = StateRecorder()
        recorder.attach(supervisor)
        supervisor.start(plan(mode: "crash", port: try freePort()))
        guard case .failed(let message)? = recorder.wait(timeout: 20, { if case .failed = $0 { return true } else { return false } }) else {
            return XCTFail("never failed: \(recorder.states)")
        }
        XCTAssertTrue(message.contains("boom: simulated failure"), message)
        let attempts = recorder.states.compactMap { state -> Int? in
            if case .starting(let attempt) = state { return attempt }
            return nil
        }
        XCTAssertGreaterThanOrEqual(attempts.max() ?? 0, 3, "\(recorder.states)")
    }

    func testWatchdogRestartsAnUnresponsiveDaemon() throws {
        let (supervisor, _) = try makeSupervisor {
            $0.healthInterval = 0.2
            $0.healthTimeout = 0.5
            $0.maxHealthFailures = 2
        }
        let recorder = StateRecorder()
        recorder.attach(supervisor)
        supervisor.start(plan(mode: "nohttp", port: try freePort()))
        let first = try XCTUnwrap(recorder.wait(timeout: 20) { $0.info != nil }?.info)
        let replacement = try XCTUnwrap(recorder.wait(timeout: 20) { ($0.info?.pid ?? first.pid) != first.pid }?.info,
                                        "states: \(recorder.states)")
        XCTAssertNotEqual(replacement.pid, first.pid)
        let stopped = expectation(description: "stopped")
        supervisor.stop { stopped.fulfill() }
        wait(for: [stopped], timeout: 10)
    }
}
