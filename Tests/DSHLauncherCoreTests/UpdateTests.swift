import CryptoKit
import XCTest
@testable import DSHLauncherCore

final class UpdateFeedTests: XCTestCase {
    private let key = Curve25519.Signing.PrivateKey()
    private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("dsh-launcher-feed-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private var feed: UpdateFeed {
        UpdateFeed(baseURL: root, publicKey: key.publicKey.rawRepresentation.base64EncodedString())!
    }

    private func sample(_ channel: UpdateChannel = .stable) -> UpdateManifest {
        let artifact = UpdateManifest.Artifact(url: URL(string: "https://example.com/dsh-runtime-0.1.6-arm64.aar")!,
                                               sha256: String(repeating: "a", count: 64), size: 10)
        let runtime = UpdateManifest.Runtime(dshVersion: "0.1.6", nodeVersion: "24.17.0", pnpmVersion: "11.7.0",
                                             minShellVersion: "0.1.0", archives: ["arm64": artifact])
        return UpdateManifest(channel: channel, publishedAt: "2026-09-20T00:00:00Z", runtime: runtime)
    }

    /// Write `manifest` as `<channel>.json` with a base64 signature beside it.
    private func publish(_ manifest: UpdateManifest, as channel: UpdateChannel, signedBy signer: Curve25519.Signing.PrivateKey? = nil) throws {
        let data = try JSONEncoder().encode(manifest)
        try data.write(to: root.appendingPathComponent("\(channel.rawValue).json"))
        let signature = try (signer ?? key).signature(for: data).base64EncodedString()
        try Data(signature.utf8).write(to: root.appendingPathComponent("\(channel.rawValue).json.sig"))
    }

    func testFetchesAndVerifiesASignedManifest() throws {
        try publish(sample(), as: .stable)
        XCTAssertEqual(try feed.fetch(.stable), sample())
    }

    func testRejectsAManifestSignedByAnotherKey() throws {
        try publish(sample(), as: .stable, signedBy: Curve25519.Signing.PrivateKey())
        XCTAssertThrowsError(try feed.fetch(.stable)) { error in
            XCTAssertTrue(error.localizedDescription.contains("invalid signature"), error.localizedDescription)
        }
    }

    func testRejectsATamperedManifest() throws {
        try publish(sample(), as: .stable)
        var tampered = sample()
        tampered.runtime?.dshVersion = "9.9.9"
        try JSONEncoder().encode(tampered).write(to: root.appendingPathComponent("stable.json"))
        XCTAssertThrowsError(try feed.fetch(.stable))
    }

    func testRejectsAnotherChannelsManifest() throws {
        try publish(sample(.preview), as: .stable)
        XCTAssertThrowsError(try feed.fetch(.stable)) { error in
            XCTAssertTrue(error.localizedDescription.contains("served the preview manifest"), error.localizedDescription)
        }
    }

    func testRejectsAnUnusablePublicKey() {
        XCTAssertNil(UpdateFeed(baseURL: root, publicKey: "not a key"))
        XCTAssertNil(UpdateFeed(baseURL: root, publicKey: Data(repeating: 1, count: 16).base64EncodedString()))
    }
}

final class RuntimeUpdateDecisionTests: XCTestCase {
    private let artifact = UpdateManifest.Artifact(url: URL(string: "https://example.com/dsh-runtime-0.1.6-arm64.aar")!,
                                                   sha256: String(repeating: "b", count: 64), size: 42)

    private func release(_ version: String = "0.1.6", minShell: String = "0.1.0") -> UpdateManifest.Runtime {
        UpdateManifest.Runtime(dshVersion: version, nodeVersion: "24.17.0", pnpmVersion: "11.7.0",
                               minShellVersion: minShell, archives: ["arm64": artifact])
    }

    private func receipt(_ version: String, identity: String = "installed") -> RuntimeReceipt {
        RuntimeReceipt(identity: identity, dshVersion: version, nodeVersion: "24.17.0", pnpmVersion: "11.7.0",
                       arch: "arm64", source: .bundle, installedAt: "2026-09-20T00:00:00Z")
    }

    private func decide(_ runtime: UpdateManifest.Runtime?, arch: String = "arm64", shell: String = "0.1.0",
                        current: RuntimeReceipt? = nil, pending: RuntimeReceipt? = nil, failed: Set<String> = []) -> RuntimeUpdateDecision {
        RuntimeUpdateDecision.decide(runtime: runtime, arch: arch, shellVersion: shell, current: current, pending: pending, failedIdentities: failed)
    }

    /// The identity the offered release gets once staged.
    private var offeredIdentity: String {
        RuntimeManifest(dshVersion: "0.1.6", nodeVersion: "24.17.0", pnpmVersion: "11.7.0", arch: "arm64",
                        archive: artifact.url.lastPathComponent, archiveSHA256: artifact.sha256).identity
    }

    func testStagesANewerRuntime() {
        guard case .stage(let manifest, let offered) = decide(release(), current: receipt("0.1.5-rc.2")) else {
            return XCTFail("expected a newer runtime to be staged")
        }
        XCTAssertEqual(offered, artifact)
        XCTAssertEqual(manifest.dshVersion, "0.1.6")
        XCTAssertEqual(manifest.archive, "dsh-runtime-0.1.6-arm64.aar")
        XCTAssertEqual(manifest.archiveSHA256, artifact.sha256)
        XCTAssertEqual(manifest.identity, offeredIdentity)
    }

    func testNeverDowngradesOrReinstallsTheSameVersion() {
        XCTAssertEqual(decide(release("0.1.5-rc.2"), current: receipt("0.1.6-alpha.2")), .upToDate("0.1.6-alpha.2"))
        XCTAssertEqual(decide(release("0.1.6"), current: receipt("0.1.6")), .upToDate("0.1.6"))
    }

    func testSkipsWhatThisLauncherCannotTake() {
        XCTAssertEqual(decide(release(), arch: "x86_64"), .skip("no x86_64 runtime for dsh 0.1.6"))
        XCTAssertEqual(decide(release(minShell: "0.3.0"), shell: "0.2.9"), .skip("dsh 0.1.6 needs DSH Launcher 0.3.0 or later"))
        XCTAssertEqual(decide(nil), .skip("the channel lists no runtime"))
    }

    func testSkipsARuntimeThatFailedToStart() {
        XCTAssertEqual(decide(release(), current: receipt("0.1.5-rc.2"), failed: [offeredIdentity]), .skip("dsh 0.1.6 failed to start before"))
    }

    func testReportsARuntimeAlreadyStaged() {
        XCTAssertEqual(decide(release(), current: receipt("0.1.5-rc.2"), pending: receipt("0.1.6", identity: offeredIdentity)), .staged("0.1.6"))
    }
}

final class HTTPTransferTests: XCTestCase {
    private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("dsh-launcher-transfer-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private func source() throws -> (URL, String) {
        let file = root.appendingPathComponent("source.bin")
        try Data(repeating: 7, count: 1000).write(to: file)
        return (file, try RuntimeInstaller.sha256(of: file))
    }

    func testDownloadsAVerifiedFile() throws {
        let (file, sha) = try source()
        let destination = root.appendingPathComponent("download.bin")
        try HTTPTransfer.download(file, to: destination, expectedSize: 1000, sha256: sha)
        XCTAssertEqual(try Data(contentsOf: destination), try Data(contentsOf: file))
    }

    func testRejectsAWrongChecksumOrSizeAndLeavesNothingBehind() throws {
        let (file, sha) = try source()
        let destination = root.appendingPathComponent("download.bin")
        XCTAssertThrowsError(try HTTPTransfer.download(file, to: destination, expectedSize: 1000, sha256: String(repeating: "0", count: 64)))
        XCTAssertThrowsError(try HTTPTransfer.download(file, to: destination, expectedSize: 999, sha256: sha))
        XCTAssertFalse(FileManager.default.fileExists(atPath: destination.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: destination.appendingPathExtension("part").path))
    }
}

final class ActivityProbeTests: XCTestCase {
    private func sleeper() throws -> Process {
        let child = Process()
        child.executableURL = URL(fileURLWithPath: "/bin/sleep")
        child.arguments = ["30"]
        try child.run()
        return child
    }

    func testSeesChildProcesses() throws {
        let child = try sleeper()
        defer { child.terminate(); child.waitUntilExit() }
        XCTAssertTrue(ActivityProbe.childProcesses(of: getpid()).contains(child.processIdentifier))
        XCTAssertEqual(ActivityProbe.childProcesses(of: child.processIdentifier), [])
    }

    func testJudgesSessionLogsByTheirLastWrite() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("dsh-launcher-sessions-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let session = root.appendingPathComponent("--project--/session-1")
        try FileManager.default.createDirectory(at: session, withIntermediateDirectories: true)
        let log = session.appendingPathComponent("session.v3.jsonl.zstd")
        let lock = session.appendingPathComponent("session.lock")
        try Data("x".utf8).write(to: log)
        try Data().write(to: lock)
        let now = Date()
        try FileManager.default.setAttributes([.modificationDate: now.addingTimeInterval(-1200)], ofItemAtPath: log.path)

        let idle = try sleeper()
        defer { idle.terminate(); idle.waitUntilExit() }
        // A fresh lock file is not work; only the session log counts.
        XCTAssertNil(ActivityProbe.busyReason(daemonPid: idle.processIdentifier, sessionsRoot: root, quietPeriod: 600, now: now))

        try FileManager.default.setAttributes([.modificationDate: now.addingTimeInterval(-60)], ofItemAtPath: log.path)
        let reason = ActivityProbe.busyReason(daemonPid: idle.processIdentifier, sessionsRoot: root, quietPeriod: 600, now: now)
        XCTAssertTrue(reason?.contains("session log") == true, reason ?? "nil")
        XCTAssertTrue(ActivityProbe.busyReason(daemonPid: getpid(), sessionsRoot: root, quietPeriod: 0, now: now)?.contains("child process") == true)
    }
}
