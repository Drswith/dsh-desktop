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

use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use tauri::Manager;

/// 本机没有别的东西占用时的默认端口；暂时不做占用重试，以后需要再加。
const DEFAULT_PORT: u16 = 31080;

/// 挂在 Tauri 状态里的唯一一份共享数据。
pub struct AppState {
    /// `dsh web` 子进程（如果启动成功的话）。
    dsh: Mutex<Option<dsh::DshProcess>>,
    /// 退出确认框是不是已经弹出来了——连点几下托盘「退出」不该堆出好几个框。
    quit_dialog_open: AtomicBool,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {
            // 已经有一份在跑了，它自己的托盘图标还在，这里不用做什么。
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handles = tray::build(app.handle())?;

            // 记这次 dsh 子进程的 pid，方便下次启动时发现并清理没能正常退出而
            // 遗留下来的孤儿；拿不到应用数据目录（少见）就不记，不影响本次使用。
            let record_path = app.path().app_data_dir().ok().map(|dir| dir.join("dsh.pid"));

            let dsh_process =
                match dsh::DshProcess::spawn(DEFAULT_PORT, handles.open_item.clone(), record_path) {
                    Ok(process) => Some(process),
                    Err(error) => {
                        eprintln!("启动 dsh web 失败：{error}（PATH 里要能找到 dsh 命令）");
                        let _ = handles.open_item.set_text("未找到 dsh 命令");
                        None
                    }
                };
            app.manage(AppState {
                dsh: Mutex::new(dsh_process),
                quit_dialog_open: AtomicBool::new(false),
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
