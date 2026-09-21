//! 两份持久化日志：启动器自身，以及 dsh 的 stdout/stderr。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 8 * 1024 * 1024;
const KEEP_ROTATED: u32 = 3;

#[derive(Clone)]
pub struct LogFile {
    path: PathBuf,
    file: Arc<Mutex<Option<File>>>,
}

impl LogFile {
    pub fn new(path: PathBuf) -> Self {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        Self {
            path,
            file: Arc::new(Mutex::new(None)),
        }
    }

    pub fn log(&self, stream: &str, message: &str) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let line = format!("[{timestamp}] {stream} | {}\n", redact(message));
        let Ok(mut file) = self.file.lock() else { return };
        if file.is_none() {
            *file = open_file(&self.path);
        }
        if file
            .as_ref()
            .and_then(|current| current.metadata().ok())
            .map(|metadata| metadata.len().saturating_add(line.len() as u64) > MAX_BYTES)
            .unwrap_or(false)
        {
            file.take();
            rotate(&self.path);
            *file = open_file(&self.path);
        }
        if let Some(file) = file.as_mut() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
}

fn open_file(path: &Path) -> Option<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path).ok()
}

fn rotated_path(path: &Path, index: u32) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "launcher.log".to_owned());
    path.with_file_name(format!("{name}.{index}"))
}

fn rotate(path: &Path) {
    let _ = fs::remove_file(rotated_path(path, KEEP_ROTATED));
    for index in (1..KEEP_ROTATED).rev() {
        let from = rotated_path(path, index);
        if from.exists() {
            let _ = fs::rename(&from, rotated_path(path, index + 1));
        }
    }
    let _ = fs::rename(path, rotated_path(path, 1));
}

#[derive(Clone)]
pub struct LogFiles {
    pub launcher: LogFile,
    pub dsh: LogFile,
}

impl LogFiles {
    pub fn new(logs_dir: &Path) -> Self {
        Self {
            launcher: LogFile::new(logs_dir.join("launcher.log")),
            dsh: LogFile::new(logs_dir.join("dsh.log")),
        }
    }
}

pub fn redact(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find("token=") {
        let start = cursor + relative;
        output.push_str(&text[cursor..start]);
        output.push_str("token=<redacted>");
        let mut end = start + "token=".len();
        while end < text.len() {
            let byte = text.as_bytes()[end];
            if !(byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-') {
                break;
            }
            end += 1;
        }
        cursor = end;
    }
    output.push_str(&text[cursor..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_ready_token_without_touching_other_query_values() {
        assert_eq!(
            redact("dsh web: http://127.0.0.1:31080/?token=abc_DEF-123&x=1"),
            "dsh web: http://127.0.0.1:31080/?token=<redacted>&x=1"
        );
    }
}
