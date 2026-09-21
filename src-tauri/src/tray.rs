//! 托盘图标 + 菜单：「打开 DSH」在 dsh 就绪前是禁用的「启动中…」，「退出」会先
//! 弹一个确认框，确认后才真的退出。
//!
//! 用的都是 Tauri 自带的 `tray`/`menu` 模块、官方 `tauri-plugin-opener` 和
//! `tauri-plugin-dialog`；Dock 显示策略也通过 Tauri 的跨平台抽象设置。Dock
//! 右键菜单这类纯 macOS 菜单仍未实现。

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::TrayIconBuilder;
use tauri::{image::Image, AppHandle, Manager, Wry};
use tauri_plugin_autostart::ManagerExt as AutoStartManagerExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

use crate::AppState;

/// 菜单建好之后，`lib.rs` 还需要用到「打开 DSH」项；其余动作直接在这里分发。
pub struct TrayHandles {
    pub status_item: MenuItem<Wry>,
    pub runtime_item: MenuItem<Wry>,
    pub open_item: MenuItem<Wry>,
    pub copy_item: MenuItem<Wry>,
    pub start_item: MenuItem<Wry>,
    pub stop_item: MenuItem<Wry>,
    pub restart_item: MenuItem<Wry>,
}

pub fn build(app: &AppHandle, launch_at_login_enabled: bool) -> tauri::Result<TrayHandles> {
    let status_item = MenuItem::with_id(app, "status", "状态：启动中…", false, None::<&str>)?;
    let runtime_item = MenuItem::with_id(app, "runtime", "运行时：读取中…", false, None::<&str>)?;
    let open_item = MenuItem::with_id(app, "open", "启动中…", false, None::<&str>)?;
    let copy_item = MenuItem::with_id(app, "copy", "复制访问链接", false, None::<&str>)?;
    let start_item = MenuItem::with_id(app, "start", "启动", true, None::<&str>)?;
    let stop_item = MenuItem::with_id(app, "stop", "停止", true, None::<&str>)?;
    let restart_item = MenuItem::with_id(app, "restart", "重启", true, None::<&str>)?;
    let config_item = MenuItem::with_id(app, "config", "编辑配置…", true, None::<&str>)?;
    let logs_item = MenuItem::with_id(app, "logs", "打开日志", true, None::<&str>)?;
    let dsh_home_item = MenuItem::with_id(app, "dsh-home", "打开 DSH 数据目录", true, None::<&str>)?;
    let login_items_item = MenuItem::with_id(app, "login-items", "打开登录项设置", true, None::<&str>)?;
    let launch_at_login_item = CheckMenuItem::with_id(
        app,
        "launch-at-login",
        "登录时启动",
        true,
        launch_at_login_enabled,
        None::<&str>,
    )?;
    let dock_item = CheckMenuItem::with_id(
        app,
        "dock-icon",
        "隐藏 Dock 图标",
        cfg!(target_os = "macos"),
        true,
        None::<&str>,
    )?;
    let about_item = MenuItem::with_id(app, "about", "关于 DSH Launcher", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status_item,
            &runtime_item,
            &PredefinedMenuItem::separator(app)?,
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
            &login_items_item,
            &launch_at_login_item,
            &dock_item,
            &PredefinedMenuItem::separator(app)?,
            &about_item,
            &quit_item,
        ],
    )?;

    // Swift 版的 macOS 应用菜单和 Edit 菜单。即使当前没有 WebView 窗口，
    // Tauri 仍然可以把它们注册为 app-wide menu；后续增加窗口时也会自动继承。
    let app_about_item = MenuItem::with_id(app, "app-about", "关于 DSH Launcher", true, None::<&str>)?;
    let app_quit_item = MenuItem::with_id(app, "app-quit", "退出 DSH Launcher", true, None::<&str>)?;
    let app_menu = Submenu::with_items(
        app,
        "DSH Launcher",
        true,
        &[
            &app_about_item,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &app_quit_item,
        ],
    )?;
    let edit_menu = Submenu::with_items(
        app,
        "编辑",
        true,
        &[
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    app.set_menu(Menu::with_items(app, &[&app_menu, &edit_menu])?)?;

    let launch_at_login_for_events = launch_at_login_item.clone();
    let dock_for_events = dock_item.clone();
    let mut builder = TrayIconBuilder::with_id("main")
        .icon(tray_icon())
        .menu(&menu)
        .tooltip("DSH Launcher")
        .on_menu_event(move |app, event| {
            if event.id().as_ref() == "launch-at-login" {
                toggle_launch_at_login(app, &launch_at_login_for_events);
            } else if event.id().as_ref() == "dock-icon" {
                toggle_dock_icon(app, &dock_for_events);
            } else {
                handle_menu_event(app, event.id().as_ref());
            }
        });

    // macOS 把不带颜色、只有 alpha 通道的「模板图」按亮暗菜单栏自动上色；
    // 其它平台没有这个概念，用带色图标即可。
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon_as_template(true);
    }

    builder.build(app)?;

    Ok(TrayHandles {
        status_item,
        runtime_item,
        open_item,
        copy_item,
        start_item,
        stop_item,
        restart_item,
    })
}

pub fn handle_menu_event(app: &AppHandle, id: &str) {
    match id {
        "open" => {
            let Some(state) = app.try_state::<AppState>() else {
                return;
            };
            let dsh_guard = state.dsh.lock().unwrap();
            let Some(dsh) = dsh_guard.as_ref() else {
                return;
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
        "login-items" => open_login_items(app),
        "about" | "app-about" => show_about(app),
        "quit" | "app-quit" => confirm_quit(app),
        _ => {}
    }
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
    open_config_file(app);
}

pub(crate) fn show_startup_error(app: &AppHandle, error: &str) {
    let missing_runner = error.contains("No such file") || error.contains("找不到");
    let message = if missing_runner {
        "找不到外部 Runner 命令。\n\n本项目默认通过 pnpm dlx 启动固定版本的 @deepseek-ai/dsh，不要求全局安装 dsh。请确认 Node.js 和 pnpm 已安装，并且 pnpm 在登录 shell 的 PATH 中；也可以在 ~/.dsh-launcher/config.json 的 runner.command 中填写 pnpm 的绝对路径。\n\n启动器不会安装或升级 runtime。"
    } else {
        "DSH Runner 启动失败。\n\n启动器会继续保留托盘入口，你可以修正 runner 配置后点击“重启”。"
    };
    let app = app.clone();
    app.dialog()
        .message(format!("{message}\n\n详细原因：{error}"))
        .title("DSH 启动失败")
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "编辑配置".to_owned(),
            "关闭".to_owned(),
        ))
        .show(move |open_config| {
            if open_config {
                open_config_file(&app);
            }
        });
}

pub(crate) fn confirm_build_permissions(
    app: &AppHandle,
    package: &str,
    dependencies: &[String],
    on_result: impl FnOnce(bool) + Send + 'static,
) {
    let dependency_list = dependencies
        .iter()
        .map(|dependency| format!("• {dependency}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.dialog()
        .message(format!(
            "固定 DSH 包 {package} 请求执行以下依赖的构建脚本：\n\n{dependency_list}\n\n这些脚本来自第三方 npm 依赖。允许后会写入配置，并仅对当前精确 DSH 包版本复用。"
        ))
        .title("需要确认 DSH 构建许可")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "允许并记住".to_owned(),
            "取消".to_owned(),
        ))
        .show(on_result);
}

/// 只用于关键生命周期节点。托盘状态仍然是完整、可回看的状态源，通知不承载
/// watchdog 每次重试的细节，避免后台重试时连续打扰用户。
pub(crate) fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(error) = app.notification().builder().title(title).body(body).show() {
        eprintln!("发送系统通知失败：{error}");
    }
}

fn open_config_file(app: &AppHandle) {
    let home = app
        .path()
        .home_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let path = crate::config::AppPaths::new(home).config;
    if let Err(error) = crate::config::ShellConfig::ensure_file(&path) {
        show_config_error(app, &path, &error);
        return;
    }
    if let Err(error) = app.opener().open_path(path.display().to_string(), None::<&str>) {
        show_config_error(app, &path, &format!("打开配置文件失败：{error}"));
    }
}

pub(crate) fn show_config_error(app: &AppHandle, path: &std::path::Path, error: &str) {
    app.dialog()
        .message(format!(
            "无法读取配置文件：{}\n\n{}\n\n将继续使用默认配置。",
            path.display(),
            error
        ))
        .title("配置错误")
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::Ok)
        .show(|_| {});
}

pub(crate) fn open_logs(app: &AppHandle) {
    let home = app
        .path()
        .home_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let path = crate::config::AppPaths::new(home).logs;
    let files = [path.join("launcher.log"), path.join("dsh.log")];
    let existing = files.iter().filter(|file| file.exists()).collect::<Vec<_>>();
    let result = if existing.is_empty() {
        app.opener().open_path(path.display().to_string(), None::<&str>)
    } else {
        app.opener().reveal_items_in_dir(existing)
    };
    if let Err(error) = result {
        eprintln!("打开日志目录失败：{error}");
    }
}

fn open_login_items(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let target = "x-apple.systempreferences:com.apple.LoginItems-Settings";
    #[cfg(windows)]
    let target = "ms-settings:startupapps";
    #[cfg(all(unix, not(target_os = "macos")))]
    let target = {
        let home = app
            .path()
            .home_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."));
        let path = home.join(".config/autostart");
        if let Err(error) = std::fs::create_dir_all(&path) {
            eprintln!("创建自启动目录失败：{error}");
        }
        return app
            .opener()
            .open_path(path.display().to_string(), None::<&str>)
            .unwrap_or_else(|error| eprintln!("打开自启动目录失败：{error}"));
    };
    if let Err(error) = app.opener().open_url(target, None::<&str>) {
        eprintln!("打开登录项设置失败：{error}");
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

pub(crate) fn show_about(app: &AppHandle) {
    let app = app.clone();
    thread::spawn(move || {
        let details = about_details(&app);
        let dialog_app = app.clone();
        let _ = app.run_on_main_thread(move || show_about_dialog(&dialog_app, details));
    });
}

pub(crate) fn refresh_runtime_summary(app: &AppHandle, item: MenuItem<Wry>) {
    let app = app.clone();
    thread::spawn(move || {
        let summary = runtime_summary(&app);
        let _ = app.run_on_main_thread(move || {
            let _ = item.set_text(summary);
        });
    });
}

fn about_details(app: &AppHandle) -> String {
    let home = app
        .path()
        .home_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let paths = crate::config::AppPaths::new(home.clone());
    let (dsh_version, node_version, pnpm_version) = runtime_versions(&home, &paths.config);
    let commit = env!("DSH_LAUNCHER_GIT_COMMIT");
    let build_date = if env!("DSH_LAUNCHER_BUILD_DATE").is_empty() {
        "未知"
    } else {
        env!("DSH_LAUNCHER_BUILD_DATE")
    };
    format!(
        "Version: {} (build {})\nGitHub: {}\nCommit: {}\nBuilt: {}\nDSH: {}\nNode.js: {}\npnpm: {}\nOS: {} {}",
        env!("DSH_LAUNCHER_VERSION_LABEL"),
        env!("DSH_LAUNCHER_BUILD_NUMBER"),
        env!("DSH_LAUNCHER_REPO_URL"),
        commit,
        build_date,
        dsh_version,
        node_version,
        pnpm_version,
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn runtime_versions(home: &std::path::Path, config_path: &std::path::Path) -> (String, String, String) {
    let config = crate::config::ShellConfig::load(config_path).unwrap_or_default();
    let environment = config.environment(home);
    let node_program = config
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.node.as_deref())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("node");
    let pnpm_program = if config.runner.is_some() || config.runtime.is_none() {
        let runner = config.effective_runner();
        let command_name = std::path::Path::new(&runner.command)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&runner.command)
            .to_ascii_lowercase();
        if command_name == "pnpm" || command_name == "pnpm.exe" {
            runner.command
        } else {
            "pnpm".to_owned()
        }
    } else {
        "pnpm".to_owned()
    };
    let dsh_version = if config.runner.is_some() || config.runtime.is_none() {
        format!("包 {}", config.effective_runner().package)
    } else {
        command_version("dsh", &["--version"], &environment).unwrap_or_else(|| "未找到 dsh 命令".to_owned())
    };
    let node_version = command_version(node_program, &["--version"], &environment)
        .unwrap_or_else(|| "未找到 node 命令".to_owned());
    let pnpm_version = command_version(&pnpm_program, &["--version"], &environment)
        .unwrap_or_else(|| "未找到 pnpm 命令".to_owned());
    (dsh_version, node_version, pnpm_version)
}

fn runtime_summary(app: &AppHandle) -> String {
    let home = app
        .path()
        .home_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let paths = crate::config::AppPaths::new(home.clone());
    let (dsh, node, pnpm) = runtime_versions(&home, &paths.config);
    format!("运行时：外部 · DSH {dsh} · Node.js {node} · pnpm {pnpm}")
}

fn show_about_dialog(app: &AppHandle, details: String) {
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

fn command_version(program: &str, args: &[&str], environment: &BTreeMap<String, String>) -> Option<String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .envs(environment)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Err(_) => return None,
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    for bytes in [&output.stdout, &output.stderr] {
        if let Some(value) = String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .find(|value| !value.is_empty())
        {
            return Some(value.to_owned());
        }
    }
    None
}

fn toggle_launch_at_login(app: &AppHandle, item: &CheckMenuItem<Wry>) {
    let current = app.autolaunch().is_enabled().unwrap_or(false);
    let result = if current {
        app.autolaunch().disable()
    } else {
        app.autolaunch().enable()
    };
    match result {
        Ok(()) => {
            let enabled = !current;
            let _ = item.set_checked(enabled);
            if let Some(state) = app.try_state::<AppState>() {
                state.preferences.set("launchAtLogin", enabled);
                let _ = state.preferences.save();
            }
        }
        Err(error) => {
            let _ = item.set_checked(current);
            app.dialog()
                .message(format!("无法更新登录启动设置：{error}"))
                .title("登录时启动")
                .kind(MessageDialogKind::Error)
                .show(|_| {});
        }
    }
}

fn toggle_dock_icon(app: &AppHandle, item: &CheckMenuItem<Wry>) {
    #[cfg(target_os = "macos")]
    {
        let currently_visible = item.is_checked().unwrap_or(true);
        let policy = if currently_visible {
            tauri::ActivationPolicy::Accessory
        } else {
            tauri::ActivationPolicy::Regular
        };
        match app.set_activation_policy(policy) {
            Ok(()) => {
                let _ = item.set_checked(!currently_visible);
                let _ = item.set_text(if currently_visible {
                    "显示 Dock 图标"
                } else {
                    "隐藏 Dock 图标"
                });
            }
            Err(error) => {
                app.dialog()
                    .message(format!("无法更新 Dock 图标显示设置：{error}"))
                    .title("Dock 图标")
                    .kind(MessageDialogKind::Error)
                    .show(|_| {});
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, item);
    }
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
    let should_confirm = state
        .preferences
        .get("confirmQuit")
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    if !should_confirm {
        app.exit(0);
        return;
    }
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
        .buttons(MessageDialogButtons::YesNoCancelCustom(
            "退出".to_string(),
            "退出且不再询问".to_string(),
            "取消".to_string(),
        ))
        .show_with_result(move |result| {
            if let Some(state) = app.try_state::<AppState>() {
                state.quit_dialog_open.store(false, Ordering::SeqCst);
            }
            match result {
                MessageDialogResult::Yes => app.exit(0),
                MessageDialogResult::No => {
                    if let Some(state) = app.try_state::<AppState>() {
                        state.preferences.set("confirmQuit", false);
                        let _ = state.preferences.save();
                    }
                    app.exit(0);
                }
                MessageDialogResult::Custom(value) if value == "退出" => app.exit(0),
                MessageDialogResult::Custom(value) if value == "退出且不再询问" => {
                    if let Some(state) = app.try_state::<AppState>() {
                        state.preferences.set("confirmQuit", false);
                        let _ = state.preferences.save();
                    }
                    app.exit(0);
                }
                _ => {}
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
