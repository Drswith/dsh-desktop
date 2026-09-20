//! Shell-owned state directory (`~/.dsh-launcher` by default). dsh product data
//! (sessions, settings, credentials, profiles) stays in `$DSH_HOME`; this tree
//! holds only the executable runtime, logs, and supervisor state.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::Result;

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub home: PathBuf,
}

impl AppPaths {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        AppPaths { home: home.into() }
    }

    pub fn standard(dir_name: &str) -> Self {
        AppPaths::new(crate::core::home_dir().join(dir_name))
    }

    pub fn runtime_root(&self) -> PathBuf {
        self.home.join("runtime")
    }

    /// Symlink naming the active runtime directory (relative target).
    pub fn current_runtime_link(&self) -> PathBuf {
        self.runtime_root().join("current")
    }

    /// Symlink naming the runtime the last upgrade replaced, when it is kept.
    pub fn previous_runtime_link(&self) -> PathBuf {
        self.runtime_root().join("previous")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.home.join("logs")
    }

    pub fn run_dir(&self) -> PathBuf {
        self.home.join("run")
    }

    pub fn shell_log(&self) -> PathBuf {
        self.logs_dir().join("launcher.log")
    }

    pub fn daemon_log(&self) -> PathBuf {
        self.logs_dir().join("dsh.log")
    }

    pub fn daemon_state_file(&self) -> PathBuf {
        self.run_dir().join("daemon.json")
    }

    pub fn config_file(&self) -> PathBuf {
        self.home.join("config.json")
    }

    /// Create the tree with owner-only permissions.
    pub fn prepare(&self) -> Result<()> {
        for dir in [
            self.home.clone(),
            self.runtime_root(),
            self.logs_dir(),
            self.run_dir(),
        ] {
            fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        }
        Ok(())
    }
}

/// Development override: run an existing Node binary and dsh entry instead of
/// the bundled runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalRuntime {
    pub node: String,
    pub entry: String,
}

/// Optional user overrides read from `config.json`; every field may be omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ShellConfig {
    /// Preferred loopback port; the next free port is used when it is taken.
    pub port: Option<u16>,
    /// dsh profile under `$DSH_HOME/profiles`; initialized from the shipped `web` template.
    pub profile: Option<String>,
    /// Explicit `DSH_HOME`; otherwise the login-shell value or `~/.dsh`.
    pub dsh_home: Option<String>,
    /// Extra arguments appended after the Web app flags.
    pub extra_args: Option<Vec<String>>,
    /// Extra environment variables for the dsh process.
    pub environment: Option<HashMap<String, String>>,
    /// Development override; see [`ExternalRuntime`].
    pub runtime: Option<ExternalRuntime>,
}

impl ShellConfig {
    /// Load `config.json`; a missing (or blank) file yields the empty configuration.
    pub fn load(path: &Path) -> Result<Self> {
        let data = match fs::read(path) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(ShellConfig::default()),
            Err(error) => return Err(error.into()),
        };
        if data
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            return Ok(ShellConfig::default());
        }
        Ok(serde_json::from_slice(&data)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_blank_configuration_files_are_empty() {
        let dir = crate::core::testing::temp_dir("config");
        let file = dir.join("config.json");
        assert_eq!(ShellConfig::load(&file).unwrap(), ShellConfig::default());
        fs::write(&file, "  \n\t").unwrap();
        assert_eq!(ShellConfig::load(&file).unwrap(), ShellConfig::default());
    }

    #[test]
    fn reads_the_documented_camel_case_keys() {
        let dir = crate::core::testing::temp_dir("config");
        let file = dir.join("config.json");
        fs::write(
            &file,
            r#"{"port":31080,"profile":"launcher","dshHome":"~/alt","extraArgs":["--trusted-host","h"],
                "environment":{"HTTPS_PROXY":"http://127.0.0.1:7890"},
                "runtime":{"node":"/n","entry":"/e"}}"#,
        )
        .unwrap();
        let config = ShellConfig::load(&file).unwrap();
        assert_eq!(config.port, Some(31080));
        assert_eq!(config.dsh_home.as_deref(), Some("~/alt"));
        assert_eq!(config.extra_args.unwrap(), ["--trusted-host", "h"]);
        assert_eq!(config.runtime.unwrap().entry, "/e");
    }
}
