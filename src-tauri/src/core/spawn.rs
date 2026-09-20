//! A long-lived child in its own process group, with default signal dispositions
//! and no inherited descriptors beyond stdio, so one signal reaches everything
//! the daemon started and nothing of the shell leaks into it.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};

use crate::core::command::CommandSpec;
use crate::core::{Error, Result};

pub struct SpawnedProcess {
    pub pid: i32,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    child: Child,
}

impl SpawnedProcess {
    /// Signal the child itself.
    pub fn signal(&self, signal: i32) -> bool {
        signal_pid(self.pid, signal)
    }

    /// Signal every process still in the child's group (the group id is the pid).
    pub fn signal_group(&self, signal: i32) -> bool {
        signal_group(self.pid, signal)
    }

    /// Wait for the child and return its exit code (or 128 + signal).
    pub fn wait(mut self) -> Option<i32> {
        let status = self.child.wait().ok()?;
        Some(exit_code(&status))
    }
}

pub fn exit_code(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| status.signal().map(|signal| 128 + signal).unwrap_or(-1))
}

pub fn spawn(
    executable: &Path,
    arguments: &[String],
    environment: &HashMap<String, String>,
    working_directory: &Path,
) -> Result<SpawnedProcess> {
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .current_dir(working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .envs(environment);
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            // Its own process group, so stragglers can be reaped with one signal.
            libc::setpgid(0, 0);
            // Rust ignores SIGPIPE for itself; children must not inherit that.
            // Darwin's NSIG; libc does not export it.
            for number in 1..32 {
                if number != libc::SIGKILL && number != libc::SIGSTOP {
                    libc::signal(number, libc::SIG_DFL);
                }
            }
            let mut empty: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut empty);
            libc::pthread_sigmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| Error::new(format!("could not start {}: {error}", executable.display())))?;
    Ok(SpawnedProcess {
        pid: child.id() as i32,
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        child,
    })
}

pub fn signal_pid(pid: i32, signal: i32) -> bool {
    pid > 0 && unsafe { libc::kill(pid, signal) } == 0
}

pub fn signal_group(pid: i32, signal: i32) -> bool {
    pid > 0 && unsafe { libc::kill(-pid, signal) } == 0
}

/// Whether a process with this pid exists (and is visible to us).
pub fn is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe {
        if libc::kill(pid, 0) == 0 {
            return true;
        }
        *libc::__error() == libc::EPERM
    }
}

/// The full command line of a process, used to recognize an orphaned daemon.
pub fn command_line(pid: i32) -> Option<String> {
    let result = CommandSpec::new("/bin/ps", ["-ww", "-o", "command=", "-p", &pid.to_string()])
        .timeout(std::time::Duration::from_secs(5))
        .run()
        .ok()?;
    if !result.succeeded() {
        return None;
    }
    let text = result.stdout_text().trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn runs_in_its_own_process_group_with_the_given_environment() {
        let mut child = spawn(
            Path::new("/bin/sh"),
            &["-c".to_owned(), "echo $GREETING; ps -o pgid= -p $$".to_owned()],
            &HashMap::from([("GREETING".to_owned(), "hi".to_owned())]),
            Path::new("/tmp"),
        )
        .unwrap();
        let pid = child.pid;
        let mut output = String::new();
        child.stdout.take().unwrap().read_to_string(&mut output).unwrap();
        assert_eq!(child.wait(), Some(0));
        let mut lines = output.lines();
        assert_eq!(lines.next(), Some("hi"));
        assert_eq!(
            lines.next().unwrap().trim().parse::<i32>().unwrap(),
            pid,
            "pgid is the pid"
        );
    }

    #[test]
    fn recognizes_a_live_process_and_reads_its_command_line() {
        let child = spawn(
            Path::new("/bin/sh"),
            &["-c".to_owned(), "sleep 5".to_owned()],
            &HashMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]),
            Path::new("/tmp"),
        )
        .unwrap();
        assert!(is_alive(child.pid));
        assert!(command_line(child.pid).unwrap().contains("sleep 5"));
        assert!(child.signal_group(libc::SIGKILL));
        child.wait();
    }
}
