//! Append-only log file with size-based rotation (`name.log`, `name.log.1`, …).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct FileLogger {
    path: PathBuf,
    max_bytes: u64,
    keep: u32,
    echo_to_stderr: bool,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    file: Option<fs::File>,
    size: u64,
}

impl FileLogger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let echo = unsafe { libc::isatty(libc::STDERR_FILENO) } != 0;
        FileLogger::with_options(path, 8 * 1024 * 1024, 3, echo)
    }

    pub fn quiet(path: impl Into<PathBuf>) -> Self {
        FileLogger::with_options(path, 8 * 1024 * 1024, 3, false)
    }

    pub fn with_options(path: impl Into<PathBuf>, max_bytes: u64, keep: u32, echo_to_stderr: bool) -> Self {
        FileLogger {
            path: path.into(),
            max_bytes,
            keep: keep.max(1),
            echo_to_stderr,
            state: Mutex::new(State::default()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one timestamped line; secrets must already be redacted by the caller.
    pub fn log(&self, message: impl AsRef<str>) {
        let line = format!("[{}] {}\n", timestamp(), message.as_ref());
        let bytes = line.as_bytes();
        if self.echo_to_stderr {
            let _ = std::io::stderr().write_all(bytes);
        }
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.size + bytes.len() as u64 > self.max_bytes {
            self.rotate(&mut state);
        }
        if self.open_if_needed(&mut state).is_none() {
            return;
        }
        let file = state.file.as_mut().expect("opened above");
        match file.write_all(bytes) {
            Ok(()) => state.size += bytes.len() as u64,
            Err(_) => state.file = None,
        }
    }

    /// Block until queued writes reach the file.
    pub fn flush(&self) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(file) = state.file.as_mut() {
                let _ = file.sync_all();
            }
        }
    }

    fn open_if_needed<'a>(&self, state: &'a mut State) -> Option<&'a mut fs::File> {
        if state.file.is_none() {
            if let Some(parent) = self.path.parent() {
                let _ = fs::DirBuilder::new().recursive(true).mode(0o700).create(parent);
            }
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&self.path)
                .ok()?;
            state.size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
            state.file = Some(file);
        }
        state.file.as_mut()
    }

    fn rotate(&self, state: &mut State) {
        state.file = None;
        state.size = 0;
        let numbered = |index: u32| {
            let mut name = self.path.as_os_str().to_owned();
            name.push(format!(".{index}"));
            PathBuf::from(name)
        };
        let _ = fs::remove_file(numbered(self.keep));
        for index in (1..self.keep).rev() {
            let from = numbered(index);
            if from.exists() {
                let _ = fs::rename(&from, numbered(index + 1));
            }
        }
        let _ = fs::rename(&self.path, numbered(1));
    }
}

/// `2026-09-19 13:05:24.123 +0800`, local time with the offset spelled out.
fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs() as libc::time_t;
    let millis = now.subsec_millis();
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&seconds, &mut parts) };
    let offset_minutes = parts.tm_gmtoff / 60;
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let offset_minutes = offset_minutes.abs();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {}{:02}{:02}",
        parts.tm_year + 1900,
        parts.tm_mon + 1,
        parts.tm_mday,
        parts.tm_hour,
        parts.tm_min,
        parts.tm_sec,
        millis,
        sign,
        offset_minutes / 60,
        offset_minutes % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_when_the_file_grows_past_the_limit() {
        let dir = crate::core::testing::temp_dir("logger");
        let path = dir.join("launcher.log");
        let logger = FileLogger::with_options(&path, 200, 2, false);
        for index in 0..40 {
            logger.log(format!("line {index} with enough text to fill the file"));
        }
        assert!(path.exists());
        assert!(path.with_extension("log.1").exists(), "a rotated file is kept");
        assert!(
            !path.with_extension("log.3").exists(),
            "only `keep` files survive"
        );
    }
}
