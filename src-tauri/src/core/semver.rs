//! Semantic version with SemVer 2.0 precedence (build metadata ignored).

use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, Eq)]
pub struct SemVer {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub prerelease: Vec<String>,
}

impl SemVer {
    pub fn parse(text: &str) -> Option<Self> {
        let mut raw = text.trim();
        if let Some(rest) = raw.strip_prefix('v') {
            raw = rest;
        }
        if let Some(plus) = raw.find('+') {
            raw = &raw[..plus];
        }
        let (core, prerelease) = match raw.find('-') {
            Some(dash) => {
                let parts: Vec<String> = raw[dash + 1..].split('.').map(str::to_owned).collect();
                if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
                    return None;
                }
                (&raw[..dash], parts)
            }
            None => (raw, Vec::new()),
        };
        let numbers: Vec<&str> = core.split('.').collect();
        if numbers.len() != 3 {
            return None;
        }
        let mut parsed = [0u64; 3];
        for (slot, text) in parsed.iter_mut().zip(numbers) {
            // `u64` rejects the signs and the trailing garbage `Int(_:)` also rejects.
            if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            *slot = text.parse().ok()?;
        }
        Some(SemVer {
            major: parsed[0],
            minor: parsed[1],
            patch: parsed[2],
            prerelease,
        })
    }
}

impl fmt::Display for SemVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.prerelease.is_empty() {
            write!(f, "-{}", self.prerelease.join("."))?;
        }
        Ok(())
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        let core = (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch));
        if core != Ordering::Equal {
            return core;
        }
        // A release outranks any of its prereleases.
        match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            (false, false) => {}
        }
        for (left, right) in self.prerelease.iter().zip(&other.prerelease) {
            if left == right {
                continue;
            }
            return match (left.parse::<u64>(), right.parse::<u64>()) {
                (Ok(left), Ok(right)) => left.cmp(&right),
                // Numeric identifiers sort before alphanumeric ones.
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => left.cmp(right),
            };
        }
        self.prerelease.len().cmp(&other.prerelease.len())
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for SemVer {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_and_prerelease() {
        let version = SemVer::parse("v0.1.5-rc.2+build.7").unwrap();
        assert_eq!((version.major, version.minor, version.patch), (0, 1, 5));
        assert_eq!(version.prerelease, ["rc", "2"]);
        assert_eq!(version.to_string(), "0.1.5-rc.2");
        assert!(SemVer::parse("1.2").is_none());
        assert!(SemVer::parse("1.2.x").is_none());
        assert!(SemVer::parse("1.2.3-").is_none());
    }

    #[test]
    fn precedence_follows_semver() {
        let ordered = [
            "0.1.2-alpha.3",
            "0.1.5-alpha.1",
            "0.1.5-alpha.2",
            "0.1.5-rc.1",
            "0.1.5-rc.2",
            "0.1.5",
            "0.1.6-alpha.1",
            "0.2.0",
        ];
        let parsed: Vec<SemVer> = ordered.iter().map(|text| SemVer::parse(text).unwrap()).collect();
        for pair in parsed.windows(2) {
            assert!(pair[0] < pair[1], "{} < {}", pair[0], pair[1]);
        }
        assert!(SemVer::parse("1.0.0-2").unwrap() < SemVer::parse("1.0.0-alpha").unwrap());
        assert!(SemVer::parse("1.0.0-alpha").unwrap() < SemVer::parse("1.0.0-alpha.1").unwrap());
        assert_eq!(SemVer::parse("1.0.0+a"), SemVer::parse("1.0.0+b"));
    }
}
