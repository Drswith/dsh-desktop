import XCTest
@testable import DSHLauncherCore

final class RuntimeInstallerTests: XCTestCase {
    private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("dsh-launcher-tests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    /// A payload whose "node" is a shell script reporting the expected version.
    private func makePayload(dshVersion: String, named name: String = "payload") throws -> (URL, RuntimeManifest) {
        let fm = FileManager.default
        let tree = root.appendingPathComponent("\(name)-tree")
        let nodeBin = tree.appendingPathComponent("node/bin")
        let dsh = tree.appendingPathComponent("app/node_modules/@deepseek-ai/dsh")
        try fm.createDirectory(at: nodeBin, withIntermediateDirectories: true)
        try fm.createDirectory(at: dsh.appendingPathComponent("lib"), withIntermediateDirectories: true)
        let node = nodeBin.appendingPathComponent("node")
        try Data("#!/bin/sh\necho v24.17.0\n".utf8).write(to: node)
        try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: node.path)
        try Data(#"{"name":"@deepseek-ai/dsh","version":"\#(dshVersion)"}"#.utf8).write(to: dsh.appendingPathComponent("package.json"))
        try Data("// cli\n".utf8).write(to: dsh.appendingPathComponent("lib/bin.js"))

        let payload = root.appendingPathComponent(name)
        try fm.createDirectory(at: payload, withIntermediateDirectories: true)
        let archive = payload.appendingPathComponent("runtime.aar")
        try CommandRunner.check("/usr/bin/aa", ["archive", "-d", tree.path, "-o", archive.path, "-a", "lzma",
                                                "-include-path", "node", "-include-path", "app"])
        let manifest = RuntimeManifest(dshVersion: dshVersion, nodeVersion: "24.17.0", pnpmVersion: "11.7.0",
                                       arch: "arm64", archiveSHA256: try RuntimeInstaller.sha256(of: archive))
        try JSONEncoder().encode(manifest).write(to: payload.appendingPathComponent("manifest.json"))
        return (payload, manifest)
    }

    private func installer(payload: URL?) -> RuntimeInstaller {
        let paths = AppPaths(home: root.appendingPathComponent("home"))
        return RuntimeInstaller(paths: paths, logger: FileLogger(url: paths.shellLog, echoToStderr: false), payloadDirectory: payload)
    }

    private var runtimeRoot: URL { root.appendingPathComponent("home/runtime") }

    private func link(_ name: String) -> String? {
        try? FileManager.default.destinationOfSymbolicLink(atPath: runtimeRoot.appendingPathComponent(name).path)
    }

    /// Everything under `runtime/` except the `current`, `previous` and `pending` links.
    private func runtimeDirectories() throws -> Set<String> {
        Set(try FileManager.default.contentsOfDirectory(atPath: runtimeRoot.path)).subtracting(["current", "previous", "pending"])
    }

    func testInstallsOnceThenReuses() throws {
        let (payload, manifest) = try makePayload(dshVersion: "0.1.5-rc.2")
        let subject = installer(payload: payload)
        let first = try subject.ensureRuntime()
        XCTAssertEqual(first.receipt.identity, manifest.identity)
        XCTAssertEqual(first.receipt.source, .bundle)
        XCTAssertEqual(first.directory.lastPathComponent, manifest.directoryName)
        XCTAssertTrue(FileManager.default.isExecutableFile(atPath: first.nodeExecutable.path))

        // A marker inside the installed tree proves the second launch did not re-extract.
        let marker = first.directory.appendingPathComponent("marker")
        try Data().write(to: marker)
        let second = try subject.ensureRuntime()
        XCTAssertEqual(second.directory, first.directory)
        XCTAssertTrue(FileManager.default.fileExists(atPath: marker.path))
        XCTAssertEqual(subject.currentRuntime()?.receipt, first.receipt)
    }

    func testUpgradeSwitchesCurrentAndKeepsOnePrevious() throws {
        let (oldPayload, _) = try makePayload(dshVersion: "0.1.5-rc.1", named: "old")
        let old = try installer(payload: oldPayload).ensureRuntime()
        let (newPayload, newManifest) = try makePayload(dshVersion: "0.1.5-rc.2", named: "new")
        let subject = installer(payload: newPayload)
        let upgraded = try subject.ensureRuntime()
        XCTAssertEqual(upgraded.receipt.dshVersion, "0.1.5-rc.2")
        XCTAssertEqual(link("current"), newManifest.directoryName)
        XCTAssertEqual(link("previous"), old.directory.lastPathComponent, "the replaced runtime is kept as previous")

        // Launching the older app again keeps the newer runtime.
        XCTAssertEqual(try installer(payload: oldPayload).ensureRuntime().receipt.dshVersion, "0.1.5-rc.2")
    }

    func testAnotherUpgradeDropsTheOldestRuntime() throws {
        var manifests: [RuntimeManifest] = []
        for (index, version) in ["0.1.5-rc.1", "0.1.5-rc.2", "0.1.5"].enumerated() {
            let (payload, manifest) = try makePayload(dshVersion: version, named: "payload\(index)")
            _ = try installer(payload: payload).ensureRuntime()
            manifests.append(manifest)
        }
        XCTAssertEqual(link("current"), manifests[2].directoryName)
        XCTAssertEqual(link("previous"), manifests[1].directoryName)
        XCTAssertEqual(try runtimeDirectories(), [manifests[2].directoryName, manifests[1].directoryName])
    }

    func testReinstallingTheCurrentRuntimeKeepsPrevious() throws {
        let (oldPayload, oldManifest) = try makePayload(dshVersion: "0.1.5-rc.1", named: "old")
        let (newPayload, newManifest) = try makePayload(dshVersion: "0.1.5-rc.2", named: "new")
        _ = try installer(payload: oldPayload).ensureRuntime()
        _ = try installer(payload: newPayload).ensureRuntime()
        _ = try installer(payload: newPayload).installBundled() // "Reinstall Bundled Runtime"
        XCTAssertEqual(link("current"), newManifest.directoryName)
        XCTAssertEqual(link("previous"), oldManifest.directoryName)
        XCTAssertEqual(try runtimeDirectories(), [newManifest.directoryName, oldManifest.directoryName])
    }

    func testWithoutKeepingPreviousOnlyCurrentRemains() throws {
        let (oldPayload, _) = try makePayload(dshVersion: "0.1.5-rc.1", named: "old")
        let (newPayload, newManifest) = try makePayload(dshVersion: "0.1.5-rc.2", named: "new")
        _ = try installer(payload: oldPayload).ensureRuntime()
        let subject = installer(payload: newPayload)
        subject.keepsPreviousRuntime = false
        _ = try subject.ensureRuntime()
        XCTAssertNil(link("previous"))
        XCTAssertEqual(try runtimeDirectories(), [newManifest.directoryName])
    }

    func testTurningRetentionOffRemovesPreviousAtOnce() throws {
        let (oldPayload, _) = try makePayload(dshVersion: "0.1.5-rc.1", named: "old")
        let (newPayload, newManifest) = try makePayload(dshVersion: "0.1.5-rc.2", named: "new")
        _ = try installer(payload: oldPayload).ensureRuntime()
        let subject = installer(payload: newPayload)
        _ = try subject.ensureRuntime()
        XCTAssertNotNil(link("previous"))
        subject.keepsPreviousRuntime = false
        subject.applyRetention()
        XCTAssertNil(link("previous"))
        XCTAssertEqual(link("current"), newManifest.directoryName)
        XCTAssertEqual(try runtimeDirectories(), [newManifest.directoryName])
    }

    /// Stage `payload` as a downloaded update and mark it pending.
    @discardableResult
    private func stagePending(_ subject: RuntimeInstaller, from payload: URL, _ manifest: RuntimeManifest) throws -> InstalledRuntime {
        let staged = try subject.stage(archive: payload.appendingPathComponent(manifest.archive), manifest: manifest, source: .update)
        try subject.setPending(staged)
        return staged
    }

    func testStagedUpdateWaitsAsPendingAndSurvivesPruning() throws {
        let (oldPayload, oldManifest) = try makePayload(dshVersion: "0.1.5-rc.2", named: "old")
        let (newPayload, newManifest) = try makePayload(dshVersion: "0.1.6", named: "new")
        let subject = installer(payload: oldPayload)
        _ = try subject.ensureRuntime()
        let staged = try stagePending(subject, from: newPayload, newManifest)
        XCTAssertEqual(staged.receipt.source, .update)
        XCTAssertEqual(link("current"), oldManifest.directoryName, "staging does not switch")
        XCTAssertEqual(subject.pendingRuntime()?.receipt.dshVersion, "0.1.6")

        subject.keepsPreviousRuntime = false
        subject.applyRetention()
        XCTAssertEqual(try runtimeDirectories(), [oldManifest.directoryName, newManifest.directoryName])
    }

    func testConfirmedSwitchKeepsTheRuntimeItReplaced() throws {
        let (oldPayload, oldManifest) = try makePayload(dshVersion: "0.1.5-rc.2", named: "old")
        let (newPayload, newManifest) = try makePayload(dshVersion: "0.1.6", named: "new")
        let subject = installer(payload: oldPayload)
        _ = try subject.ensureRuntime()
        try stagePending(subject, from: newPayload, newManifest)

        let activation = try XCTUnwrap(try subject.beginPendingActivation())
        XCTAssertEqual(activation.dshVersion, "0.1.6")
        XCTAssertEqual(activation.replaced, oldManifest.directoryName)
        XCTAssertEqual(link("current"), newManifest.directoryName)
        XCTAssertEqual(link("previous"), oldManifest.directoryName)
        XCTAssertNil(link("pending"))
        XCTAssertEqual(subject.currentActivation(), activation)
        // A later launch keeps the switched-to runtime instead of reinstalling the bundled one.
        XCTAssertEqual(try subject.ensureRuntime().receipt.dshVersion, "0.1.6")

        subject.confirmActivation()
        XCTAssertNil(subject.currentActivation())
        XCTAssertEqual(link("previous"), oldManifest.directoryName)
        XCTAssertEqual(try runtimeDirectories(), [newManifest.directoryName, oldManifest.directoryName])
    }

    func testConfirmedSwitchWithoutRetentionKeepsOnlyTheNewRuntime() throws {
        let (oldPayload, _) = try makePayload(dshVersion: "0.1.5-rc.2", named: "old")
        let (newPayload, newManifest) = try makePayload(dshVersion: "0.1.6", named: "new")
        let subject = installer(payload: oldPayload)
        _ = try subject.ensureRuntime()
        try stagePending(subject, from: newPayload, newManifest)
        subject.keepsPreviousRuntime = false
        _ = try subject.beginPendingActivation()
        XCTAssertNotNil(link("previous"), "the replaced runtime stays until the switch is confirmed")

        subject.confirmActivation()
        XCTAssertNil(link("previous"))
        XCTAssertEqual(try runtimeDirectories(), [newManifest.directoryName])
    }

    func testFailedSwitchGoesBackAndRemovesTheNewRuntime() throws {
        let (firstPayload, firstManifest) = try makePayload(dshVersion: "0.1.5-rc.1", named: "first")
        let (secondPayload, secondManifest) = try makePayload(dshVersion: "0.1.5-rc.2", named: "second")
        let (newPayload, newManifest) = try makePayload(dshVersion: "0.1.6", named: "new")
        _ = try installer(payload: firstPayload).ensureRuntime()
        let subject = installer(payload: secondPayload)
        _ = try subject.ensureRuntime()
        try stagePending(subject, from: newPayload, newManifest)
        _ = try subject.beginPendingActivation()

        let restored = try XCTUnwrap(subject.rollBackActivation())
        XCTAssertEqual(restored.receipt.dshVersion, "0.1.5-rc.2")
        XCTAssertNil(subject.currentActivation())
        XCTAssertEqual(link("current"), secondManifest.directoryName)
        XCTAssertEqual(link("previous"), firstManifest.directoryName, "the older kept runtime survives the failed switch")
        XCTAssertEqual(try runtimeDirectories(), [secondManifest.directoryName, firstManifest.directoryName])
    }

    func testPendingRuntimeOlderThanTheBundledOneIsDropped() throws {
        let (oldPayload, _) = try makePayload(dshVersion: "0.1.5-rc.2", named: "old")
        let (pendingPayload, pendingManifest) = try makePayload(dshVersion: "0.1.6", named: "pending")
        let (bundledPayload, _) = try makePayload(dshVersion: "0.1.7", named: "bundled")
        _ = try installer(payload: oldPayload).ensureRuntime()
        let subject = installer(payload: bundledPayload)
        try stagePending(subject, from: pendingPayload, pendingManifest)

        XCTAssertNil(try subject.beginPendingActivation())
        XCTAssertNil(link("pending"))
        XCTAssertFalse(try runtimeDirectories().contains(pendingManifest.directoryName))
    }

    func testRejectsTamperedArchive() throws {
        let (payload, manifest) = try makePayload(dshVersion: "0.1.5-rc.2")
        var tampered = manifest
        tampered.archiveSHA256 = String(repeating: "0", count: 64)
        try JSONEncoder().encode(tampered).write(to: payload.appendingPathComponent("manifest.json"))
        XCTAssertThrowsError(try installer(payload: payload).ensureRuntime()) { error in
            XCTAssertTrue(error.localizedDescription.contains("checksum mismatch"), error.localizedDescription)
        }
        XCTAssertNil(installer(payload: payload).currentRuntime())
    }

    func testRejectsWrongDshVersion() throws {
        let (payload, manifest) = try makePayload(dshVersion: "0.1.5-rc.2")
        var wrong = manifest
        wrong.dshVersion = "9.9.9"
        try JSONEncoder().encode(wrong).write(to: payload.appendingPathComponent("manifest.json"))
        XCTAssertThrowsError(try installer(payload: payload).installBundled())
        let leftovers = try FileManager.default.contentsOfDirectory(atPath: root.appendingPathComponent("home/runtime").path)
        XCTAssertTrue(leftovers.filter { $0.hasPrefix(".staging-") }.isEmpty, "staging is cleaned up: \(leftovers)")
    }

    func testNoPayloadAndNothingInstalledIsAnError() {
        XCTAssertThrowsError(try installer(payload: nil).ensureRuntime())
    }
}

final class NetworkProbeTests: XCTestCase {
    /// Minimal loopback listener that answers every connection with `response`.
    private final class TinyServer {
        let port: Int
        private let fd: Int32

        init(response: String) throws {
            // Locals only until every stored property is set: closures may not capture `self` earlier.
            let listener = socket(AF_INET, SOCK_STREAM, 0)
            var reuse: Int32 = 1
            setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))
            var address = PortProbe.loopback(port: 0)
            let bound = withUnsafePointer(to: &address) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(listener, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
            }
            guard bound == 0, listen(listener, 8) == 0 else {
                close(listener)
                throw CommandError("listen failed")
            }
            var actual = sockaddr_in()
            var length = socklen_t(MemoryLayout<sockaddr_in>.size)
            _ = withUnsafeMutablePointer(to: &actual) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(listener, $0, &length) }
            }
            fd = listener
            port = Int(UInt16(bigEndian: actual.sin_port))
            DispatchQueue.global().async {
                while true {
                    let client = accept(listener, nil, nil)
                    guard client >= 0 else { return }
                    var buffer = [UInt8](repeating: 0, count: 1024)
                    _ = recv(client, &buffer, buffer.count, 0)
                    _ = response.withCString { send(client, $0, strlen($0), 0) }
                    close(client)
                }
            }
        }

        func stop() { close(fd) }
    }

    func testHealthProbeAcceptsAnyHttpStatus() throws {
        let server = try TinyServer(response: "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
        defer { server.stop() }
        XCTAssertEqual(HealthProbe.check(port: server.port, timeout: 2), .healthy(status: 401))
        XCTAssertFalse(PortProbe.isAvailable(port: server.port))
        XCTAssertNotEqual(PortProbe.firstAvailable(startingAt: server.port, attempts: 5), server.port)
    }

    func testHealthProbeRejectsNonHttpAndClosedPorts() throws {
        let server = try TinyServer(response: "SSH-2.0-OpenSSH\r\n")
        defer { server.stop() }
        XCTAssertFalse(HealthProbe.check(port: server.port, timeout: 2).isHealthy)
        let free = try XCTUnwrap(PortProbe.firstAvailable(startingAt: 43_000, attempts: 200))
        XCTAssertFalse(HealthProbe.check(port: free, timeout: 1).isHealthy)
    }

    func testLineBufferSplitsAcrossChunks() {
        let buffer = LineBuffer()
        XCTAssertEqual(buffer.append(Data("dsh we".utf8)), [])
        XCTAssertEqual(buffer.append(Data("b: x\r\nsecond\nthi".utf8)), ["dsh web: x", "second"])
        XCTAssertEqual(buffer.flush(), "thi")
        XCTAssertNil(buffer.flush())
    }
}
