import Foundation

/// `run/update-state.json`: update bookkeeping that outlives one launch.
public struct UpdateState: Codable, Equatable, Sendable {
    /// Runtimes that failed their start confirmation; the updater never offers them again.
    public var failedIdentities: [String] = []

    public init() {}

    public static func load(from url: URL) -> UpdateState {
        guard let data = try? Data(contentsOf: url) else { return UpdateState() }
        return (try? JSONDecoder().decode(UpdateState.self, from: data)) ?? UpdateState()
    }

    public func save(to url: URL) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        try encoder.encode(self).write(to: url, options: .atomic)
    }
}

/// What a channel's runtime release means for this launcher right now.
public enum RuntimeUpdateDecision: Equatable, Sendable {
    /// Download `artifact` and stage it as described by `manifest`.
    case stage(RuntimeManifest, UpdateManifest.Artifact)
    /// This dsh version is already staged and waiting to be switched to.
    case staged(String)
    /// The current runtime is at least as new as the channel's.
    case upToDate(String)
    /// The channel offers nothing this launcher takes, for the given reason.
    case skip(String)

    /// Only newer runtimes are taken: switching to a channel with an older release
    /// never downgrades, since the newer dsh may already have migrated its data.
    public static func decide(runtime: UpdateManifest.Runtime?, arch: String, shellVersion: String,
                              current: RuntimeReceipt?, pending: RuntimeReceipt?, failedIdentities: Set<String>) -> RuntimeUpdateDecision {
        guard let runtime else { return .skip("the channel lists no runtime") }
        guard let offered = SemVer(runtime.dshVersion), let minimum = SemVer(runtime.minShellVersion) else {
            return .skip("unreadable versions in the channel manifest")
        }
        guard let artifact = runtime.archives[arch] else { return .skip("no \(arch) runtime for dsh \(runtime.dshVersion)") }
        if let shell = SemVer(shellVersion), shell < minimum {
            return .skip("dsh \(runtime.dshVersion) needs DSH Launcher \(runtime.minShellVersion) or later")
        }
        let manifest = RuntimeManifest(dshVersion: runtime.dshVersion, nodeVersion: runtime.nodeVersion,
                                       pnpmVersion: runtime.pnpmVersion, arch: arch,
                                       archive: artifact.url.lastPathComponent, archiveSHA256: artifact.sha256)
        if failedIdentities.contains(manifest.identity) {
            return .skip("dsh \(runtime.dshVersion) failed to start before")
        }
        if let current, let installed = SemVer(current.dshVersion), installed >= offered {
            return .upToDate(current.dshVersion)
        }
        if pending?.identity == manifest.identity { return .staged(runtime.dshVersion) }
        return .stage(manifest, artifact)
    }
}

/// The network half of runtime updates: read the channel manifest and download
/// the runtime it offers. Staging it is `RuntimeInstaller`'s job, on the queue
/// that owns installs.
public struct RuntimeUpdater: Sendable {
    public let feed: UpdateFeed
    public let arch: String
    public let shellVersion: String
    public let downloadsDirectory: URL

    public init(feed: UpdateFeed, arch: String, shellVersion: String, downloadsDirectory: URL) {
        self.feed = feed
        self.arch = arch
        self.shellVersion = shellVersion
        self.downloadsDirectory = downloadsDirectory
    }

    public func decide(channel: UpdateChannel, current: RuntimeReceipt?, pending: RuntimeReceipt?,
                       failedIdentities: Set<String>) throws -> RuntimeUpdateDecision {
        let manifest = try feed.fetch(channel)
        return RuntimeUpdateDecision.decide(runtime: manifest.runtime, arch: arch, shellVersion: shellVersion,
                                            current: current, pending: pending, failedIdentities: failedIdentities)
    }

    /// Download the archive described by `manifest`; returns the verified file.
    public func download(_ artifact: UpdateManifest.Artifact, for manifest: RuntimeManifest) throws -> URL {
        try FileManager.default.createDirectory(at: downloadsDirectory, withIntermediateDirectories: true,
                                                attributes: [.posixPermissions: 0o700])
        let destination = downloadsDirectory.appendingPathComponent(manifest.archive)
        try HTTPTransfer.download(artifact.url, to: destination, expectedSize: artifact.size, sha256: artifact.sha256)
        return destination
    }
}
