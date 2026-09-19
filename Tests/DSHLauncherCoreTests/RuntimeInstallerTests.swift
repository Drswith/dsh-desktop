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
        let archive = payload.appendingPathComponent("runtime.tar.gz")
        try CommandRunner.check("/usr/bin/tar", ["-czf", archive.path, "-C", tree.path, "node", "app"])
        let manifest = RuntimeManifest(dshVersion: dshVersion, nodeVersion: "24.17.0", pnpmVersion: "11.7.0",
                                       arch: "arm64", archiveSHA256: try RuntimeInstaller.sha256(of: archive))
        try JSONEncoder().encode(manifest).write(to: payload.appendingPathComponent("manifest.json"))
        return (payload, manifest)
    }

    private func installer(payload: URL?) -> RuntimeInstaller {
        let paths = AppPaths(home: root.appendingPathComponent("home"))
        return RuntimeInstaller(paths: paths, logger: FileLogger(url: paths.shellLog, echoToStderr: false), payloadDirectory: payload)
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
        XCTAssertEqual(try FileManager.default.destinationOfSymbolicLink(atPath: root.appendingPathComponent("home/runtime/current").path), newManifest.directoryName)
        XCTAssertTrue(FileManager.default.fileExists(atPath: old.directory.path), "previous runtime is kept for rollback")

        // Launching the older app again keeps the newer runtime.
        XCTAssertEqual(try installer(payload: oldPayload).ensureRuntime().receipt.dshVersion, "0.1.5-rc.2")
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
