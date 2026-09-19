import CryptoKit
import Foundation

/// One runtime directory: `node/`, `pnpm/`, `app/node_modules/@deepseek-ai/dsh`, `receipt.json`.
public struct InstalledRuntime: Equatable, Sendable {
    public let directory: URL
    public let receipt: RuntimeReceipt

    public init(directory: URL, receipt: RuntimeReceipt) {
        self.directory = directory
        self.receipt = receipt
    }

    public var nodeExecutable: URL { Self.nodeExecutable(in: directory) }
    public var dshEntry: URL { Self.dshEntry(in: directory) }
    public var binDirectory: URL { directory.appendingPathComponent("bin", isDirectory: true) }

    static func nodeExecutable(in dir: URL) -> URL { dir.appendingPathComponent("node/bin/node") }
    static func dshEntry(in dir: URL) -> URL { dir.appendingPathComponent("app/node_modules/@deepseek-ai/dsh/lib/bin.js") }
    static func dshManifest(in dir: URL) -> URL { dir.appendingPathComponent("app/node_modules/@deepseek-ai/dsh/package.json") }
}

/// Installs the bundled payload into `~/.dsh-launcher/runtime/<id>` and flips the
/// `current` symlink atomically, keeping the previous runtime for rollback.
public final class RuntimeInstaller: @unchecked Sendable {
    private let paths: AppPaths
    private let logger: FileLogger
    private let payloadDirectory: URL?

    public init(paths: AppPaths, logger: FileLogger, payloadDirectory: URL?) {
        self.paths = paths
        self.logger = logger
        self.payloadDirectory = payloadDirectory
    }

    public func bundledManifest() -> RuntimeManifest? {
        guard let payloadDirectory else { return nil }
        let url = payloadDirectory.appendingPathComponent("manifest.json")
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(RuntimeManifest.self, from: data)
    }

    /// The runtime `current` points at, when its receipt and entry points exist.
    public func currentRuntime() -> InstalledRuntime? {
        guard let target = try? FileManager.default.destinationOfSymbolicLink(atPath: paths.currentRuntimeLink.path) else { return nil }
        let dir = paths.runtimeRoot.appendingPathComponent(target, isDirectory: true)
        guard let receipt = readReceipt(in: dir) else { return nil }
        let runtime = InstalledRuntime(directory: dir, receipt: receipt)
        let fm = FileManager.default
        guard fm.isExecutableFile(atPath: runtime.nodeExecutable.path), fm.fileExists(atPath: runtime.dshEntry.path) else { return nil }
        return runtime
    }

    /// Apply the launch-time install decision and return the runtime to run.
    public func ensureRuntime(progress: (String) -> Void = { _ in }) throws -> InstalledRuntime {
        let bundled = bundledManifest()
        let current = currentRuntime()
        let decision = InstallDecision.decide(bundled: bundled, installed: current?.receipt, installedUsable: current != nil)
        logger.log("runtime decision=\(decision) installed=\(current?.receipt.dshVersion ?? "none") bundled=\(bundled?.dshVersion ?? "none")")
        switch decision {
        case .reuseInstalled, .keepInstalledNewer:
            if let current { return current }
            throw CommandError("runtime disappeared while resolving the install decision")
        case .installBundled:
            progress("installing")
            return try installBundled()
        case .noRuntime:
            throw CommandError("this build carries no runtime payload and no runtime is installed; set runtime in config.json")
        }
    }

    /// Extract the bundled payload unconditionally (also used by "Repair Runtime").
    public func installBundled() throws -> InstalledRuntime {
        guard let payloadDirectory, let manifest = bundledManifest() else {
            throw CommandError("no bundled runtime payload")
        }
        let archive = payloadDirectory.appendingPathComponent(manifest.archive)
        let digest = try Self.sha256(of: archive)
        guard digest == manifest.archiveSHA256 else {
            throw CommandError("payload checksum mismatch: expected \(manifest.archiveSHA256), got \(digest)")
        }
        let fm = FileManager.default
        try paths.prepare()
        let staging = paths.runtimeRoot.appendingPathComponent(".staging-\(UUID().uuidString)", isDirectory: true)
        try fm.createDirectory(at: staging, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        defer { try? fm.removeItem(at: staging) }

        let started = Date()
        try CommandRunner.check("/usr/bin/aa", ["extract", "-d", staging.path, "-i", archive.path], timeout: 600)
        // The payload is sealed by the app signature and verified above; downloaded
        // app bundles must not leak quarantine onto extracted native modules.
        _ = try? CommandRunner.run("/usr/bin/xattr", ["-dr", "com.apple.quarantine", staging.path], timeout: 120)
        try verify(runtimeAt: staging, dshVersion: manifest.dshVersion, nodeVersion: manifest.nodeVersion)

        let receipt = RuntimeReceipt(
            identity: manifest.identity,
            dshVersion: manifest.dshVersion,
            nodeVersion: manifest.nodeVersion,
            pnpmVersion: manifest.pnpmVersion,
            arch: manifest.arch,
            source: .bundle,
            installedAt: ISO8601DateFormatter().string(from: Date())
        )
        try writeReceipt(receipt, in: staging)
        let installed = try activate(staging: staging, directoryName: manifest.directoryName, receipt: receipt)
        logger.log(String(format: "runtime installed dsh=%@ node=%@ dir=%@ in %.1fs",
                          manifest.dshVersion, manifest.nodeVersion, manifest.directoryName, Date().timeIntervalSince(started)))
        return installed
    }

    /// Move a verified staging tree into place, flip `current`, and prune old runtimes.
    public func activate(staging: URL, directoryName: String, receipt: RuntimeReceipt) throws -> InstalledRuntime {
        let fm = FileManager.default
        let previous = try? fm.destinationOfSymbolicLink(atPath: paths.currentRuntimeLink.path)
        let final = paths.runtimeRoot.appendingPathComponent(directoryName, isDirectory: true)
        if fm.fileExists(atPath: final.path) {
            let parked = paths.runtimeRoot.appendingPathComponent(".replaced-\(UUID().uuidString)", isDirectory: true)
            try fm.moveItem(at: final, to: parked)
            try? fm.removeItem(at: parked)
        }
        try fm.moveItem(at: staging, to: final)

        // rename(2) over the old link is atomic; readers never observe a missing `current`.
        let tempLink = paths.runtimeRoot.appendingPathComponent(".current-\(UUID().uuidString)")
        try fm.createSymbolicLink(atPath: tempLink.path, withDestinationPath: directoryName)
        guard rename(tempLink.path, paths.currentRuntimeLink.path) == 0 else {
            let reason = String(cString: strerror(errno))
            try? fm.removeItem(at: tempLink)
            throw CommandError("could not switch the current runtime: \(reason)")
        }
        prune(keeping: Set([directoryName, previous].compactMap { $0 }))
        return InstalledRuntime(directory: final, receipt: receipt)
    }

    /// Check the extracted tree runs the expected Node and carries the expected dsh.
    public func verify(runtimeAt dir: URL, dshVersion: String, nodeVersion: String) throws {
        let node = InstalledRuntime.nodeExecutable(in: dir)
        let result = try CommandRunner.check(node.path, ["--version"], environment: ["PATH": "/usr/bin:/bin"], timeout: 30)
        let reported = result.stdoutText.trimmingCharacters(in: .whitespacesAndNewlines)
        guard reported == "v\(nodeVersion)" else {
            throw CommandError("runtime Node reports \(reported), expected v\(nodeVersion)")
        }
        let manifestData = try Data(contentsOf: InstalledRuntime.dshManifest(in: dir))
        let object = try JSONSerialization.jsonObject(with: manifestData) as? [String: Any]
        guard let version = object?["version"] as? String, version == dshVersion else {
            throw CommandError("runtime carries @deepseek-ai/dsh \(object?["version"] as? String ?? "?"), expected \(dshVersion)")
        }
        guard FileManager.default.fileExists(atPath: InstalledRuntime.dshEntry(in: dir).path) else {
            throw CommandError("runtime is missing the dsh CLI entry")
        }
    }

    func readReceipt(in dir: URL) -> RuntimeReceipt? {
        guard let data = try? Data(contentsOf: dir.appendingPathComponent("receipt.json")) else { return nil }
        return try? JSONDecoder().decode(RuntimeReceipt.self, from: data)
    }

    func writeReceipt(_ receipt: RuntimeReceipt, in dir: URL) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(receipt).write(to: dir.appendingPathComponent("receipt.json"), options: .atomic)
    }

    private func prune(keeping: Set<String>) {
        let fm = FileManager.default
        guard let entries = try? fm.contentsOfDirectory(atPath: paths.runtimeRoot.path) else { return }
        for name in entries where name != "current" && !keeping.contains(name) {
            // Leftover staging trees from an interrupted install are removed too.
            do {
                try fm.removeItem(at: paths.runtimeRoot.appendingPathComponent(name))
                logger.log("runtime pruned \(name)")
            } catch {
                logger.log("runtime prune failed \(name): \(error.localizedDescription)")
            }
        }
    }

    public static func sha256(of url: URL) throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        var hasher = SHA256()
        while let chunk = try handle.read(upToCount: 4 * 1024 * 1024), !chunk.isEmpty {
            hasher.update(data: chunk)
        }
        return hasher.finalize().map { String(format: "%02x", $0) }.joined()
    }
}
