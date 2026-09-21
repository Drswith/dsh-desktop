//! DSH Launcher：托盘里跑一个本地 `dsh web`，就绪后点菜单在默认浏览器里打开它。
//!
//! 范围刻意很小——运行时安装、登录启动、崩溃看门狗、多语言这些 Swift 版本
//! 原有的功能都还没搬过来，以后按需再加（参见仓库 README）。托盘和菜单用的是
//! Tauri 自带的 `tray`/`menu` 模块，打开浏览器走官方 `tauri-plugin-opener`，
//! 没有直接调用任何 macOS-only 的 AppKit API，因此可以跨平台编译——但目前只
//! 在 macOS 上实际跑过、点过。

mod dsh;
mod ready_line;
mod tray;

use std::sync::Mutex;

use tauri::Manager;

/// 本机没有别的东西占用时的默认端口；暂时不做占用重试，以后需要再加。
const DEFAULT_PORT: u16 = 31080;

/// 挂在 Tauri 状态里的唯一一份共享数据：`dsh web` 子进程（如果启动成功的话）。
pub struct AppState {
    dsh: Mutex<Option<dsh::DshProcess>>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {
            // 已经有一份在跑了，它自己的托盘图标还在，这里不用做什么。
        }))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handles = tray::build(app.handle())?;

            let dsh_process = match dsh::DshProcess::spawn(DEFAULT_PORT, handles.open_item.clone()) {
                Ok(process) => Some(process),
                Err(error) => {
                    eprintln!("启动 dsh web 失败：{error}（PATH 里要能找到 dsh 命令）");
                    let _ = handles.open_item.set_text("未找到 dsh 命令");
                    None
                }
            };
            app.manage(AppState {
                dsh: Mutex::new(dsh_process),
            });

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
