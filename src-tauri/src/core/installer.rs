//! Installs the bundled payload into `~/.dsh-launcher/runtime/<id>` and flips the
//! `current` symlink atomically. The runtime an upgrade replaces stays behind as
//! `previous`, at most one, unless `keeps_previous_runtime` is off.

use std::fs;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::core::command::CommandSpec;
use crate::core::logger::FileLogger;
use crate::core::manifest::{InstallDecision, ReceiptSource, RuntimeManifest, RuntimeReceipt};
use crate::core::paths::AppPaths;
use crate::core::{Error, Result};

/// One runtime directory: `node/`, `pnpm/`, `app/node_modules/@deepseek-ai/dsh`, `receipt.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledRuntime {
    pub directory: PathBuf,
    pub receipt: RuntimeReceipt,
}

impl InstalledRuntime {
    pub fn node_executable(&self) -> PathBuf {
        node_executable_in(&self.directory)
    }

    pub fn dsh_entry(&self) -> PathBuf {
        dsh_entry_in(&self.directory)
    }

    pub fn bin_directory(&self) -> PathBuf {
        self.directory.join("bin")
    }
}

pub fn node_executable_in(dir: &Path) -> PathBuf {
    dir.join("node/bin/node")
}

pub fn dsh_entry_in(dir: &Path) -> PathBuf {
    dir.join("app/node_modules/@deepseek-ai/dsh/lib/bin.js")
}

pub fn dsh_manifest_in(dir: &Path) -> PathBuf {
    dir.join("app/node_modules/@deepseek-ai/dsh/package.json")
}

pub struct RuntimeInstaller {
    paths: AppPaths,
    logger: Arc<FileLogger>,
    payload_directory: Option<PathBuf>,
    /// Keep the runtime an upgrade replaces as `previous`; off keeps `current` alone.
    pub keeps_previous_runtime: bool,
}

impl RuntimeInstaller {
    pub fn new(paths: AppPaths, logger: Arc<FileLogger>, payload_directory: Option<PathBuf>) -> Self {
        RuntimeInstaller {
            paths,
            logger,
            payload_directory,
            keeps_previous_runtime: true,
        }
    }

    pub fn bundled_manifest(&self) -> Option<RuntimeManifest> {
        let payload = self.payload_directory.as_ref()?;
        let data = fs::read(payload.join("manifest.json")).ok()?;
        serde_json::from_slice(&data).ok()
    }

    /// The runtime `current` points at, when its receipt and entry points exist.
    pub fn current_runtime(&self) -> Option<InstalledRuntime> {
        let name = link_target(&self.paths.current_runtime_link())?;
        self.runtime_named(&name)
    }

    /// The runtime directory `name` under `runtime/`, when its receipt and entry points exist.
    fn runtime_named(&self, name: &str) -> Option<InstalledRuntime> {
        let directory = self.paths.runtime_root().join(name);
        let receipt = read_receipt(&directory)?;
        let node = node_executable_in(&directory);
        let executable = fs::metadata(&node)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        if !executable || !dsh_entry_in(&directory).exists() {
            return None;
        }
        Some(InstalledRuntime { directory, receipt })
    }

    /// Apply the launch-time install decision and return the runtime to run.
    pub fn ensure_runtime(&self, mut progress: impl FnMut(&str)) -> Result<InstalledRuntime> {
        let bundled = self.bundled_manifest();
        let current = self.current_runtime();
        let decision = InstallDecision::decide(
            bundled.as_ref(),
            current.as_ref().map(|runtime| &runtime.receipt),
            current.is_some(),
        );
        self.logger.log(format!(
            "runtime decision={} installed={} bundled={}",
            decision.label(),
            current
                .as_ref()
                .map(|runtime| runtime.receipt.dsh_version.as_str())
                .unwrap_or("none"),
            bundled
                .as_ref()
                .map(|manifest| manifest.dsh_version.as_str())
                .unwrap_or("none"),
        ));
        match decision {
            InstallDecision::ReuseInstalled | InstallDecision::KeepInstalledNewer => current
                .ok_or_else(|| Error::new("runtime disappeared while resolving the install decision")),
            InstallDecision::InstallBundled { .. } => {
                progress("installing");
                self.install_bundled()
            }
            InstallDecision::NoRuntime => Err(Error::new(
                "this build carries no runtime payload and no runtime is installed; set runtime in config.json",
            )),
        }
    }

    /// Extract the bundled payload unconditionally (also used by "Reinstall Bundled Runtime").
    pub fn install_bundled(&self) -> Result<InstalledRuntime> {
        let (Some(payload), Some(manifest)) = (self.payload_directory.as_ref(), self.bundled_manifest())
        else {
            return Err(Error::new("no bundled runtime payload"));
        };
        let archive = payload.join(&manifest.archive);
        let digest = sha256(&archive)?;
        if digest != manifest.archive_sha256 {
            return Err(Error::new(format!(
                "payload checksum mismatch: expected {}, got {digest}",
                manifest.archive_sha256
            )));
        }
        self.paths.prepare()?;
        let staging = self
            .paths
            .runtime_root()
            .join(format!(".staging-{}", unique_suffix()));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&staging)?;
        let started = Instant::now();
        let outcome = self.extract_and_activate(&archive, &staging, &manifest);
        let _ = fs::remove_dir_all(&staging);
        let installed = outcome?;
        self.logger.log(format!(
            "runtime installed dsh={} node={} dir={} in {:.1}s",
            manifest.dsh_version,
            manifest.node_version,
            manifest.directory_name(),
            started.elapsed().as_secs_f64()
        ));
        Ok(installed)
    }

    fn extract_and_activate(
        &self,
        archive: &Path,
        staging: &Path,
        manifest: &RuntimeManifest,
    ) -> Result<InstalledRuntime> {
        CommandSpec::new(
            "/usr/bin/aa",
            [
                "extract",
                "-d",
                &staging.to_string_lossy(),
                "-i",
                &archive.to_string_lossy(),
            ],
        )
        .timeout(Duration::from_secs(600))
        .check()?;
        // The payload is sealed by the app signature and verified above; downloaded
        // app bundles must not leak quarantine onto extracted native modules.
        let _ = CommandSpec::new(
            "/usr/bin/xattr",
            ["-dr", "com.apple.quarantine", &staging.to_string_lossy()],
        )
        .timeout(Duration::from_secs(120))
        .run();
        self.verify(staging, &manifest.dsh_version, &manifest.node_version)?;

        let receipt = RuntimeReceipt::from_manifest(manifest, ReceiptSource::Bundle);
        write_receipt(&receipt, staging)?;
        self.activate(staging, &manifest.directory_name(), receipt)
    }

    /// Move a verified staging tree into place, flip `current`, and prune old runtimes.
    /// The usable runtime this install replaces becomes `previous`; reinstalling the
    /// current runtime keeps the existing `previous`. Everything else is removed.
    pub fn activate(
        &self,
        staging: &Path,
        directory_name: &str,
        receipt: RuntimeReceipt,
    ) -> Result<InstalledRuntime> {
        let usable = |name: String| -> Option<String> {
            if name == directory_name {
                None
            } else {
                self.runtime_named(&name).map(|runtime| {
                    runtime
                        .directory
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
            }
        };
        let replaced = link_target(&self.paths.current_runtime_link()).and_then(usable);
        let kept = link_target(&self.paths.previous_runtime_link()).and_then(usable);
        let final_directory = self.paths.runtime_root().join(directory_name);
        if final_directory.exists() {
            let parked = self
                .paths
                .runtime_root()
                .join(format!(".replaced-{}", unique_suffix()));
            fs::rename(&final_directory, &parked)?;
            let _ = fs::remove_dir_all(&parked);
        }
        fs::rename(staging, &final_directory)?;
        self.set_link(&self.paths.current_runtime_link(), Some(directory_name))?;

        let previous = if self.keeps_previous_runtime {
            replaced.or(kept)
        } else {
            None
        };
        let _ = self.set_link(&self.paths.previous_runtime_link(), previous.as_deref());
        let mut keeping = vec![directory_name.to_owned()];
        keeping.extend(previous);
        self.prune(&keeping);
        Ok(InstalledRuntime {
            directory: final_directory,
            receipt,
        })
    }

    /// Apply a change of `keeps_previous_runtime` right away: turning it off removes `previous`.
    pub fn apply_retention(&self) {
        if self.keeps_previous_runtime {
            return;
        }
        let Some(current) = link_target(&self.paths.current_runtime_link()) else {
            return;
        };
        let _ = self.set_link(&self.paths.previous_runtime_link(), None);
        self.prune(&[current]);
    }

    /// Check the extracted tree runs the expected Node and carries the expected dsh.
    pub fn verify(&self, dir: &Path, dsh_version: &str, node_version: &str) -> Result<()> {
        let node = node_executable_in(dir);
        let result = CommandSpec::new(&node.to_string_lossy(), ["--version"])
            .environment(std::collections::HashMap::from([(
                "PATH".to_owned(),
                "/usr/bin:/bin".to_owned(),
            )]))
            .timeout(Duration::from_secs(30))
            .check()?;
        let reported = result.stdout_text().trim().to_owned();
        if reported != format!("v{node_version}") {
            return Err(Error::new(format!(
                "runtime Node reports {reported}, expected v{node_version}"
            )));
        }
        let data = fs::read(dsh_manifest_in(dir)).map_err(|error| {
            Error::new(format!(
                "runtime carries no @deepseek-ai/dsh package.json: {error}"
            ))
        })?;
        let package: serde_json::Value = serde_json::from_slice(&data)?;
        let version = package.get("version").and_then(|value| value.as_str());
        if version != Some(dsh_version) {
            return Err(Error::new(format!(
                "runtime carries @deepseek-ai/dsh {}, expected {dsh_version}",
                version.unwrap_or("?")
            )));
        }
        if !dsh_entry_in(dir).exists() {
            return Err(Error::new("runtime is missing the dsh CLI entry"));
        }
        Ok(())
    }

    /// Point `link` at `target`, or remove it for `None`. rename(2) over the old link
    /// is atomic, so readers never observe a missing `current`.
    fn set_link(&self, link: &Path, target: Option<&str>) -> Result<()> {
        let Some(target) = target else {
            if link_target(link).is_some() {
                fs::remove_file(link)?;
            }
            return Ok(());
        };
        let temp = self
            .paths
            .runtime_root()
            .join(format!(".link-{}", unique_suffix()));
        std::os::unix::fs::symlink(target, &temp)?;
        if let Err(error) = fs::rename(&temp, link) {
            let _ = fs::remove_file(&temp);
            return Err(Error::new(format!(
                "could not point runtime/{} at {target}: {error}",
                link.file_name().unwrap_or_default().to_string_lossy()
            )));
        }
        Ok(())
    }

    fn prune(&self, keeping: &[String]) {
        let Ok(entries) = fs::read_dir(self.paths.runtime_root()) else {
            return;
        };
        let links = ["current", "previous"];
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if links.contains(&name.as_str()) || keeping.contains(&name) {
                continue;
            }
            // Leftover staging trees from an interrupted install are removed too.
            match fs::remove_dir_all(entry.path()) {
                Ok(()) => self.logger.log(format!("runtime pruned {name}")),
                Err(error) => self.logger.log(format!("runtime prune failed {name}: {error}")),
            }
        }
    }
}

pub fn read_receipt(dir: &Path) -> Option<RuntimeReceipt> {
    let data = fs::read(dir.join("receipt.json")).ok()?;
    serde_json::from_slice(&data).ok()
}

pub fn write_receipt(receipt: &RuntimeReceipt, dir: &Path) -> Result<()> {
    let json = serde_json::to_vec_pretty(receipt)?;
    fs::write(dir.join("receipt.json"), json)?;
    Ok(())
}

fn link_target(link: &Path) -> Option<String> {
    fs::read_link(link)
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

pub fn sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 4 * 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Enough to keep concurrent installs apart, without a uuid dependency.
fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    format!(
        "{}-{nanos:x}-{:x}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::{temp_dir, TempDir};
    use std::collections::HashSet;

    struct Fixture {
        root: TempDir,
    }

    impl Fixture {
        fn new() -> Fixture {
            Fixture {
                root: temp_dir("installer"),
            }
        }

        fn paths(&self) -> AppPaths {
            AppPaths::new(self.root.join("home"))
        }

        fn runtime_root(&self) -> PathBuf {
            self.root.join("home/runtime")
        }

        /// A payload whose "node" is a shell script reporting the expected version.
        fn make_payload(&self, dsh_version: &str, name: &str) -> (PathBuf, RuntimeManifest) {
            let tree = self.root.join(format!("{name}-tree"));
            let node_bin = tree.join("node/bin");
            let dsh = tree.join("app/node_modules/@deepseek-ai/dsh");
            fs::create_dir_all(&node_bin).unwrap();
            fs::create_dir_all(dsh.join("lib")).unwrap();
            let node = node_bin.join("node");
            fs::write(&node, "#!/bin/sh\necho v24.17.0\n").unwrap();
            fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(
                dsh.join("package.json"),
                format!(r#"{{"name":"@deepseek-ai/dsh","version":"{dsh_version}"}}"#),
            )
            .unwrap();
            fs::write(dsh.join("lib/bin.js"), "// cli\n").unwrap();

            let payload = self.root.join(name);
            fs::create_dir_all(&payload).unwrap();
            let archive = payload.join("runtime.aar");
            CommandSpec::new(
                "/usr/bin/aa",
                [
                    "archive",
                    "-d",
                    &tree.to_string_lossy(),
                    "-o",
                    &archive.to_string_lossy(),
                    "-a",
                    "lzma",
                    "-include-path",
                    "node",
                    "-include-path",
                    "app",
                ],
            )
            .check()
            .unwrap();
            let manifest = RuntimeManifest {
                schema_version: 1,
                dsh_version: dsh_version.to_owned(),
                node_version: "24.17.0".to_owned(),
                pnpm_version: "11.7.0".to_owned(),
                platform: "darwin".to_owned(),
                arch: "arm64".to_owned(),
                archive: "runtime.aar".to_owned(),
                archive_sha256: sha256(&archive).unwrap(),
                created_at: None,
            };
            fs::write(
                payload.join("manifest.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            (payload, manifest)
        }

        fn installer(&self, payload: Option<PathBuf>) -> RuntimeInstaller {
            let paths = self.paths();
            let logger = Arc::new(FileLogger::quiet(paths.shell_log()));
            RuntimeInstaller::new(paths, logger, payload)
        }

        fn link(&self, name: &str) -> Option<String> {
            link_target(&self.runtime_root().join(name))
        }

        /// Everything under `runtime/` except the `current` and `previous` links.
        fn runtime_directories(&self) -> HashSet<String> {
            fs::read_dir(self.runtime_root())
                .unwrap()
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name != "current" && name != "previous")
                .collect()
        }
    }

    fn ensure(installer: &RuntimeInstaller) -> Result<InstalledRuntime> {
        installer.ensure_runtime(|_| {})
    }

    #[test]
    fn installs_once_then_reuses() {
        let fixture = Fixture::new();
        let (payload, manifest) = fixture.make_payload("0.1.5-rc.2", "payload");
        let installer = fixture.installer(Some(payload));
        let first = ensure(&installer).unwrap();
        assert_eq!(first.receipt.identity, manifest.identity());
        assert_eq!(first.receipt.source, ReceiptSource::Bundle);
        assert_eq!(
            first.directory.file_name().unwrap(),
            manifest.directory_name().as_str()
        );
        assert!(first.node_executable().exists());

        // A marker inside the installed tree proves the second launch did not re-extract.
        let marker = first.directory.join("marker");
        fs::write(&marker, "").unwrap();
        let second = ensure(&installer).unwrap();
        assert_eq!(second.directory, first.directory);
        assert!(marker.exists());
        assert_eq!(installer.current_runtime().unwrap().receipt, first.receipt);
    }

    #[test]
    fn upgrade_switches_current_and_keeps_one_previous() {
        let fixture = Fixture::new();
        let (old_payload, _) = fixture.make_payload("0.1.5-rc.1", "old");
        let old = ensure(&fixture.installer(Some(old_payload.clone()))).unwrap();
        let (new_payload, new_manifest) = fixture.make_payload("0.1.5-rc.2", "new");
        let upgraded = ensure(&fixture.installer(Some(new_payload))).unwrap();
        assert_eq!(upgraded.receipt.dsh_version, "0.1.5-rc.2");
        assert_eq!(
            fixture.link("current").as_deref(),
            Some(new_manifest.directory_name().as_str())
        );
        assert_eq!(
            fixture.link("previous").as_deref(),
            old.directory.file_name().unwrap().to_str(),
            "the replaced runtime is kept as previous"
        );

        // Launching the older app again keeps the newer runtime.
        assert_eq!(
            ensure(&fixture.installer(Some(old_payload)))
                .unwrap()
                .receipt
                .dsh_version,
            "0.1.5-rc.2"
        );
    }

    #[test]
    fn another_upgrade_drops_the_oldest_runtime() {
        let fixture = Fixture::new();
        let mut manifests = Vec::new();
        for (index, version) in ["0.1.5-rc.1", "0.1.5-rc.2", "0.1.5"].iter().enumerate() {
            let (payload, manifest) = fixture.make_payload(version, &format!("payload{index}"));
            ensure(&fixture.installer(Some(payload))).unwrap();
            manifests.push(manifest);
        }
        assert_eq!(
            fixture.link("current").as_deref(),
            Some(manifests[2].directory_name().as_str())
        );
        assert_eq!(
            fixture.link("previous").as_deref(),
            Some(manifests[1].directory_name().as_str())
        );
        assert_eq!(
            fixture.runtime_directories(),
            HashSet::from([manifests[2].directory_name(), manifests[1].directory_name()])
        );
    }

    #[test]
    fn reinstalling_the_current_runtime_keeps_previous() {
        let fixture = Fixture::new();
        let (old_payload, old_manifest) = fixture.make_payload("0.1.5-rc.1", "old");
        let (new_payload, new_manifest) = fixture.make_payload("0.1.5-rc.2", "new");
        ensure(&fixture.installer(Some(old_payload))).unwrap();
        ensure(&fixture.installer(Some(new_payload.clone()))).unwrap();
        fixture.installer(Some(new_payload)).install_bundled().unwrap(); // "Reinstall Bundled Runtime"
        assert_eq!(
            fixture.link("current").as_deref(),
            Some(new_manifest.directory_name().as_str())
        );
        assert_eq!(
            fixture.link("previous").as_deref(),
            Some(old_manifest.directory_name().as_str())
        );
        assert_eq!(
            fixture.runtime_directories(),
            HashSet::from([new_manifest.directory_name(), old_manifest.directory_name()])
        );
    }

    #[test]
    fn without_keeping_previous_only_current_remains() {
        let fixture = Fixture::new();
        let (old_payload, _) = fixture.make_payload("0.1.5-rc.1", "old");
        let (new_payload, new_manifest) = fixture.make_payload("0.1.5-rc.2", "new");
        ensure(&fixture.installer(Some(old_payload))).unwrap();
        let mut installer = fixture.installer(Some(new_payload));
        installer.keeps_previous_runtime = false;
        ensure(&installer).unwrap();
        assert!(fixture.link("previous").is_none());
        assert_eq!(
            fixture.runtime_directories(),
            HashSet::from([new_manifest.directory_name()])
        );
    }

    #[test]
    fn turning_retention_off_removes_previous_at_once() {
        let fixture = Fixture::new();
        let (old_payload, _) = fixture.make_payload("0.1.5-rc.1", "old");
        let (new_payload, new_manifest) = fixture.make_payload("0.1.5-rc.2", "new");
        ensure(&fixture.installer(Some(old_payload))).unwrap();
        let mut installer = fixture.installer(Some(new_payload));
        ensure(&installer).unwrap();
        assert!(fixture.link("previous").is_some());
        installer.keeps_previous_runtime = false;
        installer.apply_retention();
        assert!(fixture.link("previous").is_none());
        assert_eq!(
            fixture.link("current").as_deref(),
            Some(new_manifest.directory_name().as_str())
        );
        assert_eq!(
            fixture.runtime_directories(),
            HashSet::from([new_manifest.directory_name()])
        );
    }

    #[test]
    fn rejects_a_tampered_archive() {
        let fixture = Fixture::new();
        let (payload, manifest) = fixture.make_payload("0.1.5-rc.2", "payload");
        let mut tampered = manifest;
        tampered.archive_sha256 = "0".repeat(64);
        fs::write(
            payload.join("manifest.json"),
            serde_json::to_vec(&tampered).unwrap(),
        )
        .unwrap();
        let error = ensure(&fixture.installer(Some(payload.clone()))).unwrap_err();
        assert!(error.message().contains("checksum mismatch"), "{error}");
        assert!(fixture.installer(Some(payload)).current_runtime().is_none());
    }

    #[test]
    fn rejects_the_wrong_dsh_version() {
        let fixture = Fixture::new();
        let (payload, manifest) = fixture.make_payload("0.1.5-rc.2", "payload");
        let mut wrong = manifest;
        wrong.dsh_version = "9.9.9".to_owned();
        fs::write(payload.join("manifest.json"), serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(fixture.installer(Some(payload)).install_bundled().is_err());
        let leftovers = fixture.runtime_directories();
        assert!(
            !leftovers.iter().any(|name| name.starts_with(".staging-")),
            "staging is cleaned up: {leftovers:?}"
        );
    }

    #[test]
    fn no_payload_and_nothing_installed_is_an_error() {
        let fixture = Fixture::new();
        assert!(ensure(&fixture.installer(None)).is_err());
    }
}
