//! DSH Launcher：托盘里跑一个本地 dsh Web Profile，就绪后在默认浏览器里打开它。
//!
//! 范围刻意控制在托盘启动器，但已经包含 dsh 看门狗、深链接、持久化配置和登录启动。
//! 运行时安装、多语言和 Dock 菜单这些 Swift 版本原有的功能仍未搬过来（参见仓库 README）。
//! 托盘和菜单用的是
//! Tauri 自带的 `tray`/`menu` 模块，浏览器打开、配置和日志是跨平台 Rust 实现，
//! 没有直接调用任何 macOS-only 的 AppKit API，因此可以跨平台编译——但目前只
//! 在 macOS 上实际跑过、点过。

mod config;
mod dsh;
mod logging;
mod process_tree;
mod ready_line;
mod tray;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as AutoStartManagerExt};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_store::{Store, StoreExt};
use url::Url;

/// Swift 版的默认端口和顺延窗口。
const DEFAULT_PORT: u16 = 31080;
const PORT_ATTEMPTS: u16 = 20;

/// 挂在 Tauri 状态里的唯一一份共享数据。
pub struct AppState {
    /// dsh Web Profile 子进程（如果启动成功的话）。
    dsh: Mutex<Option<dsh::DshProcess>>,
    /// Swift `NSUserDefaults` 的最小替代：保存退出确认和登录启动偏好。
    preferences: Arc<Store<Wry>>,
    /// 退出确认框是不是已经弹出来了——连点几下托盘「退出」不该堆出好几个框。
    quit_dialog_open: AtomicBool,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // Windows/Linux 的第二份进程会把 URL 放进 argv；macOS 则由 deep-link
            // 插件转成 Opened 事件。这里再手动遍历一次，兼容插件版本/平台差异。
            for argument in args {
                if let Ok(url) = Url::parse(&argument) {
                    handle_deep_link(app, &url);
                }
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_store::Builder::default().build())
        .on_menu_event(|app, event| tray::handle_menu_event(app, event.id().as_ref()))
        .setup(|app| {
            let home_dir = app.path().home_dir().unwrap_or_else(|_| PathBuf::from("."));
            let paths = config::AppPaths::new(home_dir.clone());
            let logs = logging::LogFiles::new(&paths.logs);
            let preferences = app.store(paths.preferences.clone())?;
            let launch_at_login_enabled = app.autolaunch().is_enabled().unwrap_or_else(|error| {
                logs.launcher
                    .log("launcher", &format!("读取登录启动状态失败：{error}"));
                preferences
                    .get("launchAtLogin")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
            });
            let handles = tray::build(app.handle(), launch_at_login_enabled)?;
            let config = match config::ShellConfig::load(&paths.config) {
                Ok(config) => config,
                Err(error) => {
                    logs.launcher.log("launcher", &error);
                    config::ShellConfig::default()
                }
            };
            logs.launcher.log(
                "launcher",
                &format!(
                    "starting dsh profile={} preferred_port={}",
                    config.profile(),
                    config.port.unwrap_or(DEFAULT_PORT)
                ),
            );

            // 记这次 dsh 子进程的 pid，方便下次启动时发现并清理没能正常退出而
            // 遗留下来的孤儿；拿不到应用数据目录（少见）就不记，不影响本次使用。
            let record_path = app.path().app_data_dir().ok().map(|dir| dir.join("dsh.pid"));
            let auto_open = std::env::var("DSH_LAUNCHER_NO_OPEN").as_deref() != Ok("1");

            let dsh_process = dsh::DshProcess::spawn(dsh::DshSpawnOptions {
                preferred_port: config.port.unwrap_or(DEFAULT_PORT),
                port_attempts: PORT_ATTEMPTS,
                config,
                config_path: paths.config.clone(),
                home_dir,
                logs,
                app: app.handle().clone(),
                open_item: handles.open_item.clone(),
                copy_item: handles.copy_item.clone(),
                start_item: handles.start_item.clone(),
                stop_item: handles.stop_item.clone(),
                restart_item: handles.restart_item.clone(),
                auto_open,
                record_path,
            });
            app.manage(AppState {
                dsh: Mutex::new(Some(dsh_process)),
                preferences,
                quit_dialog_open: AtomicBool::new(false),
            });

            let app_handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    handle_deep_link(&app_handle, &url);
                }
            });
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                for url in urls {
                    handle_deep_link(app.handle(), &url);
                }
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Tauri 应用初始化失败")
        .run(|app_handle, event| {
            // 退出前尽力把 dsh 子进程停掉，不然它会变成孤儿进程继续占着端口。
            if let tauri::RunEvent::Exit = event {
                if let Some(state) = app_handle.try_state::<AppState>() {
                    if let Some(process) = state.dsh.lock().unwrap().as_mut() {
                        process.kill();
                    }
                }
            }
        });
}

fn handle_deep_link(app: &AppHandle, url: &Url) {
    if url.scheme() != "dsh-launcher" {
        return;
    }
    let action = url
        .host_str()
        .filter(|host| !host.is_empty())
        .or_else(|| url.path_segments().and_then(|mut segments| segments.next()))
        .unwrap_or("open")
        .to_ascii_lowercase();
    if action == "logs" {
        tray::open_logs(app);
        return;
    }
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let guard = state.dsh.lock().unwrap();
    let Some(dsh) = guard.as_ref() else { return };
    match action.as_str() {
        "open" | "" => dsh.open_browser(),
        "start" => dsh.start(),
        "stop" => dsh.stop(),
        "restart" => dsh.restart(),
        _ => {}
    }
}
