import Foundation

/// `payload/manifest.json` written by `scripts/prepare-payload.sh` next to the runtime archive.
public struct RuntimeManifest: Codable, Equatable, Sendable {
    public var schemaVersion: Int
    public var dshVersion: String
    public var nodeVersion: String
    public var pnpmVersion: String
    public var platform: String
    public var arch: String
    /// Archive file name relative to the manifest (`runtime.tar.gz`).
    public var archive: String
    public var archiveSHA256: String
    public var createdAt: String?

    public init(schemaVersion: Int = 1, dshVersion: String, nodeVersion: String, pnpmVersion: String,
                platform: String = "darwin", arch: String, archive: String = "runtime.tar.gz",
                archiveSHA256: String, createdAt: String? = nil) {
        self.schemaVersion = schemaVersion
        self.dshVersion = dshVersion
        self.nodeVersion = nodeVersion
        self.pnpmVersion = pnpmVersion
        self.platform = platform
        self.arch = arch
        self.archive = archive
        self.archiveSHA256 = archiveSHA256
        self.createdAt = createdAt
    }

    /// Stable identity of this exact payload; equal identities never reinstall.
    public var identity: String {
        "dsh=\(dshVersion);node=\(nodeVersion);pnpm=\(pnpmVersion);arch=\(arch);sha256=\(archiveSHA256)"
    }

    /// Directory name under `runtime/` for this payload.
    public var directoryName: String {
        "dsh-\(dshVersion)-node-\(nodeVersion)-\(arch)-\(archiveSHA256.prefix(8))"
    }
}

/// `receipt.json` inside an installed runtime directory.
public struct RuntimeReceipt: Codable, Equatable, Sendable {
    public enum Source: String, Codable, Sendable {
        /// Extracted from the app bundle payload.
        case bundle
        /// Installed later from the npm registry by the in-app updater.
        case registry
    }

    public var schemaVersion: Int
    public var identity: String
    public var dshVersion: String
    public var nodeVersion: String
    public var pnpmVersion: String
    public var arch: String
    public var source: Source
    public var installedAt: String

    public init(schemaVersion: Int = 1, identity: String, dshVersion: String, nodeVersion: String,
                pnpmVersion: String, arch: String, source: Source, installedAt: String) {
        self.schemaVersion = schemaVersion
        self.identity = identity
        self.dshVersion = dshVersion
        self.nodeVersion = nodeVersion
        self.pnpmVersion = pnpmVersion
        self.arch = arch
        self.source = source
        self.installedAt = installedAt
    }
}

/// What to do with the bundled payload on launch: install it, reuse the installed
/// runtime, or keep an installed runtime that is newer than the bundled one.
public enum InstallDecision: Equatable, Sendable {
    case reuseInstalled
    case keepInstalledNewer
    case installBundled(reason: String)
    case noRuntime

    public static func decide(bundled: RuntimeManifest?, installed: RuntimeReceipt?, installedUsable: Bool) -> InstallDecision {
        guard let bundled else {
            return installed != nil && installedUsable ? .reuseInstalled : .noRuntime
        }
        guard let installed, installedUsable else {
            return .installBundled(reason: installed == nil ? "not_installed" : "installed_unusable")
        }
        if installed.identity == bundled.identity { return .reuseInstalled }
        if installed.arch != bundled.arch { return .installBundled(reason: "arch_changed") }
        guard let installedVersion = SemVer(installed.dshVersion), let bundledVersion = SemVer(bundled.dshVersion) else {
            return .installBundled(reason: "unparsable_version")
        }
        if installedVersion > bundledVersion { return .keepInstalledNewer }
        if installedVersion < bundledVersion { return .installBundled(reason: "upgrade") }
        return .installBundled(reason: "payload_changed")
    }
}
