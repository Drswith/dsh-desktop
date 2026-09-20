//! 托盘图标 + 菜单：「打开 DSH」在 dsh 就绪前是禁用的「启动中…」，「退出」直接退出。
//!
//! 用的都是 Tauri 自带的 `tray`/`menu` 模块和官方 `tauri-plugin-opener`，没有直接
//! 调用任何平台原生 API——Dock 右键菜单这类纯 macOS 概念暂时没有实现。

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{image::Image, AppHandle, Manager, Wry};
use tauri_plugin_opener::OpenerExt;

use crate::AppState;

/// 菜单建好之后，`lib.rs` 还需要用到的项——目前只有「打开 DSH」，因为
/// dsh 就绪之后要把它从禁用的占位文字换成可点的。
pub struct TrayHandles {
    pub open_item: MenuItem<Wry>,
}

pub fn build(app: &AppHandle) -> tauri::Result<TrayHandles> {
    let open_item = MenuItem::with_id(app, "open", "启动中…", false, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&open_item, &PredefinedMenuItem::separator(app)?, &quit_item],
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
                let Some(url) = state
                    .dsh
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(crate::dsh::DshProcess::url)
                else {
                    return; // 还没就绪；正常情况下这时菜单项应该是禁用的
                };
                let _ = app.opener().open_url(url, None::<&str>);
            }
            "quit" => app.exit(0),
            _ => {}
        });

    // macOS 把不带颜色、只有 alpha 通道的「模板图」按亮暗菜单栏自动上色；
    // 其它平台没有这个概念，用带色图标即可。
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon_as_template(true);
    }

    builder.build(app)?;

    Ok(TrayHandles { open_item })
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
