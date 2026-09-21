//! 拉起并监督本地 dsh，从 stdout 里找就绪行拿到带 token 的登录地址。
//!
//! 监督策略与 Swift 版保持同一组核心边界：启动 180 秒无就绪视为失败；就绪后
//! 每 15 秒探测回环 HTTP，连续 3 次失败重启；异常退出按 1/2/5/10/30 秒退避，
//! 10 分钟内累计 5 次崩溃后熔断。Unix 进程组、Windows Job Object 由
//! `process_tree` 负责，所以 dsh 通过 shell/loop 启动的普通子进程也会一起收掉。

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sysinfo::{Pid, System};
use tauri::menu::MenuItem;
use tauri::{AppHandle, Wry};
use tauri_plugin_opener::OpenerExt;
use url::Url;

use crate::config::ShellConfig;
use crate::logging::{LogFile, LogFiles};
use crate::process_tree::ManagedChild;
use crate::ready_line;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(180);
const HEALTH_INTERVAL: Duration = Duration::from_secs(15);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HEALTH_FAILURES: u32 = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(600);
const RESTART_BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

pub struct DshSpawnOptions {
    pub preferred_port: u16,
    pub port_attempts: u16,
    pub config: ShellConfig,
    pub config_path: PathBuf,
    pub home_dir: PathBuf,
    pub logs: LogFiles,
    pub app: AppHandle,
    pub status_item: MenuItem<Wry>,
    pub open_item: MenuItem<Wry>,
    pub copy_item: MenuItem<Wry>,
    pub start_item: MenuItem<Wry>,
    pub stop_item: MenuItem<Wry>,
    pub restart_item: MenuItem<Wry>,
    pub auto_open: bool,
    pub record_path: Option<PathBuf>,
}

#[derive(Clone)]
struct LaunchSpec {
    preferred_port: u16,
    port_attempts: u16,
    config: Arc<Mutex<ShellConfig>>,
    config_path: PathBuf,
    home_dir: PathBuf,
    logs: LogFiles,
    app: AppHandle,
    status_item: MenuItem<Wry>,
    open_item: MenuItem<Wry>,
    copy_item: MenuItem<Wry>,
    start_item: MenuItem<Wry>,
    stop_item: MenuItem<Wry>,
    restart_item: MenuItem<Wry>,
    auto_open: bool,
    record_path: Option<PathBuf>,
}

pub struct DshProcess {
    child: Arc<Mutex<Option<ManagedChild>>>,
    ready_url: Arc<Mutex<Option<Url>>>,
    desired_running: Arc<AtomicBool>,
    manual_start: Arc<AtomicBool>,
    manual_restart: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    spec: Arc<LaunchSpec>,
    browser_gate: Arc<Mutex<Instant>>,
}

impl DshProcess {
    /// 创建一个持续监督的 dsh。初次 spawn 失败也保留监督器，便于用户修好 PATH
    /// 或配置后通过深链接 `dsh-launcher://start` 重新拉起。
    pub fn spawn(options: DshSpawnOptions) -> Self {
        let spec = Arc::new(LaunchSpec {
            preferred_port: options.preferred_port,
            port_attempts: options.port_attempts,
            config: Arc::new(Mutex::new(options.config)),
            config_path: options.config_path,
            home_dir: options.home_dir,
            logs: options.logs,
            app: options.app,
            status_item: options.status_item,
            open_item: options.open_item,
            copy_item: options.copy_item,
            start_item: options.start_item,
            stop_item: options.stop_item,
            restart_item: options.restart_item,
            auto_open: options.auto_open,
            record_path: options.record_path,
        });
        if let Some(path) = &spec.record_path {
            kill_stale_orphan(path);
        }

        let child = Arc::new(Mutex::new(None));
        let ready_url = Arc::new(Mutex::new(None));
        let desired_running = Arc::new(AtomicBool::new(true));
        let manual_start = Arc::new(AtomicBool::new(false));
        let manual_restart = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(0));
        let browser_gate = Arc::new(Mutex::new(
            Instant::now()
                .checked_sub(Duration::from_secs(3))
                .unwrap_or_else(Instant::now),
        ));

        if let Err(error) = spawn_child(&spec, &child, &ready_url, &generation, &browser_gate) {
            spec.logs
                .launcher
                .log("watchdog", &format!("initial spawn failed: {error}"));
            set_menu(&spec.open_item, "启动失败", false);
            let status = format!("状态：启动失败 · {}", compact_reason(&error.to_string()));
            set_status(&spec.status_item, &status);
            set_controls(&spec, true, false, false);
        }

        let worker_spec = Arc::clone(&spec);
        let worker_child = Arc::clone(&child);
        let worker_ready = Arc::clone(&ready_url);
        let worker_desired = Arc::clone(&desired_running);
        let worker_manual_start = Arc::clone(&manual_start);
        let worker_manual_restart = Arc::clone(&manual_restart);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_generation = Arc::clone(&generation);
        let worker_gate = Arc::clone(&browser_gate);
        thread::Builder::new()
            .name("dsh-watchdog".to_owned())
            .spawn(move || {
                supervise(
                    worker_spec,
                    worker_child,
                    worker_ready,
                    worker_desired,
                    worker_manual_start,
                    worker_manual_restart,
                    worker_shutdown,
                    worker_generation,
                    worker_gate,
                )
            })
            .expect("启动 dsh 看门狗线程失败");

        Self {
            child,
            ready_url,
            desired_running,
            manual_start,
            manual_restart,
            shutdown,
            spec,
            browser_gate,
        }
    }

    /// 菜单手动打开；与 Swift 版一样，自动打开后的短时间内不重复开标签页。
    pub fn open_browser(&self) {
        let Some(url) = self.ready_url.lock().unwrap().clone() else {
            return;
        };
        open_browser_url(&self.spec.app, &url, &self.browser_gate, &self.spec.logs.launcher);
    }

    pub fn start(&self) {
        self.desired_running.store(true, Ordering::SeqCst);
        self.manual_start.store(true, Ordering::SeqCst);
        set_controls(&self.spec, false, true, true);
        self.spec.logs.launcher.log("watchdog", "manual start requested");
    }

    pub fn stop(&self) {
        self.desired_running.store(false, Ordering::SeqCst);
        self.manual_start.store(false, Ordering::SeqCst);
        self.manual_restart.store(false, Ordering::SeqCst);
        terminate_current(&self.child, &self.spec);
        clear_ready(&self.ready_url, &self.spec.open_item, &self.spec.copy_item);
        set_status(&self.spec.status_item, "状态：已停止");
        remove_record(&self.spec.record_path);
        set_controls(&self.spec, true, false, false);
        let _ = self.spec.copy_item.set_enabled(false);
        self.spec.logs.launcher.log("watchdog", "manual stop requested");
    }

    pub fn restart(&self) {
        match ShellConfig::load(&self.spec.config_path) {
            Ok(config) => *self.spec.config.lock().unwrap() = config,
            Err(error) => {
                self.spec
                    .logs
                    .launcher
                    .log("launcher", &format!("reload config failed: {error}"));
                crate::tray::show_config_error(&self.spec.app, &self.spec.config_path, &error);
            }
        }
        self.desired_running.store(true, Ordering::SeqCst);
        self.manual_restart.store(true, Ordering::SeqCst);
        self.manual_start.store(true, Ordering::SeqCst);
        set_controls(&self.spec, false, true, true);
        terminate_current(&self.child, &self.spec);
        clear_ready(&self.ready_url, &self.spec.open_item, &self.spec.copy_item);
        set_status(&self.spec.status_item, "状态：重启中…");
        remove_record(&self.spec.record_path);
        self.spec
            .logs
            .launcher
            .log("watchdog", "manual restart requested");
    }

    pub fn copy_access_link(&self) {
        let Some(url) = self.ready_url.lock().unwrap().clone() else {
            return;
        };
        let text = url.to_string();
        thread::spawn(move || crate::tray::copy_text_to_clipboard(&text));
    }

    /// 应用退出时尽力停掉 dsh、监督线程和进程组；失败也不阻塞退出流程。
    pub fn kill(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.desired_running.store(false, Ordering::SeqCst);
        terminate_current(&self.child, &self.spec);
        remove_record(&self.spec.record_path);
    }
}

impl Drop for DshProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

#[allow(clippy::too_many_arguments)]
fn supervise(
    spec: Arc<LaunchSpec>,
    child: Arc<Mutex<Option<ManagedChild>>>,
    ready_url: Arc<Mutex<Option<Url>>>,
    desired_running: Arc<AtomicBool>,
    manual_start: Arc<AtomicBool>,
    manual_restart: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    browser_gate: Arc<Mutex<Instant>>,
) {
    let mut next_spawn = Instant::now() + Duration::from_secs(1);
    let mut started_at = Instant::now();
    let mut next_health = Instant::now() + HEALTH_INTERVAL;
    let mut health_failures = 0;
    let mut backoff_index = 0;
    let mut crash_times = VecDeque::new();

    while !shutdown.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(200));
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        if manual_start.swap(false, Ordering::SeqCst) {
            backoff_index = 0;
            crash_times.clear();
            next_spawn = Instant::now();
        }

        if !desired_running.load(Ordering::SeqCst) {
            continue;
        }

        let exited = {
            let mut guard = child.lock().unwrap();
            match guard.as_mut() {
                Some(current) => match current.try_wait() {
                    Ok(Some(status)) => Some(
                        status
                            .code()
                            .map_or_else(|| "signal".to_owned(), |code| format!("exit {code}")),
                    ),
                    Ok(None) => None,
                    Err(error) => Some(format!("wait error: {error}")),
                },
                None => None,
            }
        };

        if let Some(reason) = exited {
            terminate_current(&child, &spec);
            clear_ready(&ready_url, &spec.open_item, &spec.copy_item);
            remove_record(&spec.record_path);
            schedule_failure(
                &spec,
                &desired_running,
                &manual_restart,
                &mut next_spawn,
                &mut backoff_index,
                &mut crash_times,
                &format!("dsh exited ({reason})"),
            );
            continue;
        }

        if child.lock().unwrap().is_none() {
            if Instant::now() < next_spawn {
                continue;
            }
            match spawn_child(&spec, &child, &ready_url, &generation, &browser_gate) {
                Ok(now) => {
                    started_at = now;
                    next_health = now + HEALTH_INTERVAL;
                    health_failures = 0;
                }
                Err(error) => {
                    schedule_failure(
                        &spec,
                        &desired_running,
                        &manual_restart,
                        &mut next_spawn,
                        &mut backoff_index,
                        &mut crash_times,
                        &format!("spawn failed: {error}"),
                    );
                }
            }
            continue;
        }

        if ready_url.lock().unwrap().is_none() {
            if started_at.elapsed() >= STARTUP_TIMEOUT {
                spec.logs
                    .launcher
                    .log("watchdog", "startup timeout; restarting dsh");
                terminate_current(&child, &spec);
                clear_ready(&ready_url, &spec.open_item, &spec.copy_item);
                remove_record(&spec.record_path);
                schedule_failure(
                    &spec,
                    &desired_running,
                    &manual_restart,
                    &mut next_spawn,
                    &mut backoff_index,
                    &mut crash_times,
                    "startup timeout",
                );
            }
            continue;
        }

        if Instant::now() >= next_health {
            let url = ready_url.lock().unwrap().clone();
            let healthy = url.as_ref().is_some_and(health_check);
            next_health = Instant::now() + HEALTH_INTERVAL;
            if healthy {
                health_failures = 0;
                backoff_index = 0;
                crash_times.clear();
                spec.logs.launcher.log("watchdog", "health check passed");
            } else {
                health_failures += 1;
                spec.logs.launcher.log(
                    "watchdog",
                    &format!("health check failed ({health_failures}/{MAX_HEALTH_FAILURES})"),
                );
                if health_failures >= MAX_HEALTH_FAILURES {
                    terminate_current(&child, &spec);
                    clear_ready(&ready_url, &spec.open_item, &spec.copy_item);
                    remove_record(&spec.record_path);
                    schedule_failure(
                        &spec,
                        &desired_running,
                        &manual_restart,
                        &mut next_spawn,
                        &mut backoff_index,
                        &mut crash_times,
                        "health check failure",
                    );
                    health_failures = 0;
                }
            }
        }
    }
}

fn schedule_failure(
    spec: &LaunchSpec,
    desired_running: &AtomicBool,
    manual_restart: &AtomicBool,
    next_spawn: &mut Instant,
    backoff_index: &mut usize,
    crash_times: &mut VecDeque<Instant>,
    reason: &str,
) {
    spec.logs.launcher.log("watchdog", reason);
    if !desired_running.load(Ordering::SeqCst) {
        set_menu(&spec.open_item, "已停止", false);
        set_status(&spec.status_item, "状态：已停止");
        set_controls(spec, true, false, false);
        return;
    }
    if manual_restart.swap(false, Ordering::SeqCst) {
        *next_spawn = Instant::now();
        *backoff_index = 0;
        set_menu(&spec.open_item, "启动中…", false);
        set_status(&spec.status_item, "状态：启动中…");
        set_controls(spec, false, true, true);
        return;
    }

    let now = Instant::now();
    while crash_times
        .front()
        .is_some_and(|time| now.duration_since(*time) > CRASH_WINDOW)
    {
        crash_times.pop_front();
    }
    crash_times.push_back(now);
    if crash_times.len() >= 5 {
        desired_running.store(false, Ordering::SeqCst);
        set_menu(&spec.open_item, "启动失败", false);
        let status = format!("状态：启动失败 · {}", compact_reason(reason));
        set_status(&spec.status_item, &status);
        set_controls(spec, true, false, false);
        spec.logs.launcher.log("watchdog", "crash circuit breaker opened");
        return;
    }

    let delay = RESTART_BACKOFF[*backoff_index];
    *backoff_index = (*backoff_index + 1).min(RESTART_BACKOFF.len() - 1);
    *next_spawn = now + delay;
    set_menu(&spec.open_item, "重启中…", false);
    let status = format!("状态：重启中… · {}", compact_reason(reason));
    set_status(&spec.status_item, &status);
    set_controls(spec, false, true, true);
    spec.logs
        .launcher
        .log("watchdog", &format!("retry scheduled in {}s", delay.as_secs()));
}

fn spawn_child(
    spec: &LaunchSpec,
    child_cell: &Arc<Mutex<Option<ManagedChild>>>,
    ready_url: &Arc<Mutex<Option<Url>>>,
    generation: &Arc<AtomicU64>,
    browser_gate: &Arc<Mutex<Instant>>,
) -> std::io::Result<Instant> {
    let config = spec.config.lock().unwrap().clone();
    let preferred_port = config.port.unwrap_or(spec.preferred_port);
    let port = match first_available_port(preferred_port, spec.port_attempts) {
        Ok(port) => port,
        Err(error) => {
            spec.logs
                .launcher
                .log("launcher", &format!("port selection failed: {error}"));
            return Err(error);
        }
    };
    let arguments = config.arguments(port, &spec.home_dir);
    let (executable, entry) = config.runtime_command();
    let mut command = Command::new(&executable);
    if let Some(entry) = entry {
        command.arg(entry);
    }
    command
        .args(&arguments)
        .env_clear()
        .envs(config.environment(&spec.home_dir))
        .current_dir(&spec.home_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    spec.logs.launcher.log(
        "launcher",
        &format!("spawning {executable} profile={} port={port}", config.profile()),
    );
    let mut process = ManagedChild::spawn(&mut command)?;
    if let Some(path) = &spec.record_path {
        write_record(
            path,
            process.id(),
            port,
            process.process_group_id(),
            process_start_time(process.id()),
        );
    }
    let stdout = process.take_stdout().expect("stdout 已经设成 piped");
    let stderr = process.take_stderr().expect("stderr 已经设成 piped");
    let this_generation = generation.fetch_add(1, Ordering::SeqCst) + 1;
    *ready_url.lock().unwrap() = None;
    set_menu(&spec.open_item, "启动中…", false);
    set_status(&spec.status_item, "状态：启动中…");
    set_controls(spec, false, true, true);
    *child_cell.lock().unwrap() = Some(process);

    let watcher_url = Arc::clone(ready_url);
    let watcher_generation = Arc::clone(generation);
    let watcher_gate = Arc::clone(browser_gate);
    let watcher_app = spec.app.clone();
    let watcher_item = spec.open_item.clone();
    let watcher_copy = spec.copy_item.clone();
    let watcher_status = spec.status_item.clone();
    let watcher_log = spec.logs.dsh.clone();
    let launcher_log = spec.logs.launcher.clone();
    let auto_open = spec.auto_open;
    thread::spawn(move || {
        let mut ready = false;
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            watcher_log.log("stdout", &line);
            if ready || watcher_generation.load(Ordering::SeqCst) != this_generation {
                continue;
            }
            if let Some(url) = ready_line::authenticated_url(&line) {
                *watcher_url.lock().unwrap() = Some(url.clone());
                let _ = watcher_item.set_text("打开 DSH");
                let _ = watcher_item.set_enabled(true);
                let _ = watcher_copy.set_enabled(true);
                let _ =
                    watcher_status.set_text(format!("状态：运行中 · 端口 {}", url.port().unwrap_or(port)));
                launcher_log.log(
                    "launcher",
                    &format!("dsh ready port={}", url.port().unwrap_or(port)),
                );
                if auto_open {
                    open_browser_url(&watcher_app, &url, &watcher_gate, &launcher_log);
                }
                ready = true;
            }
        }
    });

    let stderr_log = spec.logs.dsh.clone();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            stderr_log.log("stderr", &line);
        }
    });

    Ok(Instant::now())
}

fn terminate_current(child: &Arc<Mutex<Option<ManagedChild>>>, spec: &LaunchSpec) {
    if let Some(mut current) = child.lock().unwrap().take() {
        spec.logs.launcher.log("launcher", "stopping dsh process group");
        current.terminate();
    }
}

fn clear_ready(ready_url: &Arc<Mutex<Option<Url>>>, open_item: &MenuItem<Wry>, copy_item: &MenuItem<Wry>) {
    *ready_url.lock().unwrap() = None;
    set_menu(open_item, "启动中…", false);
    let _ = copy_item.set_enabled(false);
}

fn compact_reason(reason: &str) -> String {
    const MAX_CHARS: usize = 96;
    let mut result = reason.chars().take(MAX_CHARS).collect::<String>();
    if reason.chars().count() > MAX_CHARS {
        result.push('…');
    }
    result
}

fn set_status(item: &MenuItem<Wry>, text: &str) {
    let _ = item.set_text(text);
}

fn remove_record(record_path: &Option<PathBuf>) {
    if let Some(path) = record_path {
        let _ = std::fs::remove_file(path);
    }
}

fn set_menu(item: &MenuItem<Wry>, text: &str, enabled: bool) {
    let _ = item.set_text(text);
    let _ = item.set_enabled(enabled);
}

fn set_controls(spec: &LaunchSpec, start: bool, stop: bool, restart: bool) {
    let _ = spec.start_item.set_enabled(start);
    let _ = spec.stop_item.set_enabled(stop);
    let _ = spec.restart_item.set_enabled(restart);
}

fn open_browser_url(app: &AppHandle, url: &Url, gate: &Arc<Mutex<Instant>>, log: &LogFile) {
    let Ok(mut last_open) = gate.lock() else { return };
    if last_open.elapsed() < Duration::from_secs(2) {
        return;
    }
    *last_open = Instant::now();
    match app.opener().open_url(url.as_str(), None::<&str>) {
        Ok(()) => log.log("launcher", &format!("opening {}", ready_line::clean_url(url))),
        Err(error) => log.log("launcher", &format!("open browser failed: {error}")),
    }
}

fn first_available_port(start: u16, attempts: u16) -> std::io::Result<u16> {
    if start == 0 || attempts == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "端口范围无效",
        ));
    }
    for offset in 0..attempts {
        let Some(port) = start.checked_add(offset) else {
            break;
        };
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AddrNotAvailable,
        format!("没有可用的回环端口（{start} 起连续尝试 {attempts} 个）"),
    ))
}

fn health_check(url: &Url) -> bool {
    let Some(host) = url.host_str() else { return false };
    let Some(port) = url.port_or_known_default() else {
        return false;
    };
    let address = format!("{host}:{port}");
    let Ok(socket_address) = address.parse() else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&socket_address, HEALTH_TIMEOUT) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(HEALTH_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HEALTH_TIMEOUT));
    let host_header = format!("{host}:{port}");
    if stream
        .write_all(format!("HEAD / HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n\r\n").as_bytes())
        .is_err()
    {
        return false;
    }
    let mut response = [0_u8; 128];
    let Ok(size) = stream.read(&mut response) else {
        return false;
    };
    let line = String::from_utf8_lossy(&response[..size]);
    line.starts_with("HTTP/") && line.as_bytes().get(9).is_some_and(u8::is_ascii_digit)
}

/// 读记录文件里的 pid：那个进程还活着、名字或命令行里看着像是 dsh，就杀掉。
/// 进程已经不在了，或者 pid 被系统挪去给了别的进程用，就什么都不做。
fn kill_stale_orphan(record_path: &Path) {
    let Ok(contents) = std::fs::read_to_string(record_path) else {
        return;
    };
    let Some(record) = ProcessRecord::parse(&contents) else {
        return;
    };

    let system = System::new_all();
    let Some(process) = system.process(Pid::from_u32(record.pid)) else {
        return;
    };

    let name_has_dsh = process.name().to_string_lossy().to_lowercase().contains("dsh");
    let cmd_has_dsh = process
        .cmd()
        .iter()
        .any(|arg| arg.to_string_lossy().to_lowercase().contains("dsh"));
    let start_time_matches = record
        .start_time
        .is_none_or(|expected| expected == process.start_time());
    let group_id_matches = record.group_id.is_none_or(|group_id| group_id == record.pid);
    if (!name_has_dsh && !cmd_has_dsh) || !start_time_matches || !group_id_matches {
        return;
    }

    eprintln!(
        "发现上次遗留的 dsh 进程（pid {}，端口 {}），清理进程树",
        record.pid, record.port
    );
    if record.group_id.is_some() {
        crate::process_tree::terminate_record(record.pid, record.group_id);
    } else {
        let _ = process.kill();
    }
}

fn process_start_time(pid: u32) -> Option<u64> {
    let system = System::new_all();
    system
        .process(Pid::from_u32(pid))
        .map(|process| process.start_time())
}

struct ProcessRecord {
    pid: u32,
    port: u16,
    group_id: Option<u32>,
    start_time: Option<u64>,
}

impl ProcessRecord {
    fn parse(contents: &str) -> Option<Self> {
        let mut lines = contents.lines();
        let pid = lines.next()?.parse().ok()?;
        let port = lines.next()?.parse().ok()?;
        let group_id = lines.next().and_then(|line| line.parse().ok());
        let start_time = lines.next().and_then(|line| line.parse().ok());
        Some(Self {
            pid,
            port,
            group_id,
            start_time,
        })
    }
}

fn write_record(record_path: &Path, pid: u32, port: u16, group_id: Option<u32>, start_time: Option<u64>) {
    if let Some(parent) = record_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let group_id = group_id.map_or_else(|| "-".to_owned(), |value| value.to_string());
    let start_time = start_time.map_or_else(|| "-".to_owned(), |value| value.to_string());
    let _ = std::fs::write(record_path, format!("{pid}\n{port}\n{group_id}\n{start_time}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_forward_ports() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let occupied = listener.local_addr().unwrap().port();
        let selected = first_available_port(occupied, 2).unwrap();
        assert!(selected > occupied);
    }

    #[test]
    fn health_probe_accepts_an_http_response() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 256];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .unwrap();
        });
        let url = Url::parse(&format!("http://127.0.0.1:{port}/?token=test")).unwrap();
        assert!(health_check(&url));
        worker.join().unwrap();
    }
}
