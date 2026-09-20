//! Lifecycle logic with no UI framework in it: runtime install, daemon
//! supervision, probes. Everything here is testable with `cargo test`.

pub mod command;
pub mod daemon;
pub mod error;
pub mod installer;
pub mod logger;
pub mod manifest;
pub mod paths;
pub mod probes;
pub mod ready_line;
pub mod semver;
pub mod shell_env;
pub mod spawn;
pub mod supervisor;

pub use error::{Error, Result};

use std::path::PathBuf;

/// The current user's home directory, from the passwd database like
/// `FileManager.homeDirectoryForCurrentUser`, falling back to `$HOME`.
pub fn home_dir() -> PathBuf {
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if !entry.is_null() && !(*entry).pw_dir.is_null() {
            let dir = std::ffi::CStr::from_ptr((*entry).pw_dir)
                .to_string_lossy()
                .into_owned();
            if !dir.is_empty() {
                return PathBuf::from(dir);
            }
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Expand a leading `~` the way a shell would.
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    match path.strip_prefix("~/") {
        Some(rest) => home_dir().join(rest),
        None => PathBuf::from(path),
    }
}

/// UTC timestamp in the `2026-09-19T13:05:24Z` shape `ISO8601DateFormatter` writes.
pub fn iso8601_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_iso8601(seconds)
}

pub fn format_iso8601(unix_seconds: i64) -> String {
    let time = unix_seconds as libc::time_t;
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::gmtime_r(&time, &mut parts) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        parts.tm_year + 1900,
        parts.tm_mon + 1,
        parts.tm_mday,
        parts.tm_hour,
        parts.tm_min,
        parts.tm_sec
    )
}

#[cfg(test)]
pub mod testing {
    use std::ops::Deref;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A unique directory under `$TMPDIR`, removed when the test ends.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Deref for TempDir {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    pub fn temp_dir(prefix: &str) -> TempDir {
        let unique = format!(
            "dsh-launcher-{prefix}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }
}
