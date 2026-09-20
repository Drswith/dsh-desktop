//! The payload description written by `scripts/prepare-payload.sh` and the
//! receipt an installed runtime carries.

use serde::{Deserialize, Serialize};

use crate::core::semver::SemVer;

/// `payload/manifest.json`, next to the runtime archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeManifest {
    pub schema_version: u32,
    pub dsh_version: String,
    pub node_version: String,
    pub pnpm_version: String,
    #[serde(default = "darwin")]
    pub platform: String,
    pub arch: String,
    /// Apple Archive file name relative to the manifest (`runtime.aar`).
    #[serde(default = "default_archive")]
    pub archive: String,
    // The payload script writes the acronym in caps; keep the key byte for byte.
    #[serde(rename = "archiveSHA256")]
    pub archive_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

fn darwin() -> String {
    "darwin".to_owned()
}

fn default_archive() -> String {
    "runtime.aar".to_owned()
}

impl RuntimeManifest {
    /// Stable identity of this exact payload; equal identities never reinstall.
    pub fn identity(&self) -> String {
        format!(
            "dsh={};node={};pnpm={};arch={};sha256={}",
            self.dsh_version, self.node_version, self.pnpm_version, self.arch, self.archive_sha256
        )
    }

    /// Directory name under `runtime/` for this payload.
    pub fn directory_name(&self) -> String {
        format!(
            "dsh-{}-node-{}-{}-{}",
            self.dsh_version,
            self.node_version,
            self.arch,
            &self.archive_sha256[..self.archive_sha256.len().min(8)]
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReceiptSource {
    /// Extracted from the app bundle payload.
    Bundle,
    /// Installed later from the npm registry by the in-app updater.
    Registry,
}

/// `receipt.json` inside an installed runtime directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeReceipt {
    pub schema_version: u32,
    pub identity: String,
    pub dsh_version: String,
    pub node_version: String,
    pub pnpm_version: String,
    pub arch: String,
    pub source: ReceiptSource,
    pub installed_at: String,
}

impl RuntimeReceipt {
    pub fn from_manifest(manifest: &RuntimeManifest, source: ReceiptSource) -> Self {
        RuntimeReceipt {
            schema_version: 1,
            identity: manifest.identity(),
            dsh_version: manifest.dsh_version.clone(),
            node_version: manifest.node_version.clone(),
            pnpm_version: manifest.pnpm_version.clone(),
            arch: manifest.arch.clone(),
            source,
            installed_at: crate::core::iso8601_now(),
        }
    }
}

/// What to do with the bundled payload on launch: install it, reuse the installed
/// runtime, or keep an installed runtime that is newer than the bundled one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallDecision {
    ReuseInstalled,
    KeepInstalledNewer,
    InstallBundled { reason: &'static str },
    NoRuntime,
}

impl InstallDecision {
    pub fn decide(
        bundled: Option<&RuntimeManifest>,
        installed: Option<&RuntimeReceipt>,
        installed_usable: bool,
    ) -> InstallDecision {
        let Some(bundled) = bundled else {
            return if installed.is_some() && installed_usable {
                InstallDecision::ReuseInstalled
            } else {
                InstallDecision::NoRuntime
            };
        };
        let installed = match installed {
            Some(installed) if installed_usable => installed,
            other => {
                return InstallDecision::InstallBundled {
                    reason: if other.is_none() {
                        "not_installed"
                    } else {
                        "installed_unusable"
                    },
                }
            }
        };
        if installed.identity == bundled.identity() {
            return InstallDecision::ReuseInstalled;
        }
        if installed.arch != bundled.arch {
            return InstallDecision::InstallBundled {
                reason: "arch_changed",
            };
        }
        let (Some(current), Some(candidate)) = (
            SemVer::parse(&installed.dsh_version),
            SemVer::parse(&bundled.dsh_version),
        ) else {
            return InstallDecision::InstallBundled {
                reason: "unparsable_version",
            };
        };
        if current > candidate {
            InstallDecision::KeepInstalledNewer
        } else if current < candidate {
            InstallDecision::InstallBundled { reason: "upgrade" }
        } else {
            InstallDecision::InstallBundled {
                reason: "payload_changed",
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            InstallDecision::ReuseInstalled => "reuseInstalled".to_owned(),
            InstallDecision::KeepInstalledNewer => "keepInstalledNewer".to_owned(),
            InstallDecision::InstallBundled { reason } => format!("installBundled({reason})"),
            InstallDecision::NoRuntime => "noRuntime".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str, sha: &str) -> RuntimeManifest {
        RuntimeManifest {
            schema_version: 1,
            dsh_version: version.to_owned(),
            node_version: "24.17.0".to_owned(),
            pnpm_version: "11.7.0".to_owned(),
            platform: "darwin".to_owned(),
            arch: "arm64".to_owned(),
            archive: "runtime.aar".to_owned(),
            archive_sha256: sha.to_owned(),
            created_at: None,
        }
    }

    fn receipt(manifest: &RuntimeManifest, source: ReceiptSource) -> RuntimeReceipt {
        RuntimeReceipt::from_manifest(manifest, source)
    }

    #[test]
    fn decisions() {
        let bundled = manifest("0.1.5-rc.2", "aaaa");
        let installed = receipt(&bundled, ReceiptSource::Bundle);
        assert_eq!(
            InstallDecision::decide(Some(&bundled), None, false),
            InstallDecision::InstallBundled {
                reason: "not_installed"
            }
        );
        assert_eq!(
            InstallDecision::decide(Some(&bundled), Some(&installed), true),
            InstallDecision::ReuseInstalled
        );
        assert_eq!(
            InstallDecision::decide(Some(&bundled), Some(&installed), false),
            InstallDecision::InstallBundled {
                reason: "installed_unusable"
            }
        );
        let older = receipt(&manifest("0.1.5-rc.1", "aaaa"), ReceiptSource::Bundle);
        assert_eq!(
            InstallDecision::decide(Some(&bundled), Some(&older), true),
            InstallDecision::InstallBundled { reason: "upgrade" }
        );
        let newer = receipt(&manifest("0.1.6-alpha.2", "aaaa"), ReceiptSource::Registry);
        assert_eq!(
            InstallDecision::decide(Some(&bundled), Some(&newer), true),
            InstallDecision::KeepInstalledNewer
        );
        let repacked = receipt(&manifest("0.1.5-rc.2", "bbbb"), ReceiptSource::Bundle);
        assert_eq!(
            InstallDecision::decide(Some(&bundled), Some(&repacked), true),
            InstallDecision::InstallBundled {
                reason: "payload_changed"
            }
        );
        assert_eq!(
            InstallDecision::decide(None, Some(&installed), true),
            InstallDecision::ReuseInstalled
        );
        assert_eq!(
            InstallDecision::decide(None, None, false),
            InstallDecision::NoRuntime
        );
    }

    #[test]
    fn manifest_json_uses_the_shape_the_payload_script_writes() {
        let written = r#"{"schemaVersion":1,"dshVersion":"0.1.5-rc.2","nodeVersion":"24.17.0",
            "pnpmVersion":"11.7.0","platform":"darwin","arch":"arm64","archive":"runtime.aar",
            "archiveSHA256":"abcdef0123456789","createdAt":"2026-09-19T05:05:24Z"}"#;
        let manifest: RuntimeManifest = serde_json::from_str(written).unwrap();
        assert_eq!(manifest.dsh_version, "0.1.5-rc.2");
        assert_eq!(manifest.archive_sha256, "abcdef0123456789");
        assert_eq!(
            manifest.directory_name(),
            "dsh-0.1.5-rc.2-node-24.17.0-arm64-abcdef01"
        );
    }
}
