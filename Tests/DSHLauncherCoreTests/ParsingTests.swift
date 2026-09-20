import XCTest
@testable import DSHLauncherCore

final class SemVerTests: XCTestCase {
    func testParsesReleaseAndPrerelease() throws {
        let version = try XCTUnwrap(SemVer("v0.1.5-rc.2+build.7"))
        XCTAssertEqual(version.major, 0)
        XCTAssertEqual(version.minor, 1)
        XCTAssertEqual(version.patch, 5)
        XCTAssertEqual(version.prerelease, ["rc", "2"])
        XCTAssertEqual(version.description, "0.1.5-rc.2")
        XCTAssertNil(SemVer("1.2"))
        XCTAssertNil(SemVer("1.2.x"))
        XCTAssertNil(SemVer("1.2.3-"))
    }

    func testPrecedenceFollowsSemVer() throws {
        let ordered = ["0.1.2-alpha.3", "0.1.5-alpha.1", "0.1.5-alpha.2", "0.1.5-rc.1", "0.1.5-rc.2", "0.1.5", "0.1.6-alpha.1", "0.2.0"]
        let parsed = try ordered.map { try XCTUnwrap(SemVer($0)) }
        for (lower, higher) in zip(parsed, parsed.dropFirst()) {
            XCTAssertLessThan(lower, higher, "\(lower) < \(higher)")
        }
        XCTAssertLessThan(try XCTUnwrap(SemVer("1.0.0-2")), try XCTUnwrap(SemVer("1.0.0-alpha")))
        XCTAssertLessThan(try XCTUnwrap(SemVer("1.0.0-alpha")), try XCTUnwrap(SemVer("1.0.0-alpha.1")))
        XCTAssertEqual(SemVer("1.0.0+a"), SemVer("1.0.0+b"))
    }
}

final class ReadyLineTests: XCTestCase {
    func testExtractsAuthenticatedLoopbackURL() throws {
        let url = try XCTUnwrap(ReadyLine.authenticatedURL(in: "dsh web: http://127.0.0.1:31080/?token=abc_DEF-123"))
        XCTAssertEqual(url.port, 31080)
        XCTAssertEqual(ReadyLine.cleanURL(url).absoluteString, "http://127.0.0.1:31080/")
    }

    func testIgnoresLanSuffixAndColor() throws {
        let line = "\u{1B}[32mdsh web: http://127.0.0.1:4000/?token=t0k (LAN: http://192.168.1.2:4000/?token=t0k)\u{1B}[0m"
        let url = try XCTUnwrap(ReadyLine.authenticatedURL(in: line))
        XCTAssertEqual(url.host, "127.0.0.1")
        XCTAssertEqual(url.port, 4000)
    }

    func testRejectsLinesWithoutTokenOrOnOtherHosts() {
        XCTAssertNil(ReadyLine.authenticatedURL(in: "dsh web: opening the default browser; pass --no-open to disable"))
        XCTAssertNil(ReadyLine.authenticatedURL(in: "dsh web: http://127.0.0.1:4000/"))
        XCTAssertNil(ReadyLine.authenticatedURL(in: "dsh web: http://evil.example:4000/?token=x"))
        XCTAssertNil(ReadyLine.authenticatedURL(in: "web-app: could not open the default browser"))
    }

    func testRedactsEveryToken() {
        let redacted = ReadyLine.redact("dsh web: http://127.0.0.1:1/?token=secret (LAN: http://10.0.0.2:1/?token=secret)")
        XCTAssertFalse(redacted.contains("secret"))
        XCTAssertEqual(redacted.components(separatedBy: "token=<redacted>").count, 3)
    }
}

final class ShellEnvironmentTests: XCTestCase {
    func testParsesNulSeparatedEnvironmentBetweenMarkers() throws {
        var data = Data("motd noise\n\(ShellEnvironment.beginMarker)".utf8)
        data.append(Data("PATH=/opt/homebrew/bin:/usr/bin\u{0}MULTI=a\nb=c\u{0}EMPTY=\u{0}".utf8))
        data.append(Data("\(ShellEnvironment.endMarker)trailing".utf8))
        let environment = try XCTUnwrap(ShellEnvironment.parse(data))
        XCTAssertEqual(environment["PATH"], "/opt/homebrew/bin:/usr/bin")
        XCTAssertEqual(environment["MULTI"], "a\nb=c")
        XCTAssertEqual(environment["EMPTY"], "")
        XCTAssertNil(ShellEnvironment.parse(Data("no markers".utf8)))
    }

    func testDaemonEnvironmentDropsLaunchArtifactsAndAppliesOverrides() {
        let login = ["PATH": "/custom/bin:/usr/bin", "HOME": "/Users/me", "__CFBundleIdentifier": "x", "SHLVL": "2", "DSH_LAUNCHER_RESOLVING_SHELL_ENV": "1"]
        let environment = ShellEnvironment.daemonEnvironment(login: login, overrides: ["FOO": "bar"])
        XCTAssertEqual(environment["PATH"], "/custom/bin:/usr/bin")
        XCTAssertNil(environment["__CFBundleIdentifier"])
        XCTAssertNil(environment["SHLVL"])
        XCTAssertNil(environment["DSH_LAUNCHER_RESOLVING_SHELL_ENV"])
        XCTAssertEqual(environment["FOO"], "bar")
        XCTAssertNotNil(environment["LANG"])
    }

    func testFallbackPathExtendsLaunchdPath() {
        let environment = ShellEnvironment.daemonEnvironment(login: nil, current: ["PATH": "/usr/bin:/bin", "HOME": "/Users/me"])
        let entries = (environment["PATH"] ?? "").split(separator: ":").map(String.init)
        XCTAssertEqual(entries.first, "/opt/homebrew/bin")
        XCTAssertTrue(entries.contains("/usr/bin"))
        XCTAssertEqual(entries.count, Set(entries).count, "no duplicate PATH entries")
    }
}

final class LaunchPlanTests: XCTestCase {
    private func plan(profile: String, home: URL) -> DaemonLaunchPlan {
        DaemonLaunchPlan(
            node: URL(fileURLWithPath: "/rt/node/bin/node"),
            entry: URL(fileURLWithPath: "/rt/app/node_modules/@deepseek-ai/dsh/lib/bin.js"),
            dshVersion: "0.1.5-rc.2", nodeVersion: "24.17.0", profile: profile, dshHome: home,
            preferredPort: 31080, extraArgs: ["--trusted-host", "dev.local"], environment: [:],
            workingDirectory: URL(fileURLWithPath: "/")
        )
    }

    func testArgumentsPutLauncherFlagsBeforeAppFlags() {
        let args = plan(profile: "launcher", home: URL(fileURLWithPath: "/h")).arguments(port: 31081, initializeProfile: true)
        XCTAssertEqual(args, [
            "/rt/app/node_modules/@deepseek-ai/dsh/lib/bin.js", "--profile", "launcher",
            "--from-default-profile", "web", "--no-open", "--host", "127.0.0.1", "--port", "31081",
            "--trusted-host", "dev.local",
        ])
    }

    func testCustomProfileInitializesOnceAndShippedProfilesNever() throws {
        let home = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: home) }
        let custom = plan(profile: "launcher", home: home)
        XCTAssertTrue(custom.needsProfileInitialization())
        try FileManager.default.createDirectory(at: custom.profileDirectory, withIntermediateDirectories: true)
        try Data("{}".utf8).write(to: custom.profileDirectory.appendingPathComponent("package.json"))
        XCTAssertFalse(custom.needsProfileInitialization())
        XCTAssertFalse(plan(profile: "web", home: home).needsProfileInitialization())
    }

    func testResolvesDshHomeLikeTheCli() {
        let home = URL(fileURLWithPath: "/Users/me")
        XCTAssertEqual(DaemonLaunchPlan.resolveDshHome(configured: nil, environment: [:], home: home).path, "/Users/me/.dsh")
        XCTAssertEqual(DaemonLaunchPlan.resolveDshHome(configured: nil, environment: ["DSH_HOME": "  "], home: home).path, "/Users/me/.dsh")
        XCTAssertEqual(DaemonLaunchPlan.resolveDshHome(configured: nil, environment: ["DSH_HOME": "~/alt"], home: home).path, "/Users/me/alt")
        XCTAssertEqual(DaemonLaunchPlan.resolveDshHome(configured: "/data/dsh", environment: ["DSH_HOME": "~/alt"], home: home).path, "/data/dsh")
    }

    func testRuntimeSummaryUsesProductNames() {
        XCTAssertEqual(plan(profile: "launcher", home: URL(fileURLWithPath: "/h")).runtimeSummary, "DSH 0.1.5-rc.2 · Node.js 24.17.0")
    }
}

final class InstallDecisionTests: XCTestCase {
    private func manifest(_ version: String, sha: String = "aaaa") -> RuntimeManifest {
        RuntimeManifest(dshVersion: version, nodeVersion: "24.17.0", pnpmVersion: "11.7.0", arch: "arm64", archiveSHA256: sha)
    }

    private func receipt(for manifest: RuntimeManifest, source: RuntimeReceipt.Source = .bundle) -> RuntimeReceipt {
        RuntimeReceipt(identity: manifest.identity, dshVersion: manifest.dshVersion, nodeVersion: manifest.nodeVersion,
                       pnpmVersion: manifest.pnpmVersion, arch: manifest.arch, source: source, installedAt: "now")
    }

    func testDecisions() {
        let bundled = manifest("0.1.5-rc.2")
        XCTAssertEqual(InstallDecision.decide(bundled: bundled, installed: nil, installedUsable: false), .installBundled(reason: "not_installed"))
        XCTAssertEqual(InstallDecision.decide(bundled: bundled, installed: receipt(for: bundled), installedUsable: true), .reuseInstalled)
        XCTAssertEqual(InstallDecision.decide(bundled: bundled, installed: receipt(for: bundled), installedUsable: false), .installBundled(reason: "installed_unusable"))
        XCTAssertEqual(InstallDecision.decide(bundled: bundled, installed: receipt(for: manifest("0.1.5-rc.1")), installedUsable: true), .installBundled(reason: "upgrade"))
        XCTAssertEqual(InstallDecision.decide(bundled: bundled, installed: receipt(for: manifest("0.1.6-alpha.2"), source: .update), installedUsable: true), .keepInstalledNewer)
        XCTAssertEqual(InstallDecision.decide(bundled: bundled, installed: receipt(for: manifest("0.1.5-rc.2", sha: "bbbb")), installedUsable: true), .installBundled(reason: "payload_changed"))
        XCTAssertEqual(InstallDecision.decide(bundled: nil, installed: receipt(for: bundled), installedUsable: true), .reuseInstalled)
        XCTAssertEqual(InstallDecision.decide(bundled: nil, installed: nil, installedUsable: false), .noRuntime)
    }
}
