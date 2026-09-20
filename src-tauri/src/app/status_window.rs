//! The small progress/failure window shown while the runtime installs or the
//! service starts after a manual launch.

use serde::Serialize;
use tauri::{AppHandle, LogicalSize, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::app::i18n::tr;

pub const LABEL: &str = "status";

#[derive(Debug, Clone, Serialize)]
pub struct StatusPayload {
    /// `progress` spins; `failure` shows the buttons.
    pub kind: &'static str,
    pub title: String,
    pub detail: String,
    pub retry: String,
    pub logs: String,
    pub close: String,
}

impl StatusPayload {
    fn new(kind: &'static str, title: String, detail: String) -> StatusPayload {
        StatusPayload {
            kind,
            title,
            detail,
            retry: tr("action.retry"),
            logs: tr("action.openLogs"),
            close: tr("action.close"),
        }
    }

    pub fn progress(title: String, detail: String) -> StatusPayload {
        StatusPayload::new("progress", title, detail)
    }

    pub fn failure(title: String, detail: String) -> StatusPayload {
        StatusPayload::new("failure", title, detail)
    }

    /// A first guess only: the page measures its content and resizes the window.
    fn height(&self) -> f64 {
        if self.kind == "failure" {
            200.0
        } else {
            140.0
        }
    }
}

/// Create the window on first use, then reuse it; closing only hides it.
pub fn present(app: &AppHandle, payload: &StatusPayload) {
    let window = match app.get_webview_window(LABEL) {
        Some(window) => window,
        None => {
            let built = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
                .title(tr("window.title"))
                .inner_size(420.0, payload.height())
                .resizable(false)
                .minimizable(false)
                .maximizable(false)
                .always_on_top(true)
                .title_bar_style(tauri::TitleBarStyle::Transparent)
                .hidden_title(true)
                .center()
                .visible(false)
                .build();
            match built {
                Ok(window) => window,
                Err(error) => {
                    eprintln!("status window could not be created: {error}");
                    return;
                }
            }
        }
    };
    let _ = window.set_size(LogicalSize::new(420.0, payload.height()));
    let _ = window.emit_payload(payload);
    let _ = window.show();
    let _ = window.set_focus();
    crate::app::macos::activate();
}

pub fn hide(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.hide();
    }
}

pub fn is_visible(app: &AppHandle) -> bool {
    app.get_webview_window(LABEL)
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

trait EmitPayload {
    fn emit_payload(&self, payload: &StatusPayload) -> tauri::Result<()>;
}

impl EmitPayload for tauri::WebviewWindow {
    fn emit_payload(&self, payload: &StatusPayload) -> tauri::Result<()> {
        use tauri::Emitter;
        self.emit("status", payload)
    }
}
