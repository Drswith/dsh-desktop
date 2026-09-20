//! Everything needed to boot `dsh --profile <name> … --no-open` once, plus the
//! state the shell shows for it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::core::ready_line;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonLaunchPlan {
    pub node: PathBuf,
    pub entry: PathBuf,
    pub dsh_version: Option<String>,
    pub node_version: Option<String>,
    pub profile: String,
    pub dsh_home: PathBuf,
    pub preferred_port: u16,
    pub extra_args: Vec<String>,
    pub environment: HashMap<String, String>,
    pub working_directory: PathBuf,
}

/// Profiles the CLI ships; they initialize themselves and reject `--from-default-profile`.
pub const SHIPPED_PROFILES: &[&str] = &["acp", "web", "headless", "sdk", "sdk-minimal"];

impl DaemonLaunchPlan {
    /// Resolve `$DSH_HOME` the way `@deepseek-ai/dsh-home-paths` does.
    pub fn resolve_dsh_home(
        configured: Option<&str>,
        environment: &HashMap<String, String>,
        home: &Path,
    ) -> PathBuf {
        let non_blank =
            |value: &str| -> Option<String> { (!value.trim().is_empty()).then(|| value.to_owned()) };
        let raw = configured
            .and_then(non_blank)
            .or_else(|| environment.get("DSH_HOME").and_then(|value| non_blank(value)));
        let Some(raw) = raw else { return home.join(".dsh") };
        if raw == "~" {
            return home.to_path_buf();
        }
        if let Some(rest) = raw.strip_prefix("~/") {
            return home.join(rest);
        }
        PathBuf::from(raw)
    }

    pub fn profile_directory(&self) -> PathBuf {
        self.dsh_home.join("profiles").join(&self.profile)
    }

    /// Custom profiles are created from the shipped `web` template exactly once.
    pub fn needs_profile_initialization(&self) -> bool {
        if SHIPPED_PROFILES.contains(&self.profile.as_str()) {
            return false;
        }
        !self.profile_directory().join("package.json").exists()
    }

    /// Launcher flags first, then the Web app's own flags, then user extras.
    pub fn arguments(&self, port: u16, initialize_profile: bool) -> Vec<String> {
        let mut args = vec![
            self.entry.to_string_lossy().into_owned(),
            "--profile".to_owned(),
            self.profile.clone(),
        ];
        if initialize_profile {
            args.extend(["--from-default-profile".to_owned(), "web".to_owned()]);
        }
        args.extend([
            "--no-open".to_owned(),
            "--host".to_owned(),
            "127.0.0.1".to_owned(),
            "--port".to_owned(),
            port.to_string(),
        ]);
        args.extend(self.extra_args.iter().cloned());
        args
    }

    pub fn runtime_summary(&self) -> String {
        [
            self.dsh_version.as_ref().map(|version| format!("DSH {version}")),
            self.node_version
                .as_ref()
                .map(|version| format!("Node.js {version}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonInfo {
    pub pid: i32,
    pub port: u16,
    /// Loopback root URL carrying this process's launch token.
    pub authenticated_url: Url,
    pub started_at: SystemTime,
}

impl DaemonInfo {
    pub fn clean_url(&self) -> Url {
        ready_line::clean_url(&self.authenticated_url)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonState {
    Idle,
    Starting { attempt: u32 },
    Running(DaemonInfo),
    Stopping,
    Stopped,
    Failed(String),
}

impl DaemonState {
    pub fn info(&self) -> Option<&DaemonInfo> {
        match self {
            DaemonState::Running(info) => Some(info),
            _ => None,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            DaemonState::Starting { .. } | DaemonState::Running(_) | DaemonState::Stopping
        )
    }
}

#[derive(Debug, Clone)]
pub struct SupervisorPolicy {
    pub startup_timeout: Duration,
    pub stop_grace: Duration,
    pub health_interval: Duration,
    pub health_timeout: Duration,
    pub max_health_failures: u32,
    pub wake_grace: Duration,
    pub restart_backoff: Vec<Duration>,
    pub crash_window: Duration,
    pub max_crashes_in_window: usize,
    pub port_attempts: u16,
}

impl Default for SupervisorPolicy {
    fn default() -> Self {
        SupervisorPolicy {
            startup_timeout: Duration::from_secs(180),
            stop_grace: Duration::from_secs(8),
            health_interval: Duration::from_secs(15),
            health_timeout: Duration::from_secs(5),
            max_health_failures: 3,
            wake_grace: Duration::from_secs(60),
            restart_backoff: [1, 2, 5, 10, 30]
                .iter()
                .map(|s| Duration::from_secs(*s))
                .collect(),
            crash_window: Duration::from_secs(600),
            max_crashes_in_window: 5,
            port_attempts: 20,
        }
    }
}

/// `run/daemon.json`: lets the next launch recognize and stop an orphaned daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonRecord {
    pub pid: i32,
    pub port: u16,
    pub entry: String,
    pub shell_pid: i32,
    pub started_at: String,
}

/// Splits a byte stream into lines.
#[derive(Default)]
pub struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    pub fn append(&mut self, data: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(data);
        let mut lines = Vec::new();
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line = &self.pending[..newline];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            lines.push(String::from_utf8_lossy(line).into_owned());
            self.pending.drain(..=newline);
        }
        // A runaway line without a newline is emitted in chunks rather than buffered forever.
        if self.pending.len() > 64 * 1024 {
            lines.push(String::from_utf8_lossy(&self.pending).into_owned());
            self.pending.clear();
        }
        lines
    }

    pub fn flush(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let line = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(profile: &str, home: &Path) -> DaemonLaunchPlan {
        DaemonLaunchPlan {
            node: PathBuf::from("/rt/node/bin/node"),
            entry: PathBuf::from("/rt/app/node_modules/@deepseek-ai/dsh/lib/bin.js"),
            dsh_version: Some("0.1.5-rc.2".to_owned()),
            node_version: Some("24.17.0".to_owned()),
            profile: profile.to_owned(),
            dsh_home: home.to_path_buf(),
            preferred_port: 31080,
            extra_args: vec!["--trusted-host".to_owned(), "dev.local".to_owned()],
            environment: HashMap::new(),
            working_directory: PathBuf::from("/"),
        }
    }

    #[test]
    fn arguments_put_launcher_flags_before_app_flags() {
        let args = plan("launcher", Path::new("/h")).arguments(31081, true);
        assert_eq!(
            args,
            [
                "/rt/app/node_modules/@deepseek-ai/dsh/lib/bin.js",
                "--profile",
                "launcher",
                "--from-default-profile",
                "web",
                "--no-open",
                "--host",
                "127.0.0.1",
                "--port",
                "31081",
                "--trusted-host",
                "dev.local",
            ]
        );
    }

    #[test]
    fn custom_profile_initializes_once_and_shipped_profiles_never() {
        let home = crate::core::testing::temp_dir("plan");
        let custom = plan("launcher", home.path());
        assert!(custom.needs_profile_initialization());
        std::fs::create_dir_all(custom.profile_directory()).unwrap();
        std::fs::write(custom.profile_directory().join("package.json"), "{}").unwrap();
        assert!(!custom.needs_profile_initialization());
        assert!(!plan("web", home.path()).needs_profile_initialization());
    }

    #[test]
    fn resolves_dsh_home_like_the_cli() {
        let home = Path::new("/Users/me");
        let empty = HashMap::new();
        let blank = HashMap::from([("DSH_HOME".to_owned(), "  ".to_owned())]);
        let tilde = HashMap::from([("DSH_HOME".to_owned(), "~/alt".to_owned())]);
        assert_eq!(
            DaemonLaunchPlan::resolve_dsh_home(None, &empty, home),
            Path::new("/Users/me/.dsh")
        );
        assert_eq!(
            DaemonLaunchPlan::resolve_dsh_home(None, &blank, home),
            Path::new("/Users/me/.dsh")
        );
        assert_eq!(
            DaemonLaunchPlan::resolve_dsh_home(None, &tilde, home),
            Path::new("/Users/me/alt")
        );
        assert_eq!(
            DaemonLaunchPlan::resolve_dsh_home(Some("/data/dsh"), &tilde, home),
            Path::new("/data/dsh")
        );
    }

    #[test]
    fn runtime_summary_uses_product_names() {
        assert_eq!(
            plan("launcher", Path::new("/h")).runtime_summary(),
            "DSH 0.1.5-rc.2 · Node.js 24.17.0"
        );
    }

    #[test]
    fn line_buffer_splits_across_chunks() {
        let mut buffer = LineBuffer::default();
        assert!(buffer.append(b"dsh we").is_empty());
        assert_eq!(buffer.append(b"b: x\r\nsecond\nthi"), ["dsh web: x", "second"]);
        assert_eq!(buffer.flush().as_deref(), Some("thi"));
        assert!(buffer.flush().is_none());
    }
}
