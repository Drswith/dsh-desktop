//! 拉起本地 `dsh web`，从它的 stdout 里找就绪行拿到带 token 的登录地址。
//!
//! 启动前会先看一眼上次留下的记录文件（`<app 数据目录>/dsh.pid`）：如果里面
//! 记的进程还活着、看着像是 dsh，就先杀掉再起新的——这样上次被强制退出、崩溃
//! 遗留下来的 dsh 不会一直占着端口。自己把子进程杀干净之后会删掉这个文件；
//! 拿不到应用数据目录（少见）就跳过这一整套，不影响本次正常使用。
//!
//! Unix 上会把 `dsh` 放进独立进程组，正常的 shell/script/worker 子进程会随组
//! 一起清理；显式 daemonize、创建新 session 的进程仍可能主动逃逸。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::{fs, thread};

use sysinfo::{Pid, System};
use tauri::menu::MenuItem;
use tauri::Wry;
use url::Url;

use crate::process_tree::ManagedChild;
use crate::ready_line;

pub struct DshProcess {
    child: Option<ManagedChild>,
    ready_url: Arc<Mutex<Option<Url>>>,
    record_path: Option<PathBuf>,
}

impl DshProcess {
    /// 生成 `dsh web` 子进程；后台线程读它的 stdout，找到就绪行后把
    /// `open_item` 从禁用的「启动中…」切换成可点的「打开 DSH」。
    ///
    /// `record_path` 给了的话，会先按它清一次上次遗留的孤儿进程，再把这次的
    /// pid/端口写进去，方便下次启动时找到这次（如果这次也没能正常退出）。
    pub fn spawn(port: u16, open_item: MenuItem<Wry>, record_path: Option<PathBuf>) -> std::io::Result<Self> {
        if let Some(path) = &record_path {
            kill_stale_orphan(path);
        }

        let mut command = Command::new("dsh");
        command
            .args([
                "web",
                "--no-open",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        let mut child = ManagedChild::spawn(&mut command)?;

        if let Some(path) = &record_path {
            write_record(
                path,
                child.id(),
                port,
                child.process_group_id(),
                process_start_time(child.id()),
            );
        }

        let stdout = child.take_stdout().expect("stdout 已经设成 piped");
        let ready_url = Arc::new(Mutex::new(None));
        let watcher_url = Arc::clone(&ready_url);

        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(url) = ready_line::authenticated_url(&line) {
                    *watcher_url.lock().unwrap() = Some(url);
                    let _ = open_item.set_text("打开 DSH");
                    let _ = open_item.set_enabled(true);
                    break; // 只要第一条就绪行，不用继续占着这个线程
                }
            }
        });

        Ok(DshProcess {
            child: Some(child),
            ready_url,
            record_path,
        })
    }

    /// 就绪后的登录地址；还没就绪（或者一直没等到）时是 `None`。
    pub fn url(&self) -> Option<String> {
        self.ready_url.lock().unwrap().as_ref().map(Url::to_string)
    }

    /// 尽力停掉子进程、删掉记录文件；失败也不阻塞应用退出。
    pub fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.terminate();
        }
        if let Some(path) = &self.record_path {
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for DshProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

/// 读记录文件里的 pid：那个进程还活着、名字或命令行里看着像是 dsh，就杀掉。
/// 进程已经不在了，或者 pid 被系统挪去给了别的进程用，就什么都不做。
fn kill_stale_orphan(record_path: &Path) {
    let Ok(contents) = fs::read_to_string(record_path) else {
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
        return; // 不像我们认识的那个 dsh，不碰
    }

    eprintln!(
        "发现上次遗留的 dsh 进程（pid {}，端口 {}），清理进程树",
        record.pid, record.port
    );
    if record.group_id.is_some() {
        crate::process_tree::terminate_record(record.pid, record.group_id);
    } else {
        // 兼容旧的两行记录，也覆盖 Windows Job Object 创建失败时的回退。
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
        let _ = fs::create_dir_all(parent);
    }
    let group_id = group_id.map_or_else(|| "-".to_owned(), |value| value.to_string());
    let start_time = start_time.map_or_else(|| "-".to_owned(), |value| value.to_string());
    let _ = fs::write(record_path, format!("{pid}\n{port}\n{group_id}\n{start_time}\n"));
}
