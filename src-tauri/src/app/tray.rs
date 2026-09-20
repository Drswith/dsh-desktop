//! The status item itself: its template icon (dimmed while the service is not
//! running, as `appearsDisabled` drew it), its menu, and the Dock icon's
//! visibility and menu.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::image::Image;
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::AppHandle;

use crate::app::menu::{self, MenuModel};

pub const TRAY_ID: &str = "dsh-launcher";

/// The 16pt template icon, at 2x for Retina.
const ICON_BYTES: &[u8] = include_bytes!("../../icons/MenuBarIconTemplate@2x.png");

static DOCK_ICON_VISIBLE: AtomicBool = AtomicBool::new(true);

thread_local! {
    /// The Dock menu must outlive the call that hands its `NSMenu` to AppKit.
    static DOCK_MENU: RefCell<Option<muda::Menu>> = const { RefCell::new(None) };
}

pub fn install(app: &AppHandle, model: &MenuModel) -> tauri::Result<()> {
    let menu = menu::build_tray_menu(app, model)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon(model.is_running()))
        .icon_as_template(true)
        .tooltip(model.tooltip())
        .menu(&menu)
        .show_menu_on_left_click(true)
        .build(app)?;
    app.set_menu(menu::build_app_menu(app, model)?)?;
    Ok(())
}

/// Rebuild the menus and the icon from the model. Main thread only.
pub fn refresh(app: &AppHandle, model: &MenuModel) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        match menu::build_tray_menu(app, model) {
            Ok(menu) => {
                let _ = tray.set_menu(Some(menu));
            }
            Err(error) => eprintln!("tray menu could not be rebuilt: {error}"),
        }
        apply_icon(&tray, model.is_running());
        let _ = tray.set_tooltip(Some(model.tooltip()));
    }
    if let Ok(menu) = menu::build_app_menu(app, model) {
        let _ = app.set_menu(menu);
    }
}

fn apply_icon(tray: &TrayIcon, running: bool) {
    let _ = tray.set_icon(Some(icon(running)));
    let _ = tray.set_icon_as_template(true);
}

/// A template icon carries only alpha, so dimming it is a matter of scaling that
/// channel — the same look AppKit's `appearsDisabled` gives a status item.
fn icon(running: bool) -> Image<'static> {
    let image = Image::from_bytes(ICON_BYTES).expect("the menu bar icon is part of the binary");
    if running {
        return image.to_owned();
    }
    let mut rgba = image.rgba().to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[3] = (pixel[3] as u16 * 2 / 5) as u8;
    }
    Image::new_owned(rgba, image.width(), image.height())
}

pub fn dock_icon_is_visible() -> bool {
    DOCK_ICON_VISIBLE.load(Ordering::SeqCst)
}

pub fn set_dock_icon_visible(app: &AppHandle, visible: bool) {
    let policy = if visible {
        tauri::ActivationPolicy::Regular
    } else {
        tauri::ActivationPolicy::Accessory
    };
    let _ = app.set_activation_policy(policy);
    DOCK_ICON_VISIBLE.store(visible, Ordering::SeqCst);
}

/// Build the Dock menu and hand AppKit its `NSMenu`. Main thread only.
pub fn dock_menu_handle(model: &MenuModel) -> *mut c_void {
    DOCK_MENU.with(|slot| {
        let built = menu::build_dock_menu(model);
        let handle = {
            use muda::ContextMenu;
            built.ns_menu()
        };
        *slot.borrow_mut() = Some(built);
        handle
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_bar_icon_decodes_and_dims() {
        let running = icon(true);
        let idle = icon(false);
        assert_eq!((running.width(), running.height()), (idle.width(), idle.height()));
        let alpha = |image: &Image<'_>| {
            image
                .rgba()
                .iter()
                .skip(3)
                .step_by(4)
                .map(|a| *a as u64)
                .sum::<u64>()
        };
        assert!(alpha(&running) > 0, "the template icon carries an alpha channel");
        assert!(
            alpha(&idle) < alpha(&running),
            "a stopped service dims the status item"
        );
    }
}
