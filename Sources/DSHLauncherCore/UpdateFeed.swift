import CryptoKit
import Foundation

/// Release channels. Stable follows npm `latest`; preview the newest of `latest`,
/// `next` and `alpha`.
public enum UpdateChannel: String, Codable, CaseIterable, Sendable {
    case stable
    case preview
}

/// `<channel>.json` in the update feed. Nothing in it is trusted until `UpdateFeed`
/// has verified its detached Ed25519 signature, `<channel>.json.sig`.
public struct UpdateManifest: Codable, Equatable, Sendable {
    public struct Artifact: Codable, Equatable, Sendable {
        public var url: URL
        public var sha256: String
        public var size: Int

        public init(url: URL, sha256: String, size: Int) {
            self.url = url
            self.sha256 = sha256
            self.size = size
        }
    }

    /// A runtime release; `archives` maps an architecture (`arm64`, `x86_64`) to its `runtime.aar`.
    public struct Runtime: Codable, Equatable, Sendable {
        public var dshVersion: String
        public var nodeVersion: String
        public var pnpmVersion: String
        /// The oldest DSH Launcher that can run this runtime.
        public var minShellVersion: String
        public var archives: [String: Artifact]

        public init(dshVersion: String, nodeVersion: String, pnpmVersion: String, minShellVersion: String, archives: [String: Artifact]) {
            self.dshVersion = dshVersion
            self.nodeVersion = nodeVersion
            self.pnpmVersion = pnpmVersion
            self.minShellVersion = minShellVersion
            self.archives = archives
        }
    }

    /// A DSH Launcher release; `archives` maps an architecture to the zipped app.
    public struct Shell: Codable, Equatable, Sendable {
        public var version: String
        public var build: Int
        public var minMacOS: String
        public var releaseNotesURL: URL?
        public var archives: [String: Artifact]
    }

    public var schemaVersion: Int
    public var channel: UpdateChannel
    public var publishedAt: String
    public var runtime: Runtime?
    public var shell: Shell?

    public init(schemaVersion: Int = 1, channel: UpdateChannel, publishedAt: String, runtime: Runtime? = nil, shell: Shell? = nil) {
        self.schemaVersion = schemaVersion
        self.channel = channel
        self.publishedAt = publishedAt
        self.runtime = runtime
        self.shell = shell
    }
}

/// The signed update feed: `<baseURL>/<channel>.json` plus its signature. The
/// launcher has no Developer ID to check downloads against, so the Ed25519 key
/// whose public half ships in the app is what every update is trusted by.
public struct UpdateFeed: Sendable {
    public let baseURL: URL
    private let publicKey: Data

    /// `publicKey` is the base64 of the raw 32-byte Ed25519 public key.
    public init?(baseURL: URL, publicKey: String) {
        guard let raw = Data(base64Encoded: publicKey.trimmingCharacters(in: .whitespacesAndNewlines)),
              (try? Curve25519.Signing.PublicKey(rawRepresentation: raw)) != nil else { return nil }
        self.baseURL = baseURL
        self.publicKey = raw
    }

    public func manifestURL(for channel: UpdateChannel) -> URL {
        baseURL.appendingPathComponent("\(channel.rawValue).json")
    }

    /// Download, verify and decode the manifest of `channel`.
    public func fetch(_ channel: UpdateChannel) throws -> UpdateManifest {
        let url = manifestURL(for: channel)
        let data = try HTTPTransfer.data(from: url)
        let signature = try HTTPTransfer.data(from: url.appendingPathExtension("sig"), limit: 4096)
        return try manifest(from: data, signature: signature, channel: channel)
    }

    /// Verify `signature` (base64 text) over `data`, then decode it as the manifest of `channel`.
    public func manifest(from data: Data, signature: Data, channel: UpdateChannel) throws -> UpdateManifest {
        let text = String(decoding: signature, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        guard let raw = Data(base64Encoded: text),
              let key = try? Curve25519.Signing.PublicKey(rawRepresentation: publicKey),
              key.isValidSignature(raw, for: data) else {
            throw CommandError("the \(channel.rawValue) update manifest has an invalid signature")
        }
        let manifest = try JSONDecoder().decode(UpdateManifest.self, from: data)
        guard manifest.schemaVersion == 1 else {
            throw CommandError("unsupported update manifest schema \(manifest.schemaVersion)")
        }
        // A validly signed manifest of another channel must not be served in its place.
        guard manifest.channel == channel else {
            throw CommandError("the \(channel.rawValue) feed served the \(manifest.channel.rawValue) manifest")
        }
        return manifest
    }
}
