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

/// `run/runtime-activation.json`: a switch to a new runtime, on probation until the
/// service starts on it.
public struct RuntimeActivation: Codable, Equatable, Sendable {
    public var directoryName: String
    public var identity: String
    public var dshVersion: String
    /// The runtime switched away from, restored if the new one cannot start.
    public var replaced: String?
    public var replacedPrevious: String?
    public var startedAt: String
}

/// Installs runtimes into `~/.dsh-launcher/runtime/<id>` and flips the `current`
/// symlink atomically: the bundled payload, and downloaded updates staged as
/// `pending` until they are switched to on probation. The runtime an upgrade
/// replaces stays behind as `previous`, at most one, unless `keepsPreviousRuntime` is off.
public final class RuntimeInstaller: @unchecked Sendable {
    private let paths: AppPaths
    private let logger: FileLogger
    private let payloadDirectory: URL?
    /// Keep the runtime an upgrade replaces as `previous`; off keeps `current` alone.
    public var keepsPreviousRuntime = true

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
        linkTarget(paths.currentRuntimeLink).flatMap(runtime(named:))
    }

    /// The runtime directory `name` under `runtime/`, when its receipt and entry points exist.
    private func runtime(named name: String) -> InstalledRuntime? {
        let dir = paths.runtimeRoot.appendingPathComponent(name, isDirectory: true)
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
        let staged = try stage(archive: payloadDirectory.appendingPathComponent(manifest.archive), manifest: manifest, source: .bundle)
        return try activate(staged)
    }

    /// Check `archive` against `manifest`, extract and verify it, and move it into
    /// `runtime/<directoryName>` without switching to it.
    public func stage(archive: URL, manifest: RuntimeManifest, source: RuntimeReceipt.Source) throws -> InstalledRuntime {
        let digest = try Self.sha256(of: archive)
        guard digest == manifest.archiveSHA256 else {
            throw CommandError("runtime archive checksum mismatch: expected \(manifest.archiveSHA256), got \(digest)")
        }
        let fm = FileManager.default
        try paths.prepare()
        let staging = paths.runtimeRoot.appendingPathComponent(".staging-\(UUID().uuidString)", isDirectory: true)
        try fm.createDirectory(at: staging, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        defer { try? fm.removeItem(at: staging) }

        let started = Date()
        try CommandRunner.check("/usr/bin/aa", ["extract", "-d", staging.path, "-i", archive.path], timeout: 600)
        // The archive is verified above; neither a downloaded app bundle nor a
        // download may leak quarantine onto the extracted native modules.
        _ = try? CommandRunner.run("/usr/bin/xattr", ["-dr", "com.apple.quarantine", staging.path], timeout: 120)
        try verify(runtimeAt: staging, dshVersion: manifest.dshVersion, nodeVersion: manifest.nodeVersion)

        let receipt = RuntimeReceipt(
            identity: manifest.identity,
            dshVersion: manifest.dshVersion,
            nodeVersion: manifest.nodeVersion,
            pnpmVersion: manifest.pnpmVersion,
            arch: manifest.arch,
            source: source,
            installedAt: ISO8601DateFormatter().string(from: Date())
        )
        try writeReceipt(receipt, in: staging)
        let final = paths.runtimeRoot.appendingPathComponent(manifest.directoryName, isDirectory: true)
        if fm.fileExists(atPath: final.path) {
            let parked = paths.runtimeRoot.appendingPathComponent(".replaced-\(UUID().uuidString)", isDirectory: true)
            try fm.moveItem(at: final, to: parked)
            try? fm.removeItem(at: parked)
        }
        try fm.moveItem(at: staging, to: final)
        logger.log(String(format: "runtime staged dsh=%@ node=%@ source=%@ dir=%@ in %.1fs", manifest.dshVersion,
                          manifest.nodeVersion, source.rawValue, manifest.directoryName, Date().timeIntervalSince(started)))
        return InstalledRuntime(directory: final, receipt: receipt)
    }

    /// Switch to a staged runtime and prune old ones, ending any switch on probation.
    /// The usable runtime it replaces becomes `previous`; reinstalling the current
    /// runtime keeps the existing `previous`. Everything else except `pending` goes.
    @discardableResult
    public func activate(_ runtime: InstalledRuntime) throws -> InstalledRuntime {
        let name = runtime.directory.lastPathComponent
        let usable = { (other: String) in other == name ? nil : self.runtime(named: other)?.directory.lastPathComponent }
        let replaced = linkTarget(paths.currentRuntimeLink).flatMap(usable)
        let kept = linkTarget(paths.previousRuntimeLink).flatMap(usable)
        try setLink(paths.currentRuntimeLink, to: name)
        try? FileManager.default.removeItem(at: paths.runtimeActivationFile)
        if linkTarget(paths.pendingRuntimeLink) == name { try? setLink(paths.pendingRuntimeLink, to: nil) }

        let previous = keepsPreviousRuntime ? (replaced ?? kept) : nil
        try? setLink(paths.previousRuntimeLink, to: previous)
        prune(keeping: Set([name, previous].compactMap { $0 }))
        return runtime
    }

    /// Apply a change of `keepsPreviousRuntime` right away: turning it off removes
    /// `previous`, unless a switch on probation may still need to go back to it.
    public func applyRetention() {
        guard !keepsPreviousRuntime, currentActivation() == nil, let current = linkTarget(paths.currentRuntimeLink) else { return }
        try? setLink(paths.previousRuntimeLink, to: nil)
        prune(keeping: [current])
    }

    // MARK: Updates

    /// The downloaded runtime waiting to be switched to.
    public func pendingRuntime() -> InstalledRuntime? {
        linkTarget(paths.pendingRuntimeLink).flatMap(runtime(named:))
    }

    /// Mark a staged runtime as the next one to switch to, or clear the mark. A
    /// pending runtime superseded here is removed unless it is in use.
    public func setPending(_ runtime: InstalledRuntime?) throws {
        let name = runtime?.directory.lastPathComponent
        let superseded = linkTarget(paths.pendingRuntimeLink)
        try setLink(paths.pendingRuntimeLink, to: name)
        guard let superseded, superseded != name,
              ![linkTarget(paths.currentRuntimeLink), linkTarget(paths.previousRuntimeLink)].contains(superseded) else { return }
        try? FileManager.default.removeItem(at: paths.runtimeRoot.appendingPathComponent(superseded, isDirectory: true))
        logger.log("runtime pruned \(superseded)")
    }

    /// Switch to the pending runtime on probation: `current` moves to it and
    /// `previous` to the runtime it replaces, and nothing is pruned until the
    /// service starts on it (`confirmActivation`) or cannot (`rollBackActivation`).
    /// A pending runtime older than the bundled one is dropped instead.
    public func beginPendingActivation() throws -> RuntimeActivation? {
        guard let pending = pendingRuntime() else { return nil }
        if let bundled = bundledManifest().flatMap({ SemVer($0.dshVersion) }),
           let waiting = SemVer(pending.receipt.dshVersion), bundled > waiting {
            logger.log("dropping pending runtime dsh=\(waiting): the bundled dsh=\(bundled) is newer")
            try? setPending(nil)
            return nil
        }
        let name = pending.directory.lastPathComponent
        let activation = RuntimeActivation(
            directoryName: name,
            identity: pending.receipt.identity,
            dshVersion: pending.receipt.dshVersion,
            replaced: linkTarget(paths.currentRuntimeLink),
            replacedPrevious: linkTarget(paths.previousRuntimeLink),
            startedAt: ISO8601DateFormatter().string(from: Date())
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(activation).write(to: paths.runtimeActivationFile, options: .atomic)
        try setLink(paths.currentRuntimeLink, to: name)
        try? setLink(paths.previousRuntimeLink, to: activation.replaced)
        try? setLink(paths.pendingRuntimeLink, to: nil)
        logger.log("runtime switching to dsh=\(activation.dshVersion) on probation, replacing \(activation.replaced ?? "nothing")")
        return activation
    }

    /// The switch waiting for the service to start on its runtime, if any.
    public func currentActivation() -> RuntimeActivation? {
        guard let data = try? Data(contentsOf: paths.runtimeActivationFile) else { return nil }
        return try? JSONDecoder().decode(RuntimeActivation.self, from: data)
    }

    /// The service started on the new runtime: keep it and apply the retention setting.
    public func confirmActivation() {
        guard let activation = currentActivation() else { return }
        try? FileManager.default.removeItem(at: paths.runtimeActivationFile)
        let previous = keepsPreviousRuntime ? activation.replaced.flatMap { runtime(named: $0)?.directory.lastPathComponent } : nil
        try? setLink(paths.previousRuntimeLink, to: previous)
        prune(keeping: Set([activation.directoryName, previous].compactMap { $0 }))
        logger.log("runtime dsh=\(activation.dshVersion) confirmed")
    }

    /// The service could not start on the new runtime: go back to the runtime it
    /// replaced and remove the failed one. Nil when nothing usable is left to go back to.
    public func rollBackActivation() -> InstalledRuntime? {
        guard let activation = currentActivation() else { return nil }
        try? FileManager.default.removeItem(at: paths.runtimeActivationFile)
        guard let replacedName = activation.replaced, let replaced = runtime(named: replacedName) else {
            logger.log("runtime dsh=\(activation.dshVersion) failed to start and no earlier runtime is left to go back to")
            return nil
        }
        do {
            try setLink(paths.currentRuntimeLink, to: replacedName)
        } catch {
            logger.log("runtime rollback failed: \(error.localizedDescription)")
            return nil
        }
        try? setLink(paths.previousRuntimeLink, to: activation.replacedPrevious.flatMap { runtime(named: $0)?.directory.lastPathComponent })
        if activation.directoryName != replacedName {
            try? FileManager.default.removeItem(at: paths.runtimeRoot.appendingPathComponent(activation.directoryName, isDirectory: true))
        }
        logger.log("runtime dsh=\(activation.dshVersion) failed to start; back on dsh=\(replaced.receipt.dshVersion)")
        return replaced
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

    private func linkTarget(_ link: URL) -> String? {
        try? FileManager.default.destinationOfSymbolicLink(atPath: link.path)
    }

    /// Point `link` at `target`, or remove it for nil. rename(2) over the old link is
    /// atomic, so readers never observe a missing `current`.
    private func setLink(_ link: URL, to target: String?) throws {
        let fm = FileManager.default
        guard let target else {
            if linkTarget(link) != nil { try fm.removeItem(at: link) }
            return
        }
        let tempLink = paths.runtimeRoot.appendingPathComponent(".link-\(UUID().uuidString)")
        try fm.createSymbolicLink(atPath: tempLink.path, withDestinationPath: target)
        guard rename(tempLink.path, link.path) == 0 else {
            let reason = String(cString: strerror(errno))
            try? fm.removeItem(at: tempLink)
            throw CommandError("could not point runtime/\(link.lastPathComponent) at \(target): \(reason)")
        }
    }

    /// Remove every runtime directory except `keeping`, the pending runtime, and those
    /// a switch on probation may go back to.
    private func prune(keeping: Set<String>) {
        let fm = FileManager.default
        guard let entries = try? fm.contentsOfDirectory(atPath: paths.runtimeRoot.path) else { return }
        let links = [paths.currentRuntimeLink, paths.previousRuntimeLink, paths.pendingRuntimeLink].map(\.lastPathComponent)
        let activation = currentActivation()
        let protected = keeping.union([linkTarget(paths.pendingRuntimeLink), activation?.directoryName,
                                       activation?.replaced, activation?.replacedPrevious].compactMap { $0 })
        for name in entries where !links.contains(name) && !protected.contains(name) {
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
