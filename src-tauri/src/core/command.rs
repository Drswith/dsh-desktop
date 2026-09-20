//! Short-lived helper commands (aa, xattr, login shell, node --version).

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::core::{Error, Result};

#[derive(Debug)]
pub struct CommandResult {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

impl CommandResult {
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.status == 0
    }

    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

pub struct CommandSpec<'a> {
    executable: &'a str,
    arguments: Vec<String>,
    environment: Option<HashMap<String, String>>,
    current_directory: Option<&'a Path>,
    timeout: Duration,
}

impl<'a> CommandSpec<'a> {
    pub fn new(executable: &'a str, arguments: impl IntoIterator<Item = impl Into<String>>) -> Self {
        CommandSpec {
            executable,
            arguments: arguments.into_iter().map(Into::into).collect(),
            environment: None,
            current_directory: None,
            timeout: Duration::from_secs(120),
        }
    }

    pub fn environment(mut self, environment: HashMap<String, String>) -> Self {
        self.environment = Some(environment);
        self
    }

    pub fn current_directory(mut self, directory: &'a Path) -> Self {
        self.current_directory = Some(directory);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn run(self) -> Result<CommandResult> {
        let mut command = Command::new(self.executable);
        command
            .args(&self.arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(environment) = &self.environment {
            command.env_clear().envs(environment);
        }
        if let Some(directory) = self.current_directory {
            command.current_dir(directory);
        }
        let mut child = command
            .spawn()
            .map_err(|error| Error::new(format!("could not run {}: {error}", self.executable)))?;

        // Drain both pipes concurrently so a chatty child never blocks on a full pipe.
        let (sender, receiver) = mpsc::channel();
        for (is_stdout, mut pipe) in [
            (true, child.stdout.take().map(PipeReader::Out)),
            (false, child.stderr.take().map(PipeReader::Err)),
        ] {
            let sender = sender.clone();
            thread::spawn(move || {
                let mut buffer = Vec::new();
                if let Some(pipe) = pipe.as_mut() {
                    let _ = pipe.read_to_end(&mut buffer);
                }
                let _ = sender.send((is_stdout, buffer));
            });
        }
        drop(sender);

        let deadline = Instant::now() + self.timeout;
        let mut timed_out = false;
        let status = loop {
            match child.try_wait()? {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    timed_out = true;
                    // Ask first, insist after three seconds.
                    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
                    let hard_deadline = Instant::now() + Duration::from_secs(3);
                    break loop {
                        if let Some(status) = child.try_wait()? {
                            break status;
                        }
                        if Instant::now() >= hard_deadline {
                            let _ = child.kill();
                            break child.wait()?;
                        }
                        thread::sleep(Duration::from_millis(20));
                    };
                }
                None => thread::sleep(Duration::from_millis(10)),
            }
        };

        // A grandchild holding a pipe open must not hang the caller forever.
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let drain_deadline = Instant::now() + Duration::from_secs(5);
        for _ in 0..2 {
            let remaining = drain_deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining) {
                Ok((true, data)) => stdout = data,
                Ok((false, data)) => stderr = data,
                Err(_) => break,
            }
        }
        Ok(CommandResult {
            status: status.code().unwrap_or_else(|| {
                use std::os::unix::process::ExitStatusExt;
                status.signal().map(|signal| 128 + signal).unwrap_or(-1)
            }),
            stdout,
            stderr,
            timed_out,
        })
    }

    /// Run and fail with the command's stderr attached.
    pub fn check(self) -> Result<CommandResult> {
        let name = Path::new(self.executable)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.executable.to_owned());
        let timeout = self.timeout.as_secs();
        let result = self.run()?;
        if result.succeeded() {
            return Ok(result);
        }
        let detail = result.stderr_text().trim().to_owned();
        let reason = if result.timed_out {
            format!("timed out after {timeout}s")
        } else {
            format!("exited with status {}", result.status)
        };
        let tail: String = detail
            .chars()
            .rev()
            .take(600)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Err(Error::new(if tail.is_empty() {
            format!("{name} {reason}")
        } else {
            format!("{name} {reason}: {tail}")
        }))
    }
}

enum PipeReader {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl Read for PipeReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            PipeReader::Out(pipe) => pipe.read(buffer),
            PipeReader::Err(pipe) => pipe.read(buffer),
        }
    }
}

/// `CommandSpec::new(…).run()`, spelled the way call sites read best.
pub fn run(executable: &str, arguments: &[&str]) -> Result<CommandResult> {
    CommandSpec::new(executable, arguments.iter().copied()).run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_status_and_output() {
        let result = run("/bin/sh", &["-c", "printf out; printf err >&2; exit 3"]).unwrap();
        assert_eq!(result.status, 3);
        assert_eq!(result.stdout_text(), "out");
        assert_eq!(result.stderr_text(), "err");
        assert!(!result.succeeded());
    }

    #[test]
    fn kills_a_command_that_outstays_its_timeout() {
        let result = CommandSpec::new("/bin/sh", ["-c", "sleep 30"])
            .timeout(Duration::from_millis(300))
            .run()
            .unwrap();
        assert!(result.timed_out);
    }

    #[test]
    fn check_carries_stderr_into_the_error() {
        let error = CommandSpec::new("/bin/sh", ["-c", "echo boom >&2; exit 1"])
            .check()
            .unwrap_err();
        assert!(error.message().contains("boom"), "{error}");
    }
}
