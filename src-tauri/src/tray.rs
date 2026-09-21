//! 托盘图标 + 菜单：「打开 DSH」在 dsh 就绪前是禁用的「启动中…」，「退出」会先
//! 弹一个确认框，确认后才真的退出。
//!
//! 用的都是 Tauri 自带的 `tray`/`menu` 模块、官方 `tauri-plugin-opener` 和
//! `tauri-plugin-dialog`，没有直接调用任何平台原生 API——Dock 右键菜单这类纯
//! macOS 概念暂时没有实现。

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{image::Image, AppHandle, Manager, Wry};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

use crate::AppState;

/// 菜单建好之后，`lib.rs` 还需要用到「打开 DSH」项；其余动作直接在这里分发。
pub struct TrayHandles {
    pub open_item: MenuItem<Wry>,
    pub copy_item: MenuItem<Wry>,
    pub start_item: MenuItem<Wry>,
    pub stop_item: MenuItem<Wry>,
    pub restart_item: MenuItem<Wry>,
}

pub fn build(app: &AppHandle) -> tauri::Result<TrayHandles> {
    let open_item = MenuItem::with_id(app, "open", "启动中…", false, None::<&str>)?;
    let copy_item = MenuItem::with_id(app, "copy", "复制访问链接", false, None::<&str>)?;
    let start_item = MenuItem::with_id(app, "start", "启动", true, None::<&str>)?;
    let stop_item = MenuItem::with_id(app, "stop", "停止", true, None::<&str>)?;
    let restart_item = MenuItem::with_id(app, "restart", "重启", true, None::<&str>)?;
    let config_item = MenuItem::with_id(app, "config", "编辑配置…", true, None::<&str>)?;
    let logs_item = MenuItem::with_id(app, "logs", "打开日志", true, None::<&str>)?;
    let dsh_home_item = MenuItem::with_id(app, "dsh-home", "打开 DSH 数据目录", true, None::<&str>)?;
    let about_item = MenuItem::with_id(app, "about", "关于 DSH Launcher", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open_item,
            &copy_item,
            &PredefinedMenuItem::separator(app)?,
            &start_item,
            &stop_item,
            &restart_item,
            &PredefinedMenuItem::separator(app)?,
            &config_item,
            &logs_item,
            &dsh_home_item,
            &PredefinedMenuItem::separator(app)?,
            &about_item,
            &quit_item,
        ],
    )?;

    let mut builder = TrayIconBuilder::with_id("main")
        .icon(tray_icon())
        .menu(&menu)
        .tooltip("DSH Launcher")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                let Some(state) = app.try_state::<AppState>() else {
                    return;
                };
                let dsh_guard = state.dsh.lock().unwrap();
                let Some(dsh) = dsh_guard.as_ref() else {
                    return; // 还没就绪；正常情况下这时菜单项应该是禁用的
                };
                dsh.open_browser();
            }
            "copy" => with_dsh(app, |dsh| dsh.copy_access_link()),
            "start" => with_dsh(app, |dsh| dsh.start()),
            "stop" => with_dsh(app, |dsh| dsh.stop()),
            "restart" => with_dsh(app, |dsh| dsh.restart()),
            "config" => open_config(app),
            "logs" => open_logs(app),
            "dsh-home" => open_dsh_home(app),
            "about" => show_about(app),
            "quit" => confirm_quit(app),
            _ => {}
        });

    // macOS 把不带颜色、只有 alpha 通道的「模板图」按亮暗菜单栏自动上色；
    // 其它平台没有这个概念，用带色图标即可。
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon_as_template(true);
    }

    builder.build(app)?;

    Ok(TrayHandles {
        open_item,
        copy_item,
        start_item,
        stop_item,
        restart_item,
    })
}

fn with_dsh(app: &AppHandle, action: impl FnOnce(&crate::dsh::DshProcess)) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let guard = state.dsh.lock().unwrap();
    if let Some(dsh) = guard.as_ref() {
        action(dsh);
    }
}

fn open_config(app: &AppHandle) {
    let home = app
        .path()
        .home_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let path = crate::config::AppPaths::new(home).config;
    if let Err(error) = crate::config::ShellConfig::ensure_file(&path) {
        eprintln!("创建配置文件失败：{error}");
        return;
    }
    if let Err(error) = app.opener().open_path(path.display().to_string(), None::<&str>) {
        eprintln!("打开配置文件失败：{error}");
    }
}

fn open_logs(app: &AppHandle) {
    let home = app
        .path()
        .home_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let path = crate::config::AppPaths::new(home).logs;
    if let Err(error) = app.opener().open_path(path.display().to_string(), None::<&str>) {
        eprintln!("打开日志目录失败：{error}");
    }
}

fn open_dsh_home(app: &AppHandle) {
    let home = app
        .path()
        .home_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let paths = crate::config::AppPaths::new(home.clone());
    let dsh_home = crate::config::ShellConfig::load(&paths.config)
        .map(|config| config.dsh_home(&home))
        .unwrap_or_else(|_| home.join(".dsh"));
    if let Err(error) = app
        .opener()
        .open_path(dsh_home.display().to_string(), None::<&str>)
    {
        eprintln!("打开 DSH 数据目录失败：{error}");
    }
}

fn show_about(app: &AppHandle) {
    let details = format!(
        "Version: {}\nGitHub: Drswith/dsh-launcher\nDSH: external command (PATH/config.json)\nOS: {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let copy_text = details.clone();
    app.dialog()
        .message(details)
        .title("DSH Launcher")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "复制".to_owned(),
            "关闭".to_owned(),
        ))
        .show(move |copy| {
            if copy {
                std::thread::spawn(move || copy_text_to_clipboard(&copy_text));
            }
        });
}

pub(crate) fn copy_text_to_clipboard(text: &str) {
    #[cfg(target_os = "macos")]
    let mut child = Command::new("pbcopy").stdin(Stdio::piped()).spawn().ok();

    #[cfg(windows)]
    let mut child = Command::new("cmd")
        .args(["/C", "clip"])
        .stdin(Stdio::piped())
        .spawn()
        .ok();

    #[cfg(all(unix, not(target_os = "macos")))]
    let mut child = {
        let mut selected = None;
        for program in ["wl-copy", "xclip"] {
            let mut command = Command::new(program);
            if program == "xclip" {
                command.args(["-selection", "clipboard"]);
            }
            if let Ok(process) = command.stdin(Stdio::piped()).spawn() {
                selected = Some(process);
                break;
            }
        }
        selected
    };

    if let Some(child) = child.as_mut() {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

/// 弹确认框，确认了才真的退出。用的是非阻塞的 `.show(回调)`：这个事件处理本身
/// 就在主线程上跑，阻塞版 `blocking_show()` 的文档明确说了不能在主线程调用——
/// 之前手写 AppKit 弹窗时在主线程同步等过一次，直接死锁，这次不重蹈覆辙。
///
/// 已经有一个确认框在等用户点的时候，再点一次「退出」不会再弹一个——手工测试
/// 时真的连点出来过好几个摞在一起的确认框，不是假设的场景。
fn confirm_quit(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if state
        .quit_dialog_open
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return; // 已经有一个框在等着了，不用再弹一个
    }

    let app = app.clone();
    app.dialog()
        .message("dsh 服务也会一起停止。")
        .title("退出 DSH Launcher？")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "退出".to_string(),
            "取消".to_string(),
        ))
        .show(move |confirmed| {
            if let Some(state) = app.try_state::<AppState>() {
                state.quit_dialog_open.store(false, Ordering::SeqCst);
            }
            if confirmed {
                app.exit(0);
            }
        });
}

/// 内置的托盘图标：macOS 用沿用自 Swift 版的模板图，其它平台用带色的方形图标。
fn tray_icon() -> Image<'static> {
    #[cfg(target_os = "macos")]
    {
        Image::from_bytes(include_bytes!("../icons/MenuBarIconTemplate@2x.png"))
            .expect("内置托盘图标解码失败")
    }
    #[cfg(not(target_os = "macos"))]
    {
        Image::from_bytes(include_bytes!("../icons/32x32.png")).expect("内置托盘图标解码失败")
    }
}
