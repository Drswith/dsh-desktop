//! 拉起本地 `dsh web`，从它的 stdout 里找就绪行拿到带 token 的登录地址。
//!
//! 只管「启动 + 找地址 + 退出时杀掉」这三件事：运行时安装、端口占用重试、
//! 崩溃重启都还没做，以后需要再加。

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use tauri::menu::MenuItem;
use tauri::Wry;
use url::Url;

use crate::ready_line;

pub struct DshProcess {
    child: Option<Child>,
    ready_url: Arc<Mutex<Option<Url>>>,
}

impl DshProcess {
    /// 生成 `dsh web` 子进程；后台线程读它的 stdout，找到就绪行后把
    /// `open_item` 从禁用的「启动中…」切换成可点的「打开 DSH」。
    pub fn spawn(port: u16, open_item: MenuItem<Wry>) -> std::io::Result<Self> {
        let mut child = Command::new("dsh")
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
            .stdin(Stdio::null())
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout 已经设成 piped");
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
        })
    }

    /// 就绪后的登录地址；还没就绪（或者一直没等到）时是 `None`。
    pub fn url(&self) -> Option<String> {
        self.ready_url.lock().unwrap().as_ref().map(Url::to_string)
    }

    /// 尽力停掉子进程；失败也不阻塞应用退出。
    pub fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for DshProcess {
    fn drop(&mut self) {
        self.kill();
    }
}
