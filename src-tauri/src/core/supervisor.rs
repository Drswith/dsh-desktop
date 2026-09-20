//! Owns one `dsh` Web daemon: spawn, readiness via the `dsh web:` line, health
//! watchdog, crash restarts with backoff, and graceful stop. All mutable state
//! lives on the worker thread; state changes are published through a callback.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::core::daemon::{
    DaemonInfo, DaemonLaunchPlan, DaemonRecord, DaemonState, LineBuffer, SupervisorPolicy,
};
use crate::core::logger::FileLogger;
use crate::core::paths::AppPaths;
use crate::core::probes::{self, HealthResult};
use crate::core::ready_line;
use crate::core::spawn;

type Completion = Box<dyn FnOnce() + Send>;
type StateCallback = Box<dyn Fn(DaemonState) + Send + Sync>;

enum Cmd {
    Start(Box<DaemonLaunchPlan>),
    Restart(Option<Box<DaemonLaunchPlan>>),
    Stop(Option<Completion>),
    WillSleep,
    DidWake,
    Line {
        generation: u64,
        is_stdout: bool,
        text: String,
    },
    Exited {
        generation: u64,
        code: Option<i32>,
    },
    Health {
        generation: u64,
        result: HealthResult,
    },
    Shutdown,
}

struct Shared {
    state: Mutex<DaemonState>,
    recent_errors: Mutex<Vec<String>>,
    on_state_change: Mutex<Option<StateCallback>>,
}

pub struct DaemonSupervisor {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    paths: AppPaths,
    log: Arc<FileLogger>,
    stop_grace: Duration,
}

impl DaemonSupervisor {
    pub fn new(
        paths: AppPaths,
        log: Arc<FileLogger>,
        output: Arc<FileLogger>,
        policy: SupervisorPolicy,
    ) -> Arc<DaemonSupervisor> {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(DaemonState::Idle),
            recent_errors: Mutex::new(Vec::new()),
            on_state_change: Mutex::new(None),
        });
        let worker = Worker {
            shared: shared.clone(),
            paths: paths.clone(),
            log: log.clone(),
            output,
            policy: policy.clone(),
            tx: tx.clone(),
            rx,
            plan: None,
            process: None,
            generation: 0,
            desired: false,
            restart_after_exit: false,
            stop_completions: Vec::new(),
            startup_deadline: None,
            kill_deadline: None,
            health_next: None,
            restart_at: None,
            group_kills: Vec::new(),
            health_in_flight: false,
            health_failures: 0,
            health_grace_until: None,
            sleeping: false,
            crash_times: Vec::new(),
            attempt: 0,
            current_port: None,
            avoid_port: None,
            spawned_at: Instant::now(),
        };
        let handle = thread::Builder::new()
            .name("dsh-launcher.supervisor".to_owned())
            .spawn(move || worker.run())
            .expect("supervisor thread");
        Arc::new(DaemonSupervisor {
            tx,
            shared,
            worker: Mutex::new(Some(handle)),
            paths,
            log,
            stop_grace: policy.stop_grace,
        })
    }

    pub fn on_state_change(&self, callback: impl Fn(DaemonState) + Send + Sync + 'static) {
        *self.shared.on_state_change.lock().unwrap() = Some(Box::new(callback));
    }

    pub fn current_state(&self) -> DaemonState {
        self.shared.state.lock().unwrap().clone()
    }

    pub fn recent_error_lines(&self) -> Vec<String> {
        self.shared.recent_errors.lock().unwrap().clone()
    }

    /// Start (or keep) the daemon with `plan`; clears a previous failure.
    pub fn start(&self, plan: DaemonLaunchPlan) {
        let _ = self.tx.send(Cmd::Start(Box::new(plan)));
    }

    /// Stop the running daemon (if any) and start it again, optionally with a new plan.
    pub fn restart(&self, plan: Option<DaemonLaunchPlan>) {
        let _ = self.tx.send(Cmd::Restart(plan.map(Box::new)));
    }

    /// Stop without restarting.
    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop(None));
    }

    /// Stop without restarting; `completion` runs once the process has exited.
    pub fn stop_then(&self, completion: impl FnOnce() + Send + 'static) {
        let _ = self.tx.send(Cmd::Stop(Some(Box::new(completion))));
    }

    /// Stop and block until the daemon is gone (or `timeout` passes).
    pub fn stop_blocking(&self, timeout: Duration) -> bool {
        let (done_tx, done_rx) = mpsc::channel();
        self.stop_then(move || {
            let _ = done_tx.send(());
        });
        done_rx.recv_timeout(timeout).is_ok()
    }

    pub fn system_will_sleep(&self) {
        let _ = self.tx.send(Cmd::WillSleep);
    }

    pub fn system_did_wake(&self) {
        let _ = self.tx.send(Cmd::DidWake);
    }

    /// Stop a daemon left behind by a crashed shell. Blocking; call before `start`.
    pub fn terminate_orphan(&self) {
        let state_file = self.paths.daemon_state_file();
        let Ok(data) = fs::read(&state_file) else { return };
        let record: DaemonRecord = match serde_json::from_slice(&data) {
            Ok(record) => record,
            Err(_) => {
                let _ = fs::remove_file(&state_file);
                return;
            }
        };
        let _ = fs::remove_file(&state_file);
        let own_pid = std::process::id() as i32;
        if record.pid <= 0 || record.shell_pid == own_pid || !spawn::is_alive(record.pid) {
            return;
        }
        match spawn::command_line(record.pid) {
            Some(command) if command.contains(&record.entry) => {}
            _ => {
                self.log.log(format!(
                    "orphan check: pid {} is no longer a dsh daemon; leaving it alone",
                    record.pid
                ));
                return;
            }
        }
        self.log.log(format!(
            "orphan daemon pid={} port={} from shell pid={}; stopping it",
            record.pid, record.port, record.shell_pid
        ));
        spawn::signal_pid(record.pid, libc::SIGTERM);
        let deadline = Instant::now() + self.stop_grace;
        while spawn::is_alive(record.pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
        if spawn::is_alive(record.pid) {
            spawn::signal_group(record.pid, libc::SIGKILL);
            spawn::signal_pid(record.pid, libc::SIGKILL);
        }
    }
}

impl Drop for DaemonSupervisor {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(handle) = self.worker.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

struct Running {
    pid: i32,
}

struct Worker {
    shared: Arc<Shared>,
    paths: AppPaths,
    log: Arc<FileLogger>,
    output: Arc<FileLogger>,
    policy: SupervisorPolicy,
    tx: Sender<Cmd>,
    rx: Receiver<Cmd>,
    plan: Option<DaemonLaunchPlan>,
    process: Option<Running>,
    generation: u64,
    desired: bool,
    restart_after_exit: bool,
    stop_completions: Vec<Completion>,
    startup_deadline: Option<Instant>,
    kill_deadline: Option<Instant>,
    health_next: Option<Instant>,
    restart_at: Option<Instant>,
    group_kills: Vec<(Instant, i32)>,
    health_in_flight: bool,
    health_failures: u32,
    health_grace_until: Option<Instant>,
    sleeping: bool,
    crash_times: Vec<Instant>,
    attempt: u32,
    current_port: Option<u16>,
    avoid_port: Option<u16>,
    spawned_at: Instant,
}

impl Worker {
    fn run(mut self) {
        loop {
            let timeout = self
                .next_deadline()
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_secs(3600));
            match self.rx.recv_timeout(timeout) {
                Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(command) => self.handle(command),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.fire_due_timers();
        }
        self.shutdown();
    }

    fn handle(&mut self, command: Cmd) {
        match command {
            Cmd::Start(plan) => {
                self.plan = Some(*plan);
                self.desired = true;
                self.crash_times.clear();
                self.attempt = 0;
                if self.process.is_none() {
                    self.restart_at = None;
                    self.spawn();
                }
            }
            Cmd::Restart(plan) => {
                if let Some(plan) = plan {
                    self.plan = Some(*plan);
                }
                if self.plan.is_none() {
                    return;
                }
                self.desired = true;
                self.crash_times.clear();
                self.attempt = 0;
                self.restart_at = None;
                if self.process.is_none() {
                    self.spawn();
                } else {
                    self.restart_after_exit = true;
                    self.request_termination("restart requested");
                }
            }
            Cmd::Stop(completion) => {
                self.desired = false;
                self.restart_after_exit = false;
                self.restart_at = None;
                if let Some(completion) = completion {
                    self.stop_completions.push(completion);
                }
                if self.process.is_none() {
                    if self.state() != DaemonState::Idle {
                        self.transition(DaemonState::Stopped);
                    }
                    self.flush_stop_completions();
                } else {
                    self.request_termination("stop requested");
                }
            }
            Cmd::WillSleep => {
                self.health_failures = 0;
                self.sleeping = true;
                self.health_grace_until = None;
            }
            Cmd::DidWake => {
                self.health_failures = 0;
                self.sleeping = false;
                self.health_grace_until = Some(Instant::now() + self.policy.wake_grace);
                self.log.log(format!(
                    "health watchdog: wake grace window {}s",
                    self.policy.wake_grace.as_secs()
                ));
            }
            Cmd::Line {
                generation,
                is_stdout,
                text,
            } => self.handle_line(&text, is_stdout, generation),
            Cmd::Exited { generation, code } => self.handle_exit(code, generation),
            Cmd::Health { generation, result } => self.evaluate_health(result, generation),
            Cmd::Shutdown => {}
        }
    }

    // MARK: Spawning

    fn spawn(&mut self) {
        let Some(plan) = self.plan.clone() else { return };
        self.attempt += 1;
        self.generation += 1;
        let generation = self.generation;
        self.health_failures = 0;
        self.shared.recent_errors.lock().unwrap().clear();
        self.transition(DaemonState::Starting {
            attempt: self.attempt,
        });

        let start_port = match self.avoid_port {
            Some(avoided) => plan.preferred_port.max(avoided.saturating_add(1)),
            None => plan.preferred_port,
        };
        let Some(port) = probes::first_available_port(start_port, self.policy.port_attempts) else {
            self.fail(format!(
                "no free loopback port in {start_port}..<{}",
                start_port as u32 + self.policy.port_attempts as u32
            ));
            return;
        };
        let initialize = plan.needs_profile_initialization();
        if initialize {
            if let Ok(entries) = fs::read_dir(plan.profile_directory()) {
                // The CLI refuses to initialize over an existing directory; an empty one is an interrupted init.
                if entries.count() > 0 {
                    self.fail(format!(
                        "profile directory exists without package.json: {}",
                        plan.profile_directory().display()
                    ));
                    return;
                }
                let _ = fs::remove_dir(plan.profile_directory());
            }
        }
        self.log.log(format!(
            "daemon start attempt={} port={port} profile={} initialize={initialize} {}",
            self.attempt,
            plan.profile,
            plan.runtime_summary()
        ));
        let arguments = plan.arguments(port, initialize);
        let child = match spawn::spawn(&plan.node, &arguments, &plan.environment, &plan.working_directory) {
            Ok(child) => child,
            Err(error) => {
                self.fail(format!("could not start dsh: {error}"));
                return;
            }
        };
        let mut child = child;
        let pid = child.pid;
        self.process = Some(Running { pid });
        self.current_port = Some(port);
        self.spawned_at = Instant::now();
        self.write_record(pid, port, &plan.entry.to_string_lossy());
        if let Some(stdout) = child.stdout.take() {
            self.attach_reader(stdout, true, generation);
        }
        if let Some(stderr) = child.stderr.take() {
            self.attach_reader(stderr, false, generation);
        }
        let tx = self.tx.clone();
        thread::Builder::new()
            .name("dsh-launcher.daemon-wait".to_owned())
            .spawn(move || {
                let code = child.wait();
                let _ = tx.send(Cmd::Exited { generation, code });
            })
            .expect("waiter thread");
        self.startup_deadline = Some(Instant::now() + self.policy.startup_timeout);
    }

    fn attach_reader(&self, mut pipe: impl Read + Send + 'static, is_stdout: bool, generation: u64) {
        let tx = self.tx.clone();
        thread::Builder::new()
            .name(format!(
                "dsh-launcher.daemon-{}",
                if is_stdout { "out" } else { "err" }
            ))
            .spawn(move || {
                let mut buffer = LineBuffer::default();
                let mut chunk = [0u8; 8 * 1024];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            for text in buffer.append(&chunk[..read]) {
                                let _ = tx.send(Cmd::Line {
                                    generation,
                                    is_stdout,
                                    text,
                                });
                            }
                        }
                    }
                }
                if let Some(text) = buffer.flush() {
                    let _ = tx.send(Cmd::Line {
                        generation,
                        is_stdout,
                        text,
                    });
                }
            })
            .expect("reader thread");
    }

    fn handle_line(&mut self, raw: &str, is_stdout: bool, generation: u64) {
        let line = ready_line::redact(raw);
        self.output
            .log(format!("{} | {line}", if is_stdout { "out" } else { "err" }));
        if generation != self.generation {
            return;
        }
        if !is_stdout {
            let mut errors = self.shared.recent_errors.lock().unwrap();
            errors.push(line);
            let overflow = errors.len().saturating_sub(30);
            if overflow > 0 {
                errors.drain(..overflow);
            }
        }
        if !is_stdout || !matches!(self.state(), DaemonState::Starting { .. }) {
            return;
        }
        let Some(pid) = self.process.as_ref().map(|process| process.pid) else {
            return;
        };
        let Some(url) = ready_line::authenticated_url(raw) else {
            return;
        };
        self.startup_deadline = None;
        self.avoid_port = None;
        let info = DaemonInfo {
            pid,
            port: url.port().or(self.current_port).unwrap_or(0),
            authenticated_url: url,
            started_at: std::time::SystemTime::now() - self.spawned_at.elapsed(),
        };
        self.log.log(format!(
            "daemon ready pid={pid} url={} after {:.1}s",
            info.clean_url(),
            self.spawned_at.elapsed().as_secs_f64()
        ));
        self.transition(DaemonState::Running(info));
        self.health_next = Some(Instant::now() + self.policy.health_interval);
    }

    fn handle_exit(&mut self, code: Option<i32>, generation: u64) {
        if generation != self.generation {
            return;
        }
        let Some(process) = self.process.take() else {
            return;
        };
        self.cancel_timers();
        self.remove_record();
        // Anything the daemon left in its process group goes with it.
        if spawn::signal_group(process.pid, libc::SIGTERM) {
            self.group_kills
                .push((Instant::now() + Duration::from_secs(3), process.pid));
        }
        self.log.log(format!(
            "daemon exited pid={} code={} uptime={}s",
            process.pid,
            code.map(|code| code.to_string())
                .unwrap_or_else(|| "?".to_owned()),
            self.spawned_at.elapsed().as_secs()
        ));

        if self.restart_after_exit {
            self.restart_after_exit = false;
            self.spawn();
            return;
        }
        if !self.desired {
            self.transition(DaemonState::Stopped);
            self.flush_stop_completions();
            return;
        }
        if self
            .shared
            .recent_errors
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("EADDRINUSE"))
        {
            self.avoid_port = self.current_port;
        }
        let now = Instant::now();
        let window = self.policy.crash_window;
        self.crash_times.retain(|time| now.duration_since(*time) < window);
        self.crash_times.push(now);
        if self.crash_times.len() >= self.policy.max_crashes_in_window {
            let summary = self.failure_summary(code);
            self.fail(summary);
            return;
        }
        let index = (self.crash_times.len() - 1).min(self.policy.restart_backoff.len() - 1);
        let delay = self.policy.restart_backoff[index];
        self.log.log(format!(
            "daemon restart in {:.0}s (unexpected exit {}/{})",
            delay.as_secs_f64(),
            self.crash_times.len(),
            self.policy.max_crashes_in_window
        ));
        self.transition(DaemonState::Starting {
            attempt: self.attempt + 1,
        });
        self.restart_at = Some(now + delay);
    }

    fn request_termination(&mut self, reason: &str) {
        let Some(process) = self.process.as_ref() else {
            return;
        };
        let pid = process.pid;
        self.cancel_timers();
        self.transition(DaemonState::Stopping);
        self.log.log(format!("daemon stop pid={pid}: {reason}"));
        spawn::signal_pid(pid, libc::SIGTERM);
        self.kill_deadline = Some(Instant::now() + self.policy.stop_grace);
    }

    // MARK: Watchdogs

    fn next_deadline(&self) -> Option<Instant> {
        [
            self.startup_deadline,
            self.kill_deadline,
            self.health_next,
            self.restart_at,
            self.group_kills.iter().map(|(at, _)| *at).min(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn fire_due_timers(&mut self) {
        let now = Instant::now();
        if self.startup_deadline.is_some_and(|deadline| deadline <= now) {
            self.startup_deadline = None;
            self.fire_startup_timeout();
        }
        if self.kill_deadline.is_some_and(|deadline| deadline <= now) {
            self.kill_deadline = None;
            if let Some(process) = self.process.as_ref() {
                self.log.log(format!(
                    "daemon still running after {}s; killing its process group",
                    self.policy.stop_grace.as_secs()
                ));
                spawn::signal_group(process.pid, libc::SIGKILL);
                spawn::signal_pid(process.pid, libc::SIGKILL);
            }
        }
        if self.health_next.is_some_and(|deadline| deadline <= now) {
            self.health_next = Some(now + self.policy.health_interval);
            self.run_health_check();
        }
        if self.restart_at.is_some_and(|deadline| deadline <= now) {
            self.restart_at = None;
            if self.desired && self.process.is_none() {
                self.spawn();
            }
        }
        let due: Vec<i32> = self
            .group_kills
            .iter()
            .filter(|(at, _)| *at <= now)
            .map(|(_, pid)| *pid)
            .collect();
        self.group_kills.retain(|(at, _)| *at > now);
        for pid in due {
            if spawn::signal_group(pid, 0) {
                spawn::signal_group(pid, libc::SIGKILL);
            }
        }
    }

    fn fire_startup_timeout(&mut self) {
        if !matches!(self.state(), DaemonState::Starting { .. }) {
            return;
        }
        let Some(process) = self.process.as_ref() else {
            return;
        };
        let pid = process.pid;
        let seconds = self.policy.startup_timeout.as_secs();
        self.log.log(format!(
            "daemon not ready after {seconds}s; terminating pid={pid}"
        ));
        self.shared
            .recent_errors
            .lock()
            .unwrap()
            .push(format!("startup timed out after {seconds}s"));
        spawn::signal_pid(pid, libc::SIGTERM);
        self.kill_deadline = Some(Instant::now() + self.policy.stop_grace);
    }

    fn run_health_check(&mut self) {
        if self.health_in_flight {
            return;
        }
        let DaemonState::Running(info) = self.state() else {
            return;
        };
        self.health_in_flight = true;
        let generation = self.generation;
        let timeout = self.policy.health_timeout;
        let tx = self.tx.clone();
        let port = info.port;
        thread::Builder::new()
            .name("dsh-launcher.health".to_owned())
            .spawn(move || {
                let result = probes::health_check(port, timeout);
                let _ = tx.send(Cmd::Health { generation, result });
            })
            .expect("health thread");
    }

    fn evaluate_health(&mut self, result: HealthResult, generation: u64) {
        self.health_in_flight = false;
        if generation != self.generation || !matches!(self.state(), DaemonState::Running(_)) {
            return;
        }
        match result {
            HealthResult::Unhealthy { reason } => {
                let in_grace = self.sleeping
                    || self
                        .health_grace_until
                        .is_some_and(|until| Instant::now() < until);
                if in_grace {
                    self.log.log(format!(
                        "health check failed during grace window; not counted: {reason}"
                    ));
                    return;
                }
                self.health_failures += 1;
                self.log.log(format!(
                    "health check failed ({}/{}): {reason}",
                    self.health_failures, self.policy.max_health_failures
                ));
                if self.health_failures < self.policy.max_health_failures {
                    return;
                }
                self.health_failures = 0;
                self.restart_after_exit = true;
                let reason = format!(
                    "unresponsive to {} health checks",
                    self.policy.max_health_failures
                );
                self.request_termination(&reason);
            }
            HealthResult::Healthy { .. } => {
                if self.health_failures > 0 {
                    self.log.log("health check recovered");
                    self.health_failures = 0;
                }
            }
        }
    }

    // MARK: Helpers

    fn state(&self) -> DaemonState {
        self.shared.state.lock().unwrap().clone()
    }

    fn fail(&mut self, message: String) {
        self.log.log(format!("daemon failed: {message}"));
        self.desired = false;
        self.transition(DaemonState::Failed(message));
        self.flush_stop_completions();
    }

    fn failure_summary(&self, code: Option<i32>) -> String {
        let errors = self.shared.recent_errors.lock().unwrap();
        let last = errors.iter().rev().find(|line| !line.trim().is_empty()).cloned();
        let base = format!(
            "dsh exited {} times in {} min (last code {})",
            self.policy.max_crashes_in_window,
            self.policy.crash_window.as_secs() / 60,
            code.map(|code| code.to_string())
                .unwrap_or_else(|| "?".to_owned())
        );
        match last {
            Some(line) => format!("{base}: {line}"),
            None => base,
        }
    }

    fn transition(&mut self, next: DaemonState) {
        {
            let mut state = self.shared.state.lock().unwrap();
            if *state == next {
                return;
            }
            *state = next.clone();
        }
        if let Some(callback) = self.shared.on_state_change.lock().unwrap().as_ref() {
            callback(next);
        }
    }

    fn flush_stop_completions(&mut self) {
        for completion in std::mem::take(&mut self.stop_completions) {
            completion();
        }
    }

    fn cancel_timers(&mut self) {
        self.startup_deadline = None;
        self.kill_deadline = None;
        self.health_next = None;
        self.health_in_flight = false;
    }

    fn write_record(&self, pid: i32, port: u16, entry: &str) {
        let record = DaemonRecord {
            pid,
            port,
            entry: entry.to_owned(),
            shell_pid: std::process::id() as i32,
            started_at: crate::core::iso8601_now(),
        };
        let Ok(data) = serde_json::to_vec(&record) else {
            return;
        };
        let path = self.paths.daemon_state_file();
        let _ = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, &data));
    }

    fn remove_record(&self) {
        let _ = fs::remove_file(self.paths.daemon_state_file());
    }

    /// Leave no daemon behind when the shell goes away.
    fn shutdown(&mut self) {
        let Some(process) = self.process.take() else {
            return;
        };
        spawn::signal_pid(process.pid, libc::SIGTERM);
        let deadline = Instant::now() + self.policy.stop_grace;
        while spawn::is_alive(process.pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        if spawn::is_alive(process.pid) {
            spawn::signal_group(process.pid, libc::SIGKILL);
            spawn::signal_pid(process.pid, libc::SIGKILL);
        }
        self.remove_record();
    }
}

/// The environment a plan needs at the very least, for callers building one by hand.
pub fn minimal_environment() -> HashMap<String, String> {
    HashMap::from([
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        (
            "HOME".to_owned(),
            crate::core::home_dir().to_string_lossy().into_owned(),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::testing::{temp_dir, TempDir};
    use std::net::TcpListener;
    use std::path::PathBuf;

    /// Stands in for `dsh web`: prints the readiness line and answers HTTP 401.
    const FAKE_DAEMON: &str = r#"
import http.server, os, signal, sys, time
args = sys.argv[1:]
port = int(args[args.index("--port") + 1])
mode = os.environ.get("DSH_FAKE_MODE", "ok")
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
if mode == "crash":
    print("boom: simulated failure", file=sys.stderr, flush=True)
    sys.exit(3)
print("booting fake dsh", flush=True)
if mode == "nohttp":
    print(f"dsh web: http://127.0.0.1:{port}/?token=faketoken", flush=True)
    while True:
        time.sleep(1)
class Handler(http.server.BaseHTTPRequestHandler):
    def do_HEAD(self):
        self.send_response(401)
        self.end_headers()
    do_GET = do_HEAD
    def log_message(self, *_):
        pass
server = http.server.HTTPServer(("127.0.0.1", port), Handler)
print(f"dsh web: http://127.0.0.1:{port}/?token=faketoken", flush=True)
server.serve_forever()
"#;

    const PYTHON: &str = "/usr/bin/python3";

    struct Fixture {
        root: TempDir,
        script: PathBuf,
        states: Arc<Mutex<Vec<DaemonState>>>,
    }

    impl Fixture {
        fn new() -> Fixture {
            let root = temp_dir("supervisor");
            let script = root.join("fake_dsh.py");
            fs::write(&script, FAKE_DAEMON).unwrap();
            Fixture {
                root,
                script,
                states: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn supervisor(
            &self,
            configure: impl FnOnce(&mut SupervisorPolicy),
        ) -> (Arc<DaemonSupervisor>, AppPaths) {
            let paths = AppPaths::new(self.root.join("home"));
            paths.prepare().unwrap();
            let mut policy = SupervisorPolicy {
                startup_timeout: Duration::from_secs(20),
                stop_grace: Duration::from_secs(3),
                restart_backoff: vec![Duration::from_millis(50)],
                ..SupervisorPolicy::default()
            };
            configure(&mut policy);
            let supervisor = DaemonSupervisor::new(
                paths.clone(),
                Arc::new(FileLogger::quiet(paths.shell_log())),
                Arc::new(FileLogger::quiet(paths.daemon_log())),
                policy,
            );
            let states = self.states.clone();
            supervisor.on_state_change(move |state| states.lock().unwrap().push(state));
            (supervisor, paths)
        }

        fn plan(&self, mode: &str, port: u16) -> DaemonLaunchPlan {
            let mut environment = minimal_environment();
            environment.insert("DSH_FAKE_MODE".to_owned(), mode.to_owned());
            DaemonLaunchPlan {
                node: PathBuf::from(PYTHON),
                entry: self.script.clone(),
                dsh_version: Some("test".to_owned()),
                node_version: None,
                profile: "web".to_owned(),
                dsh_home: self.root.join("dsh-home"),
                preferred_port: port,
                extra_args: Vec::new(),
                environment,
                working_directory: self.root.to_path_buf(),
            }
        }

        /// The last recorded state matching `predicate`, waiting up to `timeout`.
        fn wait(&self, timeout: Duration, predicate: impl Fn(&DaemonState) -> bool) -> Option<DaemonState> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if let Some(state) = self
                    .states
                    .lock()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|state| predicate(state))
                {
                    return Some(state.clone());
                }
                thread::sleep(Duration::from_millis(25));
            }
            None
        }

        fn recorded(&self) -> Vec<DaemonState> {
            self.states.lock().unwrap().clone()
        }
    }

    fn free_port() -> u16 {
        use std::sync::atomic::{AtomicU16, Ordering};
        static NEXT: AtomicU16 = AtomicU16::new(0);
        let offset = NEXT.fetch_add(37, Ordering::Relaxed) + (std::process::id() % 3000) as u16;
        probes::first_available_port(44_000 + offset, 200).expect("a free port")
    }

    fn running_info(fixture: &Fixture, timeout: Duration) -> DaemonInfo {
        match fixture.wait(timeout, |state| state.info().is_some()) {
            Some(DaemonState::Running(info)) => info,
            _ => panic!("never became ready: {:?}", fixture.recorded()),
        }
    }

    #[test]
    fn starts_serves_and_stops_gracefully() {
        let fixture = Fixture::new();
        let (supervisor, paths) = fixture.supervisor(|_| {});
        let port = free_port();
        supervisor.start(fixture.plan("ok", port));

        let running = running_info(&fixture, Duration::from_secs(20));
        assert_eq!(running.port, port);
        assert_eq!(
            running.authenticated_url.as_str(),
            format!("http://127.0.0.1:{port}/?token=faketoken")
        );
        assert_eq!(
            probes::health_check(port, Duration::from_secs(2)),
            HealthResult::Healthy { status: 401 }
        );
        assert!(paths.daemon_state_file().exists());

        assert!(
            supervisor.stop_blocking(Duration::from_secs(10)),
            "stop completed"
        );
        assert_eq!(supervisor.current_state(), DaemonState::Stopped);
        assert!(!spawn::is_alive(running.pid));
        assert!(!paths.daemon_state_file().exists());

        let log = fs::read_to_string(paths.daemon_log()).unwrap();
        assert!(log.contains("token=<redacted>"), "{log}");
        assert!(!log.contains("faketoken"), "launch tokens never reach the log");
    }

    #[test]
    fn restart_replaces_the_process() {
        let fixture = Fixture::new();
        let (supervisor, _paths) = fixture.supervisor(|_| {});
        supervisor.start(fixture.plan("ok", free_port()));
        let first = running_info(&fixture, Duration::from_secs(20));
        supervisor.restart(None);
        let second = match fixture.wait(Duration::from_secs(20), |state| {
            state.info().is_some_and(|info| info.pid != first.pid)
        }) {
            Some(DaemonState::Running(info)) => info,
            _ => panic!("never restarted: {:?}", fixture.recorded()),
        };
        assert!(!spawn::is_alive(first.pid));
        assert_eq!(
            second.port, first.port,
            "the port is free again after a clean stop"
        );
        assert!(supervisor.stop_blocking(Duration::from_secs(10)));
    }

    #[test]
    fn falls_back_to_the_next_port_when_taken() {
        let fixture = Fixture::new();
        let (supervisor, _paths) = fixture.supervisor(|_| {});
        let taken = free_port();
        let _blocker = TcpListener::bind(probes::loopback(taken)).expect("blocker");
        supervisor.start(fixture.plan("ok", taken));
        let running = running_info(&fixture, Duration::from_secs(20));
        assert!(running.port > taken, "{} > {taken}", running.port);
        assert!(supervisor.stop_blocking(Duration::from_secs(10)));
    }

    #[test]
    fn crash_loop_ends_in_failed_state_with_stderr() {
        let fixture = Fixture::new();
        let (supervisor, _paths) = fixture.supervisor(|policy| policy.max_crashes_in_window = 3);
        supervisor.start(fixture.plan("crash", free_port()));
        let failure = fixture.wait(Duration::from_secs(20), |state| {
            matches!(state, DaemonState::Failed(_))
        });
        let Some(DaemonState::Failed(message)) = failure else {
            panic!("never failed: {:?}", fixture.recorded());
        };
        assert!(message.contains("boom: simulated failure"), "{message}");
        let attempts = fixture
            .recorded()
            .iter()
            .filter_map(|state| match state {
                DaemonState::Starting { attempt } => Some(*attempt),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        assert!(attempts >= 3, "{:?}", fixture.recorded());
    }

    #[test]
    fn watchdog_restarts_an_unresponsive_daemon() {
        let fixture = Fixture::new();
        let (supervisor, _paths) = fixture.supervisor(|policy| {
            policy.health_interval = Duration::from_millis(200);
            policy.health_timeout = Duration::from_millis(500);
            policy.max_health_failures = 2;
        });
        supervisor.start(fixture.plan("nohttp", free_port()));
        let first = running_info(&fixture, Duration::from_secs(20));
        let replacement = fixture.wait(Duration::from_secs(20), |state| {
            state.info().is_some_and(|info| info.pid != first.pid)
        });
        assert!(
            matches!(replacement, Some(DaemonState::Running(_))),
            "the watchdog never replaced the daemon: {:?}",
            fixture.recorded()
        );
        assert!(supervisor.stop_blocking(Duration::from_secs(10)));
    }
}
