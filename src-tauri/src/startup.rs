//! 短生命周期的启动进度窗口；只承载状态，不加载 DSH 页面或访问 token。
//!
//! 后端保存最新快照，前端先订阅再读取，revision 防止加载期间丢事件/倒退。
//! 窗口关闭或就绪时销毁 Webview；托盘和监督器继续运行，不保留隐藏窗口。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::logging::{redact, LogFile};

const LABEL: &str = "startup";
const EVENT: &str = "startup-progress";
const READY_DISPLAY_TIME: Duration = Duration::from_millis(1200);
const CONTENT_WIDTH: f64 = 440.0;
const MIN_CONTENT_HEIGHT: f64 = 278.0;
const MAX_CONTENT_HEIGHT: f64 = 640.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Phase {
    Initializing,
    Checking,
    Approval,
    Starting,
    Retrying,
    Ready,
    Failed,
    Stopped,
}

impl Phase {
    fn terminal(self) -> bool {
        matches!(self, Self::Ready | Self::Failed | Self::Stopped)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    revision: u64,
    attempt: u64,
    phase: Phase,
    step: u8,
    detail: String,
    elapsed_ms: u64,
    test_build: bool,
}

struct Progress {
    snapshot: Snapshot,
    started: Instant,
}

impl Progress {
    fn new() -> Self {
        Self {
            snapshot: Snapshot {
                revision: 1,
                attempt: 1,
                phase: Phase::Initializing,
                step: 0,
                detail: "正在读取配置并准备外部运行环境…".to_owned(),
                elapsed_ms: 0,
                test_build: cfg!(dsh_launcher_test_build),
            },
            started: Instant::now(),
        }
    }

    fn snapshot(&self) -> Snapshot {
        let mut snapshot = self.snapshot.clone();
        if !snapshot.phase.terminal() {
            snapshot.elapsed_ms = self.started.elapsed().as_millis() as u64;
        }
        snapshot
    }

    fn begin(&mut self) {
        let revision = self.snapshot.revision + 1;
        let attempt = self.snapshot.attempt + 1;
        *self = Self::new();
        self.snapshot.revision = revision;
        self.snapshot.attempt = attempt;
    }

    fn update(&mut self, attempt: u64, phase: Phase, detail: &str) -> bool {
        if attempt != self.snapshot.attempt || matches!(self.snapshot.phase, Phase::Stopped | Phase::Failed) {
            return false;
        }
        self.snapshot.revision += 1;
        self.snapshot.phase = phase;
        self.snapshot.step = match phase {
            Phase::Initializing => 0,
            Phase::Checking | Phase::Approval => 1,
            Phase::Starting | Phase::Retrying => 2,
            Phase::Ready => 3,
            Phase::Failed | Phase::Stopped => self.snapshot.step,
        };
        self.snapshot.detail = redact(detail).chars().take(2048).collect();
        self.snapshot.elapsed_ms = self.started.elapsed().as_millis() as u64;
        true
    }

    fn ready_revision(&self, revision: u64) -> bool {
        self.snapshot.phase == Phase::Ready && self.snapshot.revision == revision
    }
}

pub(crate) struct StartupState {
    progress: Mutex<Progress>,
    window_generation: AtomicU64,
    page_loaded: AtomicBool,
    log: LogFile,
}

/// 必须在 setup 最早阶段调用，在登录 shell / pnpm 等耗时工作之前创建窗口。
pub(crate) fn install(app: &AppHandle, log: LogFile) {
    app.manage(StartupState {
        progress: Mutex::new(Progress::new()),
        window_generation: AtomicU64::new(0),
        page_loaded: AtomicBool::new(false),
        log,
    });
    if let Err(error) = show_on_main(app) {
        log_event(
            app,
            &format!("window creation failed; using tray/notifications: {error}"),
        );
    }
}

fn log_event(app: &AppHandle, message: &str) {
    if let Some(state) = app.try_state::<StartupState>() {
        state.log.log("startup-window", message);
    }
}

fn local_page(url: &url::Url) -> bool {
    let origin = (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
        || (matches!(url.scheme(), "http" | "https") && url.host_str() == Some("tauri.localhost"));
    // Tauri 在 macOS 上会把 index.html 归一化成没有斜杠的 tauri://localhost。
    origin && url.port().is_none() && matches!(url.path(), "" | "/" | "/index.html")
}

fn show_on_main(app: &AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.unminimize()?;
        window.show()?;
        return window.set_focus();
    }
    let title = if cfg!(dsh_launcher_test_build) {
        "DSH Launcher · 测试版"
    } else {
        "DSH Launcher"
    };
    let state = app.state::<StartupState>();
    state.page_loaded.store(false, Ordering::SeqCst);
    let generation = state.window_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title(title)
        .inner_size(CONTENT_WIDTH, MIN_CONTENT_HEIGHT)
        .min_inner_size(360.0, MIN_CONTENT_HEIGHT)
        .resizable(true)
        .maximizable(false)
        .fullscreen(false)
        .center()
        .focused(true)
        .incognito(true)
        .on_navigation(local_page)
        .on_page_load(move |window, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                let app = window.app_handle();
                log_event(app, "page loaded");
                let state = app.state::<StartupState>();
                if state.window_generation.load(Ordering::SeqCst) == generation {
                    state.page_loaded.store(true, Ordering::SeqCst);
                    let snapshot = state.progress.lock().unwrap().snapshot();
                    if snapshot.phase == Phase::Ready {
                        schedule_ready_close(app, snapshot.revision, generation);
                    }
                }
            }
        })
        .build()?;
    window.on_window_event({
        let app = app.clone();
        move |event| {
            match event {
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    // 与“后台继续”同义，显式销毁而不是隐藏。destroy 不会再发 CloseRequested。
                    api.prevent_close();
                    let _ = destroy(&app, "close button");
                }
                tauri::WindowEvent::Destroyed => {
                    let state = app.state::<StartupState>();
                    if state.window_generation.load(Ordering::SeqCst) == generation {
                        state.window_generation.fetch_add(1, Ordering::SeqCst);
                        state.page_loaded.store(false, Ordering::SeqCst);
                    }
                    log_event(&app, "window destroyed (tray and service remain active)");
                }
                _ => {}
            }
        }
    });
    log_event(app, "window created");
    Ok(())
}

/// 与 cc-switch 轻量模式相同的生命周期：destroy WebviewWindow，保留 Rust
/// 状态与托盘，需要时由 show_on_main 重建。不保存隐藏 Webview 或窗口句柄。
fn destroy(app: &AppHandle, reason: &str) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(LABEL) {
        log_event(app, &format!("destroy requested: {reason}"));
        window.destroy().map_err(|error| {
            let message = format!("window destruction failed: {error}");
            log_event(app, &message);
            message
        })?;
    }
    Ok(())
}

pub(crate) fn show(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Err(error) = show_on_main(&handle) {
            log_event(&handle, &format!("window creation failed: {error}"));
        }
    });
}

pub(crate) fn is_visible(app: &AppHandle) -> bool {
    app.get_webview_window(LABEL)
        .is_some_and(|window| window.is_visible().unwrap_or(false))
}

pub(crate) fn attempt(app: &AppHandle) -> u64 {
    app.try_state::<StartupState>()
        .map(|state| state.progress.lock().unwrap().snapshot.attempt)
        .unwrap_or_default()
}

pub(crate) fn begin(app: &AppHandle) {
    if let Some(state) = app.try_state::<StartupState>() {
        let snapshot = {
            let mut progress = state.progress.lock().unwrap();
            progress.begin();
            progress.snapshot()
        };
        let _ = app.emit_to(LABEL, EVENT, snapshot);
    }
    show(app);
}

/// 旧启动会话的异步回调不能覆盖新会话，停止/失败后也不能被迟到的就绪行复活。
pub(crate) fn publish(app: &AppHandle, attempt: u64, phase: Phase, detail: &str) -> bool {
    publish_if(app, attempt, phase, detail, || true)
}

/// 将进程 epoch 校验与进度更新放进同一个临界区。失效后的 reader 不能
/// 在监督器发布 Retrying 之后，再发布同一 attempt 的迟到 Ready。
pub(crate) fn publish_if(
    app: &AppHandle,
    attempt: u64,
    phase: Phase,
    detail: &str,
    valid: impl FnOnce() -> bool,
) -> bool {
    let Some(state) = app.try_state::<StartupState>() else {
        return false;
    };
    let snapshot = {
        let mut progress = state.progress.lock().unwrap();
        if !valid() || !progress.update(attempt, phase, detail) {
            return false;
        }
        progress.snapshot()
    };
    state
        .log
        .log("startup-window", &format!("phase={phase:?} attempt={attempt}"));
    let revision = snapshot.revision;
    let _ = app.emit_to(LABEL, EVENT, snapshot);
    if phase == Phase::Ready && state.page_loaded.load(Ordering::SeqCst) {
        schedule_ready_close(app, revision, state.window_generation.load(Ordering::SeqCst));
    }
    true
}

fn schedule_ready_close(app: &AppHandle, revision: u64, generation: u64) {
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(READY_DISPLAY_TIME);
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            let ready = handle.try_state::<StartupState>().is_some_and(|state| {
                state.window_generation.load(Ordering::SeqCst) == generation
                    && state.progress.lock().unwrap().ready_revision(revision)
            });
            if ready {
                let _ = destroy(&handle, "ready");
            }
        });
    });
}

pub(crate) fn update(app: &AppHandle, phase: Phase, detail: &str) {
    publish(app, attempt(app), phase, detail);
}

fn validate_window(window: &WebviewWindow) -> Result<(), String> {
    if window.label() != LABEL || !local_page(&window.url().map_err(|error| error.to_string())?) {
        return Err("仅启动进度窗口可以调用此操作".to_owned());
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn startup_snapshot(window: WebviewWindow) -> Result<Snapshot, String> {
    validate_window(&window)?;
    let state = window.state::<StartupState>();
    let snapshot = state.progress.lock().unwrap().snapshot();
    Ok(snapshot)
}

fn bounded_content_height(requested: u32, available: f64) -> f64 {
    f64::from(requested).clamp(
        MIN_CONTENT_HEIGHT,
        available.clamp(MIN_CONTENT_HEIGHT, MAX_CONTENT_HEIGHT),
    )
}

/// 只接受内容高度；不能调整其他窗口，不授予前端通用窗口管理权限。
#[tauri::command]
pub(crate) async fn startup_resize(
    window: WebviewWindow,
    height: u32,
    viewport_height: u32,
) -> Result<(), String> {
    validate_window(&window)?;
    let resize = || -> tauri::Result<()> {
        let scale = window.scale_factor()?;
        let inner = window.inner_size()?.to_logical::<f64>(scale);
        // 某些平台的 inner_size 包含标题栏布局占用，而 Webview 的 CSS viewport 不包含。
        // 用实际视口差值校准，避免硬编码 macOS 标题栏高度；输入差值亦须限幅。
        let inset = (inner.height - f64::from(viewport_height)).clamp(0.0, 96.0);
        let outer = window.outer_size()?;
        let chrome_height = (f64::from(outer.height) / scale - inner.height).max(0.0);
        let monitor = window.current_monitor()?;
        let available = monitor.as_ref().map_or(MAX_CONTENT_HEIGHT, |monitor| {
            f64::from(monitor.work_area().size.height) / scale - chrome_height - inset - 24.0
        });
        let height = bounded_content_height(height, available) + inset;
        if (inner.height - height).abs() < 1.0 {
            return Ok(());
        }
        window.set_size(tauri::LogicalSize::new(inner.width, height))?;
        // 展开时保持原位置；仅在超出屏幕工作区时向上收回，避免按钮落到屏幕外。
        if let Some(monitor) = monitor {
            let work = monitor.work_area();
            let position = window.outer_position()?;
            let top = f64::from(work.position.y) + 12.0 * scale;
            let bottom = f64::from(work.position.y) + f64::from(work.size.height) - 12.0 * scale;
            let y = f64::from(position.y)
                .min(bottom - (height + chrome_height) * scale)
                .max(top);
            if (y - f64::from(position.y)).abs() >= 1.0 {
                window.set_position(tauri::PhysicalPosition::new(position.x, y.round() as i32))?;
            }
        }
        Ok(())
    };
    resize().map_err(|error| error.to_string())
}

/// 只提供固定动作，不接收命令、文件路径或 URL，不向 Webview 暴露 DSH token。
#[tauri::command]
pub(crate) async fn startup_action(window: WebviewWindow, action: String) -> Result<(), String> {
    validate_window(&window)?;
    if action == "background" {
        return destroy(window.app_handle(), "background button");
    }
    let app = window.app_handle().clone();
    match action.as_str() {
        "logs" => crate::tray::open_logs(&app),
        "config" => crate::tray::handle_menu_event(&app, "config"),
        "retry" => {
            // 与菜单使用同一套监督器；耗时的终止/读取配置不能堵住 UI 线程。
            tauri::async_runtime::spawn_blocking(move || {
                let state = app.state::<crate::AppState>();
                let _action = state.dsh_actions.lock().unwrap();
                let process = state.dsh.lock().unwrap().clone();
                let progress = app.state::<StartupState>();
                let phase = progress.progress.lock().unwrap().snapshot.phase;
                if !matches!(phase, Phase::Failed | Phase::Stopped) {
                    return Err("当前启动尚未结束".to_owned());
                }
                let process = process.as_ref().ok_or("启动器尚未初始化完成")?;
                process.restart();
                Ok(())
            })
            .await
            .map_err(|error| error.to_string())??;
        }
        "open" => crate::tray::handle_menu_event(&app, "open"),
        _ => return Err("不支持的启动窗口操作".to_owned()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_resize_is_bounded_even_for_untrusted_extreme_sizes() {
        assert_eq!(bounded_content_height(0, 900.0), 278.0);
        assert_eq!(bounded_content_height(440, 900.0), 440.0);
        assert_eq!(bounded_content_height(u32::MAX, 900.0), 640.0);
        assert_eq!(bounded_content_height(640, 480.0), 480.0);
        assert_eq!(bounded_content_height(640, 180.0), 278.0);
    }

    #[test]
    fn snapshot_preserves_real_step_on_failure_and_redacts_token() {
        let mut progress = Progress::new();
        assert!(progress.update(1, Phase::Checking, "checking"));
        assert!(progress.update(1, Phase::Failed, "http://127.0.0.1/?token=secret"));
        let snapshot = progress.snapshot();
        assert_eq!(snapshot.step, 1);
        assert!(!snapshot.detail.contains("secret"));
        assert!(snapshot.detail.contains("<redacted>"));
    }

    #[test]
    fn restart_invalidates_old_events_and_ready_close_timer() {
        let mut progress = Progress::new();
        progress.update(1, Phase::Ready, "ready");
        let old_revision = progress.snapshot.revision;
        assert!(progress.ready_revision(old_revision));
        progress.begin();
        assert!(!progress.ready_revision(old_revision));
        assert!(!progress.update(1, Phase::Ready, "stale"));
        assert!(progress.update(2, Phase::Starting, "new attempt"));
        assert!(progress.snapshot.revision > old_revision);
    }

    #[test]
    fn stopped_attempt_cannot_be_revived_by_late_stdout() {
        let mut progress = Progress::new();
        progress.update(1, Phase::Stopped, "stopped");
        assert!(!progress.update(1, Phase::Ready, "late stdout"));
        assert!(!progress.update(1, Phase::Starting, "late spawn"));
        progress.begin();
        assert!(progress.update(2, Phase::Starting, "manual start"));
    }

    #[test]
    fn navigation_only_allows_the_bundled_page() {
        for url in [
            "tauri://localhost",
            "tauri://localhost/index.html",
            "http://tauri.localhost/",
            "https://tauri.localhost/index.html",
        ] {
            assert!(local_page(&url.parse().unwrap()), "{url}");
        }
        for url in [
            "https://example.com/",
            "http://127.0.0.1:31080/",
            "tauri://localhost/other.html",
            "https://tauri.localhost:8080/",
            "file:///etc/passwd",
        ] {
            assert!(!local_page(&url.parse().unwrap()), "{url}");
        }
    }

    #[test]
    fn error_detail_is_bounded_and_terminal_elapsed_time_is_frozen() {
        let mut progress = Progress::new();
        progress.update(1, Phase::Failed, &"错".repeat(3000));
        let snapshot = progress.snapshot();
        assert_eq!(snapshot.detail.chars().count(), 2048);
        assert_eq!(snapshot.elapsed_ms, progress.snapshot.elapsed_ms);
    }

    #[test]
    fn ipc_snapshot_has_only_the_documented_non_secret_fields() {
        let value = serde_json::to_value(Progress::new().snapshot()).unwrap();
        let mut keys = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "attempt",
                "detail",
                "elapsedMs",
                "phase",
                "revision",
                "step",
                "testBuild"
            ]
        );
    }

    #[test]
    fn capability_does_not_grant_shell_filesystem_or_plugin_access() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        assert_eq!(capability["windows"], serde_json::json!(["startup"]));
        assert_eq!(
            capability["permissions"],
            serde_json::json!(["core:event:allow-listen", "core:event:allow-unlisten"])
        );
        assert!(capability.get("remote").is_none());
    }
}
