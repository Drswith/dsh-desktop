import Foundation

/// Semantic version with SemVer 2.0 precedence (build metadata ignored).
public struct SemVer: Comparable, CustomStringConvertible, Sendable {
    public let major: Int
    public let minor: Int
    public let patch: Int
    public let prerelease: [String]

    public init?(_ text: String) {
        var raw = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if raw.hasPrefix("v") { raw.removeFirst() }
        if let plus = raw.firstIndex(of: "+") { raw = String(raw[..<plus]) }
        var core = raw
        var pre: [String] = []
        if let dash = raw.firstIndex(of: "-") {
            core = String(raw[..<dash])
            pre = raw[raw.index(after: dash)...].split(separator: ".", omittingEmptySubsequences: false).map(String.init)
            if pre.isEmpty || pre.contains(where: \.isEmpty) { return nil }
        }
        let parts = core.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 3,
              let major = Int(parts[0]), let minor = Int(parts[1]), let patch = Int(parts[2]),
              major >= 0, minor >= 0, patch >= 0 else { return nil }
        self.major = major
        self.minor = minor
        self.patch = patch
        self.prerelease = pre
    }

    public var description: String {
        let core = "\(major).\(minor).\(patch)"
        return prerelease.isEmpty ? core : core + "-" + prerelease.joined(separator: ".")
    }

    public static func < (lhs: SemVer, rhs: SemVer) -> Bool {
        if lhs.major != rhs.major { return lhs.major < rhs.major }
        if lhs.minor != rhs.minor { return lhs.minor < rhs.minor }
        if lhs.patch != rhs.patch { return lhs.patch < rhs.patch }
        // A release outranks any of its prereleases.
        if lhs.prerelease.isEmpty || rhs.prerelease.isEmpty {
            return !lhs.prerelease.isEmpty && rhs.prerelease.isEmpty
        }
        for (left, right) in zip(lhs.prerelease, rhs.prerelease) where left != right {
            switch (Int(left), Int(right)) {
            case let (l?, r?): return l < r
            case (_?, nil): return true // numeric identifiers sort before alphanumeric ones
            case (nil, _?): return false
            case (nil, nil): return left < right
            }
        }
        return lhs.prerelease.count < rhs.prerelease.count
    }

    public static func == (lhs: SemVer, rhs: SemVer) -> Bool {
        !(lhs < rhs) && !(rhs < lhs)
    }
}
