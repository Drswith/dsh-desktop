//! The macOS pieces Tauri does not cover: native alerts, the login item,
//! sleep/wake notices, the Dock menu, and the quit paths AppKit routes through
//! the app delegate.

#![allow(unexpected_cfgs)]

use std::ffi::c_void;
use std::sync::OnceLock;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{class, msg_send, sel, MainThreadMarker};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSApplicationTerminateReply, NSControlStateValueOn,
    NSModalPanelWindowLevel, NSPasteboard, NSPasteboardTypeString, NSWorkspace,
};
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSArray, NSNotification, NSNotificationCenter,
    NSOperationQueue, NSString, NSUserDefaults,
};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

// Four-character codes from AERegistry.h.
const K_CORE_EVENT_CLASS: u32 = four_char(b"aevt");
const K_AE_QUIT_APPLICATION: u32 = four_char(b"quit");
const K_AE_OPEN_APPLICATION: u32 = four_char(b"oapp");
const K_AE_QUIT_REASON: u32 = four_char(b"why?");
const KEY_AE_PROP_DATA: u32 = four_char(b"prdt");
const KEY_AE_LAUNCHED_AS_LOGIN_ITEM: u32 = four_char(b"lgit");
/// Logout, restart and shutdown; a Dock or Activity Monitor quit carries no reason.
const SYSTEM_QUIT_REASONS: [u32; 6] = [
    four_char(b"logo"),
    four_char(b"rlgo"),
    four_char(b"rrst"),
    four_char(b"rest"),
    four_char(b"rsdn"),
    four_char(b"shut"),
];

const fn four_char(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

pub fn main_thread_marker() -> Option<MainThreadMarker> {
    MainThreadMarker::new()
}

// MARK: Alerts

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertResponse {
    /// Index of the pressed button in the order they were given.
    pub button: usize,
    /// Whether "Don't ask again" was ticked.
    pub suppressed: bool,
}

impl AlertResponse {
    pub fn is_first(&self) -> bool {
        self.button == 0
    }
}

/// A native alert, run modally. The first button is the default one, which
/// AppKit draws on the right.
pub fn alert(
    title: &str,
    body: &str,
    buttons: &[String],
    shows_suppression: bool,
    escape_closes_last: bool,
) -> AlertResponse {
    let Some(mtm) = main_thread_marker() else {
        return AlertResponse {
            button: 1,
            suppressed: false,
        };
    };
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(body));
    for (index, label) in buttons.iter().enumerate() {
        let button = alert.addButtonWithTitle(&NSString::from_str(label));
        if escape_closes_last && index + 1 == buttons.len() && buttons.len() > 1 {
            button.setKeyEquivalent(&NSString::from_str("\u{1b}"));
        }
    }
    alert.setShowsSuppressionButton(shows_suppression);
    // A menu bar app is rarely the frontmost one, and macOS no longer lets it
    // take focus at will, so the alert is raised above other apps' windows
    // instead of waiting unseen behind them.
    let window = alert.window();
    window.setLevel(NSModalPanelWindowLevel);
    window.orderFrontRegardless();
    activate();
    let response = alert.runModal();
    let suppressed = alert
        .suppressionButton()
        .map(|button| button.state() == NSControlStateValueOn)
        .unwrap_or(false);
    AlertResponse {
        button: (response - NSAlertFirstButtonReturn).max(0) as usize,
        suppressed,
    }
}

/// Bring the app forward so a modal alert is not hidden behind other windows.
pub fn activate() {
    let Some(mtm) = main_thread_marker() else { return };
    let app = NSApplication::sharedApplication(mtm);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

// MARK: Pasteboard

pub fn copy_to_pasteboard(text: &str) {
    if main_thread_marker().is_none() {
        return;
    }
    unsafe {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        pasteboard.setString_forType(&NSString::from_str(text), NSPasteboardTypeString);
    }
}

// MARK: Defaults

pub fn default_bool(key: &str, fallback: bool) -> bool {
    let defaults = NSUserDefaults::standardUserDefaults();
    match defaults.objectForKey(&NSString::from_str(key)) {
        Some(_) => defaults.boolForKey(&NSString::from_str(key)),
        None => fallback,
    }
}

pub fn set_default_bool(key: &str, value: bool) {
    NSUserDefaults::standardUserDefaults().setBool_forKey(value, &NSString::from_str(key));
}

// MARK: Language

/// `true` when the user reads Simplified Chinese before English.
pub fn prefers_simplified_chinese() -> bool {
    unsafe {
        let languages: Retained<NSArray<NSString>> = msg_send![class!(NSLocale), preferredLanguages];
        let Some(first) = languages.firstObject() else {
            return false;
        };
        let tag = first.to_string().to_lowercase();
        tag.starts_with("zh-hans")
            || tag.starts_with("zh-cn")
            || tag.starts_with("zh-sg")
            || tag == "zh"
            || tag.starts_with("zh-hans-")
    }
}

// MARK: Launch at login

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchAtLoginStatus {
    Enabled,
    RequiresApproval,
    Disabled,
}

pub mod launch_at_login {
    use super::*;

    pub fn status() -> LaunchAtLoginStatus {
        let status = unsafe { SMAppService::mainAppService().status() };
        match status {
            SMAppServiceStatus::Enabled => LaunchAtLoginStatus::Enabled,
            SMAppServiceStatus::RequiresApproval => LaunchAtLoginStatus::RequiresApproval,
            _ => LaunchAtLoginStatus::Disabled,
        }
    }

    pub fn set_enabled(enabled: bool) -> Result<(), String> {
        let service = unsafe { SMAppService::mainAppService() };
        let outcome = unsafe {
            if enabled {
                service.registerAndReturnError()
            } else {
                service.unregisterAndReturnError()
            }
        };
        outcome.map_err(|error| error.localizedDescription().to_string())
    }

    pub fn open_system_settings() {
        unsafe { SMAppService::openSystemSettingsLoginItems() };
    }
}

// MARK: Workspace

/// Open a URL or file with the user's default handler.
pub fn open_url(url: &str) {
    let Some(url) = objc2_foundation::NSURL::URLWithString(&NSString::from_str(url)) else {
        return;
    };
    NSWorkspace::sharedWorkspace().openURL(&url);
}

/// Reveal files in Finder, selecting them.
pub fn reveal_in_finder(paths: &[std::path::PathBuf]) {
    {
        let urls: Vec<Retained<objc2_foundation::NSURL>> = paths
            .iter()
            .filter_map(|path| {
                objc2_foundation::NSURL::URLWithString(&NSString::from_str(&format!(
                    "file://{}",
                    percent_encode(&path.to_string_lossy())
                )))
            })
            .collect();
        if urls.is_empty() {
            return;
        }
        let array = NSArray::from_retained_slice(&urls);
        NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&array);
    }
}

fn percent_encode(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

// MARK: Sleep and wake

/// Observe the workspace sleep/wake notices for the watchdog's grace windows.
pub fn observe_sleep_wake(on_sleep: impl Fn() + 'static, on_wake: impl Fn() + 'static) {
    unsafe {
        let center: Retained<NSNotificationCenter> = NSWorkspace::sharedWorkspace().notificationCenter();
        let queue = NSOperationQueue::mainQueue();
        let sleep_block = RcBlock::new(move |_: std::ptr::NonNull<NSNotification>| on_sleep());
        center.addObserverForName_object_queue_usingBlock(
            Some(&NSString::from_str("NSWorkspaceWillSleepNotification")),
            None,
            Some(&queue),
            &sleep_block,
        );
        let wake_block = RcBlock::new(move |_: std::ptr::NonNull<NSNotification>| on_wake());
        center.addObserverForName_object_queue_usingBlock(
            Some(&NSString::from_str("NSWorkspaceDidWakeNotification")),
            None,
            Some(&queue),
            &wake_block,
        );
        // The blocks outlive this call; the observers live as long as the app.
        std::mem::forget(sleep_block);
        std::mem::forget(wake_block);
    }
}

// MARK: Apple events

fn current_apple_event() -> Option<Retained<NSAppleEventDescriptor>> {
    NSAppleEventManager::sharedAppleEventManager().currentAppleEvent()
}

/// Logout, restart and shutdown deliver a quit event carrying a reason; a Dock or
/// Activity Monitor quit carries none, and ⌘Q is no Apple event at all.
pub fn quit_is_system_initiated() -> bool {
    let Some(event) = current_apple_event() else {
        return false;
    };
    if event.eventClass() != K_CORE_EVENT_CLASS || event.eventID() != K_AE_QUIT_APPLICATION {
        return false;
    }
    let Some(reason) = event.attributeDescriptorForKeyword(K_AE_QUIT_REASON) else {
        return false;
    };
    SYSTEM_QUIT_REASONS.contains(&reason.enumCodeValue())
}

/// Whether this launch came from the login item rather than the user.
pub fn is_login_item_launch() -> bool {
    let Some(event) = current_apple_event() else {
        return false;
    };
    if event.eventID() != K_AE_OPEN_APPLICATION {
        return false;
    }
    event
        .paramDescriptorForKeyword(KEY_AE_PROP_DATA)
        .map(|data| data.enumCodeValue() == KEY_AE_LAUNCHED_AS_LOGIN_ITEM)
        .unwrap_or(false)
}

// MARK: App delegate hooks

/// What the app answers when AppKit asks whether it may quit.
pub enum TerminateDecision {
    Now,
    Cancel,
}

type TerminateHandler = Box<dyn Fn() -> TerminateDecision + Send + Sync>;
type DockMenuHandler = Box<dyn Fn() -> *mut c_void + Send + Sync>;

static TERMINATE_HANDLER: OnceLock<TerminateHandler> = OnceLock::new();
static DOCK_MENU_HANDLER: OnceLock<DockMenuHandler> = OnceLock::new();

extern "C-unwind" fn application_should_terminate(
    _this: &AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
) -> NSApplicationTerminateReply {
    match TERMINATE_HANDLER.get().map(|handler| handler()) {
        Some(TerminateDecision::Cancel) => NSApplicationTerminateReply::TerminateCancel,
        _ => NSApplicationTerminateReply::TerminateNow,
    }
}

extern "C-unwind" fn application_dock_menu(
    _this: &AnyObject,
    _cmd: Sel,
    _sender: *mut AnyObject,
) -> *mut c_void {
    DOCK_MENU_HANDLER
        .get()
        .map(|handler| handler())
        .unwrap_or(std::ptr::null_mut())
}

/// Teach the running app delegate two methods the Tauri runtime leaves out:
/// the quit gate and the Dock icon's menu.
///
/// The delegate class is created by the windowing layer and implements neither,
/// so adding them takes effect without replacing anything.
pub fn install_delegate_hooks(
    on_terminate: impl Fn() -> TerminateDecision + Send + Sync + 'static,
    dock_menu: impl Fn() -> *mut c_void + Send + Sync + 'static,
) -> bool {
    let _ = TERMINATE_HANDLER.set(Box::new(on_terminate));
    let _ = DOCK_MENU_HANDLER.set(Box::new(dock_menu));
    let Some(mtm) = main_thread_marker() else {
        return false;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else {
        return false;
    };
    unsafe {
        let class: *const AnyClass = msg_send![&*delegate, class];
        if class.is_null() {
            return false;
        }
        let terminate = objc2::ffi::class_addMethod(
            class as *mut _,
            sel!(applicationShouldTerminate:),
            std::mem::transmute::<
                extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> NSApplicationTerminateReply,
                objc2::runtime::Imp,
            >(application_should_terminate),
            c"Q@:@".as_ptr(),
        );
        let dock = objc2::ffi::class_addMethod(
            class as *mut _,
            sel!(applicationDockMenu:),
            std::mem::transmute::<
                extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> *mut c_void,
                objc2::runtime::Imp,
            >(application_dock_menu),
            c"@@:@".as_ptr(),
        );
        terminate.as_bool() && dock.as_bool()
    }
}

/// Quit the app the way every other quit path does, so one gate answers them all.
///
/// The call is deferred to the next run loop pass on purpose. `terminate:` tears
/// the app down synchronously, and menu, signal and deep-link quits all reach
/// this from inside the windowing layer's own event dispatch, which cannot be
/// re-entered — doing it inline deadlocks the main thread.
pub fn terminate() {
    let Some(mtm) = main_thread_marker() else { return };
    let app = NSApplication::sharedApplication(mtm);
    unsafe {
        let _: () = msg_send![
            &*app,
            performSelector: sel!(terminate:),
            withObject: std::ptr::null_mut::<AnyObject>(),
            afterDelay: 0.0f64,
        ];
    }
}

// MARK: Signals

/// `kill`/`launchctl stop` send SIGTERM, which would otherwise end the app at
/// once and orphan the daemon. Blocking the signals here (before any thread
/// exists) lets one thread accept them and quit in an orderly way.
pub fn block_termination_signals() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

pub fn handle_termination_signals(handler: impl Fn(i32) + Send + 'static) {
    std::thread::Builder::new()
        .name("dsh-launcher.signals".to_owned())
        .spawn(move || unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigaddset(&mut set, libc::SIGTERM);
            libc::sigaddset(&mut set, libc::SIGINT);
            loop {
                let mut signal: i32 = 0;
                if libc::sigwait(&set, &mut signal) == 0 {
                    handler(signal);
                }
            }
        })
        .expect("signal thread");
}

/// `Darwin arm64 24.6.0`, the same shape as VS Code's `os.type() os.arch() os.release()`.
pub fn operating_system() -> String {
    unsafe {
        let mut info: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut info) != 0 {
            return "macOS".to_owned();
        }
        let text = |field: &[libc::c_char]| {
            std::ffi::CStr::from_ptr(field.as_ptr())
                .to_string_lossy()
                .into_owned()
        };
        format!(
            "{} {} {}",
            text(&info.sysname),
            text(&info.machine),
            text(&info.release)
        )
    }
}
