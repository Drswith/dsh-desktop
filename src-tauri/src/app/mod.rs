//! Menu bar shell lifecycle: install the bundled runtime, supervise `dsh`, open
//! the Web UI in the default browser, and stop the service when the app quits.

pub mod i18n;
pub mod info;
pub mod macos;
pub mod menu;
pub mod status_window;
pub mod tray;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, RunEvent};

use crate::app::info::AppInfo;
use crate::app::macos::{LaunchAtLoginStatus, TerminateDecision};
use crate::app::menu::{Command, MenuModel};
use crate::app::status_window::StatusPayload;
use crate::core::command::CommandSpec;
use crate::core::daemon::{DaemonInfo, DaemonLaunchPlan, DaemonState, SupervisorPolicy};
use crate::core::installer::{InstalledRuntime, RuntimeInstaller};
use crate::core::logger::FileLogger;
use crate::core::manifest::RuntimeReceipt;
use crate::core::paths::{AppPaths, ShellConfig};
use crate::core::supervisor::DaemonSupervisor;
use crate::core::{shell_env, Error, Result};

mod keys {
    pub const CONFIGURED_LAUNCH_AT_LOGIN: &str = "didConfigureLaunchAtLogin";
    pub const SKIP_QUIT_CONFIRMATION: &str = "skipQuitConfirmation";
    pub const KEEP_PREVIOUS_RUNTIME: &str = "keepPreviousRuntime";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunAction {
    Start,
    Restart,
}

enum Job {
    PrepareAndRun { action: RunAction, reinstall: bool },
    ApplyRetention { keep: bool },
}

pub struct AppState {
    app: AppHandle,
    pub info: AppInfo,
    pub paths: AppPaths,
    pub log: Arc<FileLogger>,
    pub supervisor: Arc<DaemonSupervisor>,
    installer: Mutex<RuntimeInstaller>,
    config: Mutex<ShellConfig>,
    plan: Mutex<Option<DaemonLaunchPlan>>,
    /// Receipt of the bundled runtime in use; `None` for a config.json runtime override.
    runtime_receipt: Mutex<Option<RuntimeReceipt>>,
    model: Mutex<MenuModel>,
    status: Mutex<Option<StatusPayload>>,
    bootstrap: Sender<Job>,
    open_when_ready: AtomicBool,
    show_progress_until_ready: AtomicBool,
    launched_at_login: AtomicBool,
    terminating: AtomicBool,
    /// Set for SIGTERM/SIGINT: nobody is at the screen to answer the confirmation.
    quit_without_confirmation: AtomicBool,
    last_browser_open: Mutex<Instant>,
}

impl AppState {
    fn new(app: &AppHandle) -> Arc<AppState> {
        let info = AppInfo::load(app);
        let paths = AppPaths::standard(info.home_dir_name);
        let _ = paths.prepare();
        let log = Arc::new(FileLogger::new(paths.shell_log()));
        let output = Arc::new(FileLogger::quiet(paths.daemon_log()));
        let supervisor =
            DaemonSupervisor::new(paths.clone(), log.clone(), output, SupervisorPolicy::default());
        let keeps_previous = macos::default_bool(keys::KEEP_PREVIOUS_RUNTIME, true);
        let mut installer = RuntimeInstaller::new(paths.clone(), log.clone(), info.payload_directory.clone());
        installer.keeps_previous_runtime = keeps_previous;

        let mut model = MenuModel::new(info.display_name.clone());
        model.can_repair = installer.bundled_manifest().is_some();
        model.keeps_previous_runtime = keeps_previous;
        model.launch_at_login = macos::launch_at_login::status();

        let (bootstrap, jobs) = mpsc::channel();
        let state = Arc::new(AppState {
            app: app.clone(),
            info,
            paths,
            log,
            supervisor,
            installer: Mutex::new(installer),
            config: Mutex::new(ShellConfig::default()),
            plan: Mutex::new(None),
            runtime_receipt: Mutex::new(None),
            model: Mutex::new(model),
            status: Mutex::new(None),
            bootstrap,
            open_when_ready: AtomicBool::new(false),
            show_progress_until_ready: AtomicBool::new(false),
            launched_at_login: AtomicBool::new(false),
            terminating: AtomicBool::new(false),
            quit_without_confirmation: AtomicBool::new(false),
            last_browser_open: Mutex::new(Instant::now() - Duration::from_secs(60)),
        });
        state.reload_config();
        state.start_bootstrap_worker(jobs);
        state
    }

    /// One worker thread runs installs and retention changes, so they never overlap.
    fn start_bootstrap_worker(self: &Arc<Self>, jobs: mpsc::Receiver<Job>) {
        let state = self.clone();
        std::thread::Builder::new()
            .name("dsh-launcher.bootstrap".to_owned())
            .spawn(move || {
                let mut orphan_checked = false;
                while let Ok(job) = jobs.recv() {
                    match job {
                        Job::PrepareAndRun { action, reinstall } => {
                            state.run_prepare(action, reinstall, &mut orphan_checked)
                        }
                        Job::ApplyRetention { keep } => {
                            let mut installer = state.installer.lock().unwrap();
                            installer.keeps_previous_runtime = keep;
                            installer.apply_retention();
                        }
                    }
                }
            })
            .expect("bootstrap thread");
    }

    fn on_main(self: &Arc<Self>, work: impl FnOnce(Arc<AppState>) + Send + 'static) {
        let state = self.clone();
        let _ = state.app.clone().run_on_main_thread(move || work(state));
    }

    // MARK: Runtime and service

    /// Resolve the runtime (installing the bundled payload when needed), rebuild the
    /// launch plan from the current login-shell environment, then start or restart.
    pub fn prepare_and_run(&self, action: RunAction, reinstall: bool) {
        let _ = self.bootstrap.send(Job::PrepareAndRun { action, reinstall });
    }

    fn run_prepare(self: &Arc<Self>, action: RunAction, reinstall: bool, orphan_checked: &mut bool) {
        match self.prepare(reinstall) {
            Ok(runtime) => {
                if !*orphan_checked {
                    *orphan_checked = true;
                    self.supervisor.terminate_orphan();
                }
                match self.make_plan(runtime.as_ref()) {
                    Ok(plan) => {
                        let receipt = runtime.map(|runtime| runtime.receipt);
                        self.on_main(move |state| state.apply_plan(plan, receipt, action));
                    }
                    Err(error) => self.prepare_failed(error),
                }
            }
            Err(error) => self.prepare_failed(error),
        }
    }

    fn prepare(self: &Arc<Self>, reinstall: bool) -> Result<Option<InstalledRuntime>> {
        self.reload_config();
        if self.config.lock().unwrap().runtime.is_some() {
            return Ok(None);
        }
        let installer = self.installer.lock().unwrap();
        let outcome = if reinstall {
            self.set_installing(true);
            installer.install_bundled()
        } else {
            let state = self.clone();
            installer.ensure_runtime(|_| state.set_installing(true))
        };
        self.set_installing(false);
        outcome.map(Some)
    }

    fn prepare_failed(self: &Arc<Self>, error: Error) {
        self.log.log(format!("prepare failed: {error}"));
        self.set_installing(false);
        let message = error.message().to_owned();
        self.on_main(move |state| {
            state.model.lock().unwrap().state = DaemonState::Failed(message.clone());
            state.refresh_menus();
            state.present_failure(&message);
        });
    }

    fn apply_plan(
        self: &Arc<Self>,
        plan: DaemonLaunchPlan,
        receipt: Option<RuntimeReceipt>,
        action: RunAction,
    ) {
        self.model.lock().unwrap().runtime_summary = Some(plan.runtime_summary());
        *self.plan.lock().unwrap() = Some(plan.clone());
        // The About dialog reports the runtime this plan runs on.
        *self.runtime_receipt.lock().unwrap() = receipt;
        self.refresh_menus();
        if self.show_progress_until_ready.load(Ordering::SeqCst) {
            self.show_progress(&i18n::tr("window.starting"));
        }
        match action {
            RunAction::Start => self.supervisor.start(plan),
            RunAction::Restart => self.supervisor.restart(Some(plan)),
        }
    }

    fn set_installing(self: &Arc<Self>, installing: bool) {
        self.on_main(move |state| {
            {
                let mut model = state.model.lock().unwrap();
                if model.installing == installing {
                    return;
                }
                model.installing = installing;
            }
            state.refresh_menus();
            let quiet = state.launched_at_login.load(Ordering::SeqCst)
                && !state.show_progress_until_ready.load(Ordering::SeqCst);
            if installing && !quiet {
                state.show_progress(&i18n::tr("window.installing"));
            }
        });
    }

    fn make_plan(&self, runtime: Option<&InstalledRuntime>) -> Result<DaemonLaunchPlan> {
        let home = crate::core::home_dir();
        let shell = shell_env::login_shell();
        let login = match shell_env::resolve_login_environment(&shell, Duration::from_secs(10)) {
            Ok(environment) => {
                self.log.log(format!(
                    "login shell environment resolved shell={shell} variables={}",
                    environment.len()
                ));
                Some(environment)
            }
            Err(error) => {
                self.log.log(format!(
                    "login shell environment unavailable ({error}); using the fallback PATH"
                ));
                None
            }
        };
        let current = shell_env::current_environment();
        let config = self.config.lock().unwrap().clone();
        let dsh_home = DaemonLaunchPlan::resolve_dsh_home(
            config.dsh_home.as_deref(),
            login.as_ref().unwrap_or(&current),
            &home,
        );
        let mut overrides: HashMap<String, String> = config.environment.clone().unwrap_or_default();
        if config.dsh_home.is_some() {
            overrides.insert("DSH_HOME".to_owned(), dsh_home.to_string_lossy().into_owned());
        }
        let environment = shell_env::daemon_environment(login.as_ref(), &current, &overrides);

        let mut dsh_version = runtime.map(|runtime| runtime.receipt.dsh_version.clone());
        let (node, entry) = match (&config.runtime, runtime) {
            (Some(external), _) => {
                let node = crate::core::expand_tilde(&external.node);
                let entry = crate::core::expand_tilde(&external.entry);
                if !is_executable(&node) {
                    return Err(Error::new(format!(
                        "config.json runtime.node is not executable: {}",
                        node.display()
                    )));
                }
                if !entry.exists() {
                    return Err(Error::new(format!(
                        "config.json runtime.entry does not exist: {}",
                        entry.display()
                    )));
                }
                dsh_version = package_version_near(&entry);
                (node, entry)
            }
            (None, Some(runtime)) => (runtime.node_executable(), runtime.dsh_entry()),
            (None, None) => return Err(Error::new("no dsh runtime is available")),
        };
        Ok(DaemonLaunchPlan {
            node,
            entry,
            dsh_version,
            node_version: runtime.map(|runtime| runtime.receipt.node_version.clone()),
            profile: config
                .profile
                .clone()
                .unwrap_or_else(|| self.info.profile.to_owned()),
            dsh_home,
            preferred_port: config.port.unwrap_or(self.info.default_port),
            extra_args: config.extra_args.clone().unwrap_or_default(),
            environment,
            working_directory: home,
        })
    }

    fn reload_config(&self) {
        match ShellConfig::load(&self.paths.config_file()) {
            Ok(config) => *self.config.lock().unwrap() = config,
            Err(error) => {
                self.log.log(format!("config.json ignored: {error}"));
                *self.config.lock().unwrap() = ShellConfig::default();
            }
        }
    }

    // MARK: State changes

    fn daemon_state_changed(self: &Arc<Self>, state: DaemonState) {
        self.model.lock().unwrap().state = state.clone();
        self.refresh_menus();
        match state {
            DaemonState::Running(info) => {
                if self.open_when_ready.swap(false, Ordering::SeqCst) {
                    self.open_browser(&info);
                }
                if self.show_progress_until_ready.swap(false, Ordering::SeqCst)
                    || status_window::is_visible(&self.app)
                {
                    self.show_progress(&i18n::tr("window.ready"));
                    let state = self.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(1200));
                        state.on_main(|state| {
                            if state.supervisor.current_state().info().is_some() {
                                status_window::hide(&state.app);
                            }
                        });
                    });
                }
            }
            DaemonState::Failed(message) => {
                self.open_when_ready.store(false, Ordering::SeqCst);
                self.show_progress_until_ready.store(false, Ordering::SeqCst);
                self.present_failure(&message);
            }
            DaemonState::Idle
            | DaemonState::Starting { .. }
            | DaemonState::Stopping
            | DaemonState::Stopped => {}
        }
    }

    fn refresh_menus(self: &Arc<Self>) {
        // The lock is released before rebuilding: AppKit can ask for the Dock menu
        // on this very thread, and a std Mutex is not reentrant.
        let model = {
            let mut model = self.model.lock().unwrap();
            model.launch_at_login = macos::launch_at_login::status();
            model.shows_dock_icon = tray::dock_icon_is_visible();
            model.clone()
        };
        tray::refresh(&self.app, &model);
    }

    fn show_progress(self: &Arc<Self>, detail: &str) {
        let payload = StatusPayload::progress(i18n::tr("window.title"), detail.to_owned());
        *self.status.lock().unwrap() = Some(payload.clone());
        status_window::present(&self.app, &payload);
    }

    fn present_failure(self: &Arc<Self>, message: &str) {
        let tail = self
            .supervisor
            .recent_error_lines()
            .iter()
            .rev()
            .take(4)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        let detail = if tail.is_empty() || message.contains(&tail) {
            message.to_owned()
        } else {
            format!("{message}\n\n{tail}")
        };
        let payload = StatusPayload::failure(i18n::tr("dialog.failed.title"), detail);
        *self.status.lock().unwrap() = Some(payload.clone());
        status_window::present(&self.app, &payload);
    }

    pub fn open_browser(self: &Arc<Self>, daemon: &DaemonInfo) {
        // Menu, Dock and deep-link requests can arrive together; open one tab.
        let mut last = self.last_browser_open.lock().unwrap();
        if last.elapsed() < Duration::from_secs(2) {
            return;
        }
        *last = Instant::now();
        drop(last);
        self.log
            .log(format!("opening {} in the default browser", daemon.clean_url()));
        macos::open_url(daemon.authenticated_url.as_str());
    }

    // MARK: Commands

    pub fn perform(self: &Arc<Self>, command: Command) {
        match command {
            Command::Open => self.open_ui(),
            Command::CopyUrl => {
                if let Some(info) = self.supervisor.current_state().info() {
                    macos::copy_to_pasteboard(info.authenticated_url.as_str());
                }
            }
            Command::Restart => self.prepare_and_run(RunAction::Restart, false),
            Command::Stop => self.supervisor.stop(),
            Command::Start => self.prepare_and_run(RunAction::Start, false),
            Command::ToggleLaunchAtLogin => self.toggle_launch_at_login(),
            Command::OpenLoginItems => macos::launch_at_login::open_system_settings(),
            Command::ToggleDock => self.toggle_dock_icon(),
            Command::ToggleKeepPreviousRuntime => self.toggle_keep_previous_runtime(),
            Command::OpenLogs => macos::reveal_in_finder(&[self.paths.shell_log(), self.paths.daemon_log()]),
            Command::OpenDshHome => self.open_dsh_home(),
            Command::EditConfig => self.edit_config(),
            Command::Repair => self.confirm_repair(),
            Command::About => self.show_about(),
            Command::Quit => macos::terminate(),
        }
    }

    fn open_ui(self: &Arc<Self>) {
        let state = self.supervisor.current_state();
        if let Some(info) = state.info() {
            self.open_browser(info);
            return;
        }
        self.open_when_ready.store(true, Ordering::SeqCst);
        let installing = self.model.lock().unwrap().installing;
        match state {
            DaemonState::Starting { .. } | DaemonState::Stopping => {
                self.show_progress(&i18n::tr("window.starting"))
            }
            // An install is already on its way; it starts the service when it lands.
            DaemonState::Idle if installing => {}
            _ => self.prepare_and_run(RunAction::Start, false),
        }
    }

    fn toggle_launch_at_login(self: &Arc<Self>) {
        let enable = macos::launch_at_login::status() == LaunchAtLoginStatus::Disabled;
        match macos::launch_at_login::set_enabled(enable) {
            Ok(()) => self.log.log(format!(
                "launch at login {} status={:?}",
                if enable { "enabled" } else { "disabled" },
                macos::launch_at_login::status()
            )),
            Err(error) => {
                self.log.log(format!("launch at login change failed: {error}"));
                macos::alert(
                    &i18n::tr1("dialog.launchAtLogin.failed", &error),
                    "",
                    &[i18n::tr("action.close")],
                    false,
                    false,
                );
            }
        }
        self.refresh_menus();
        if macos::launch_at_login::status() == LaunchAtLoginStatus::RequiresApproval {
            let response = macos::alert(
                &i18n::tr("dialog.launchAtLogin.title"),
                &i18n::tr1("dialog.launchAtLogin.approval", &self.info.display_name),
                &[i18n::tr("menu.openLoginItemsSettings"), i18n::tr("action.close")],
                false,
                true,
            );
            if response.is_first() {
                macos::launch_at_login::open_system_settings();
            }
        }
    }

    fn toggle_dock_icon(self: &Arc<Self>) {
        let visible = !tray::dock_icon_is_visible();
        self.set_dock_icon_visible(visible);
        self.log.log(format!(
            "{} Dock icon for current app session",
            if visible { "showing" } else { "hiding" }
        ));
    }

    /// Show or hide the Dock icon for the current app session only; every launch
    /// starts with it shown.
    pub fn set_dock_icon_visible(self: &Arc<Self>, visible: bool) {
        tray::set_dock_icon_visible(&self.app, visible);
        self.refresh_menus();
        if visible {
            macos::activate();
        }
    }

    /// Turning it off removes the kept runtime now; turning it on takes effect at
    /// the next upgrade.
    fn toggle_keep_previous_runtime(self: &Arc<Self>) {
        let keep = !macos::default_bool(keys::KEEP_PREVIOUS_RUNTIME, true);
        macos::set_default_bool(keys::KEEP_PREVIOUS_RUNTIME, keep);
        self.model.lock().unwrap().keeps_previous_runtime = keep;
        self.refresh_menus();
        self.log
            .log(format!("keep previous runtime after upgrades={keep}"));
        let _ = self.bootstrap.send(Job::ApplyRetention { keep });
    }

    fn open_dsh_home(self: &Arc<Self>) {
        let home = self
            .plan
            .lock()
            .unwrap()
            .as_ref()
            .map(|plan| plan.dsh_home.clone())
            .unwrap_or_else(|| crate::core::home_dir().join(".dsh"));
        let _ = CommandSpec::new("/usr/bin/open", [home.to_string_lossy().as_ref()])
            .timeout(Duration::from_secs(10))
            .run();
    }

    /// Create a documented config.json on first use, then open it in the default
    /// text editor.
    fn edit_config(self: &Arc<Self>) {
        let file = self.paths.config_file();
        if !file.exists() {
            let template = format!(
                "{{\n  \"port\": {},\n  \"profile\": \"{}\",\n  \"extraArgs\": [],\n  \"environment\": {{}}\n}}\n",
                self.info.default_port, self.info.profile
            );
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&file)
                .and_then(|mut handle| std::io::Write::write_all(&mut handle, template.as_bytes()));
        }
        // `open -t` is exactly "the editor registered for plain text".
        let opened = CommandSpec::new("/usr/bin/open", ["-t", file.to_string_lossy().as_ref()])
            .timeout(Duration::from_secs(10))
            .run();
        if !opened.map(|result| result.succeeded()).unwrap_or(false) {
            macos::reveal_in_finder(&[file]);
        }
    }

    fn confirm_repair(self: &Arc<Self>) {
        let version = self
            .installer
            .lock()
            .unwrap()
            .bundled_manifest()
            .map(|manifest| manifest.dsh_version)
            .unwrap_or_else(|| "?".to_owned());
        let response = macos::alert(
            &i18n::tr("dialog.repair.title"),
            &i18n::tr1("dialog.repair.body", version),
            &[i18n::tr("dialog.repair.action"), i18n::tr("dialog.cancel")],
            false,
            true,
        );
        if !response.is_first() {
            return;
        }
        self.log.log("runtime repair requested");
        let state = self.clone();
        self.supervisor.stop_then(move || {
            state.prepare_and_run(RunAction::Start, true);
        });
    }

    /// VS Code-style About dialog: icon, name, one "Key: value" line per fact, and
    /// a Copy button for bug reports.
    fn show_about(self: &Arc<Self>) {
        let details = self.about_details().join("\n");
        let response = macos::alert(
            &self.info.display_name,
            &details,
            &[i18n::tr("about.copy"), i18n::tr("about.ok")],
            false,
            true,
        );
        if response.is_first() {
            macos::copy_to_pasteboard(&details);
        }
    }

    pub fn about_details(&self) -> Vec<String> {
        // CFBundleVersion stays out: Commit and Date identify a build more precisely.
        let mut lines = vec![format!("Version: {}", self.info.version_label)];
        if let Some((key, value)) = self.info.repo_entry() {
            lines.push(format!("{key}: {value}"));
        }
        if let Some(commit) = self.info.git_commit {
            lines.push(format!("Commit: {commit}"));
        }
        if let Some(date) = self.info.build_date {
            let age = info::relative_age(date)
                .map(|age| format!(" ({age})"))
                .unwrap_or_default();
            lines.push(format!("Date: {date}{age}"));
        }
        let config_runtime = self.config.lock().unwrap().runtime.is_some();
        if config_runtime {
            let version = self
                .plan
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|plan| plan.dsh_version.clone())
                .unwrap_or_else(|| "unknown".to_owned());
            lines.push(format!("DSH: {version} (config.json)"));
        } else if let Some(receipt) = self.runtime_receipt.lock().unwrap().as_ref() {
            lines.push(format!("DSH: {}", receipt.dsh_version));
            lines.push(format!("Node.js: {}", receipt.node_version));
            lines.push(format!("pnpm: {}", receipt.pnpm_version));
        }
        lines.push(format!("OS: {}", macos::operating_system()));
        lines
    }

    /// Ask before stopping the running service; "Don't ask again" skips it from
    /// then on. Returns `true` to go on quitting.
    fn confirm_quit(&self) -> bool {
        if macos::default_bool(keys::SKIP_QUIT_CONFIRMATION, false) {
            return true;
        }
        let response = macos::alert(
            &i18n::tr1("dialog.quit.title", &self.info.display_name),
            &i18n::tr("dialog.quit.body"),
            &[i18n::tr("dialog.quit.action"), i18n::tr("dialog.cancel")],
            true,
            true,
        );
        if !response.is_first() {
            return false;
        }
        if response.suppressed {
            macos::set_default_bool(keys::SKIP_QUIT_CONFIRMATION, true);
        }
        true
    }

    /// Every quit path lands here: the status menu, ⌘Q, the Dock's Quit, Activity
    /// Monitor and AppleScript. Each one confirms while the service runs, except
    /// logout, restart, shutdown and signals, which must never wait for a click.
    fn should_terminate(&self) -> TerminateDecision {
        let state = self.supervisor.current_state();
        self.log.log(format!(
            "quit requested; the service is {}",
            if state.is_active() {
                "running"
            } else {
                "not running"
            }
        ));
        if self.terminating.load(Ordering::SeqCst) || !state.is_active() {
            return TerminateDecision::Now;
        }
        if !self.quit_without_confirmation.load(Ordering::SeqCst)
            && !macos::quit_is_system_initiated()
            && !self.confirm_quit()
        {
            self.log.log("quit cancelled");
            return TerminateDecision::Cancel;
        }
        self.terminating.store(true, Ordering::SeqCst);
        self.log.log("quitting: stopping the dsh service");
        // Blocking here keeps the answer simple: by the time AppKit tears the app
        // down, the daemon is gone.
        if !self.supervisor.stop_blocking(Duration::from_secs(15)) {
            self.log
                .log("the dsh service did not stop within 15s; quitting anyway");
        }
        self.log.flush();
        TerminateDecision::Now
    }

    /// `dsh-launcher://open|start|stop|restart|logs`
    pub fn handle_deep_link(self: &Arc<Self>, url: &str) {
        let Some((scheme, rest)) = url.split_once("://") else {
            return;
        };
        if !scheme.eq_ignore_ascii_case(self.info.url_scheme) {
            return;
        }
        let action = rest.split(['/', '?']).next().unwrap_or("").to_lowercase();
        self.log.log(format!(
            "deep link action={}",
            if action.is_empty() { "open" } else { &action }
        ));
        match action.as_str() {
            "" | "open" => self.perform(Command::Open),
            "start" => self.perform(Command::Start),
            "stop" => self.perform(Command::Stop),
            "restart" => self.perform(Command::Restart),
            "logs" => self.perform(Command::OpenLogs),
            _ => self.log.log("deep link ignored: unknown action"),
        }
    }

    fn configure_launch_at_login_on_first_run(self: &Arc<Self>) {
        if macos::default_bool(keys::CONFIGURED_LAUNCH_AT_LOGIN, false) {
            return;
        }
        if !self.info.is_in_stable_location() {
            self.log
                .log("launch at login left unchanged: the app is not in an Applications folder");
            return;
        }
        macos::set_default_bool(keys::CONFIGURED_LAUNCH_AT_LOGIN, true);
        match macos::launch_at_login::set_enabled(true) {
            Ok(()) => self.log.log(format!(
                "launch at login enabled on first run status={:?}",
                macos::launch_at_login::status()
            )),
            Err(error) => self
                .log
                .log(format!("launch at login could not be enabled: {error}")),
        }
        self.refresh_menus();
    }

    /// The first thing that happens once AppKit has finished launching: the quit
    /// gate, the Dock menu, sleep/wake notices, and the first start.
    fn did_finish_launching(self: &Arc<Self>) {
        let launched_at_login = macos::is_login_item_launch();
        self.launched_at_login.store(launched_at_login, Ordering::SeqCst);
        self.log.log(format!(
            "{} {} ({}) commit={} launched pid={} loginItem={launched_at_login} bundle={}",
            self.info.display_name,
            self.info.version_label,
            self.info.build,
            self.info.git_commit.unwrap_or("unknown"),
            std::process::id(),
            self.info.bundle_path.display(),
        ));

        let terminate_state = self.clone();
        let dock_state = self.clone();
        let installed = macos::install_delegate_hooks(
            move || terminate_state.should_terminate(),
            move || tray::dock_menu_handle(&dock_state.model.lock().unwrap().clone()),
        );
        if !installed {
            self.log
                .log("could not install the quit and Dock menu hooks on the app delegate");
        }

        let sleeping = self.clone();
        let waking = self.clone();
        macos::observe_sleep_wake(
            move || {
                sleeping.log.log("system will sleep");
                sleeping.supervisor.system_will_sleep();
            },
            move || {
                waking.log.log("system did wake");
                waking.supervisor.system_did_wake();
            },
        );

        let signals = self.clone();
        macos::handle_termination_signals(move |signal| {
            signals.log.log(format!("received signal {signal}; quitting"));
            signals.quit_without_confirmation.store(true, Ordering::SeqCst);
            signals.on_main(|_| macos::terminate());
        });

        self.configure_launch_at_login_on_first_run();

        // Login launches stay silent; DSH_LAUNCHER_NO_OPEN=1 does the same for
        // development runs.
        let quiet = launched_at_login || std::env::var("DSH_LAUNCHER_NO_OPEN").as_deref() == Ok("1");
        self.open_when_ready.store(!quiet, Ordering::SeqCst);
        self.show_progress_until_ready.store(!quiet, Ordering::SeqCst);
        self.prepare_and_run(RunAction::Start, false);
    }
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn package_version_near(entry: &std::path::Path) -> Option<String> {
    let manifest = entry.parent()?.parent()?.join("package.json");
    let data = std::fs::read(manifest).ok()?;
    let package: serde_json::Value = serde_json::from_slice(&data).ok()?;
    package.get("version")?.as_str().map(str::to_owned)
}

// MARK: Tauri commands

#[tauri::command]
fn status_ready(state: tauri::State<'_, Arc<AppState>>) -> Option<StatusPayload> {
    state.status.lock().unwrap().clone()
}

#[tauri::command]
fn status_action(action: String, state: tauri::State<'_, Arc<AppState>>) {
    // Commands do not promise to run on the main thread; windows and AppKit do.
    let state = state.inner().clone();
    state.clone().on_main(move |state| match action.as_str() {
        "retry" => {
            status_window::hide(&state.app);
            state.prepare_and_run(RunAction::Restart, false);
        }
        "logs" => state.perform(Command::OpenLogs),
        _ => status_window::hide(&state.app),
    });
}

// MARK: Entry point

pub fn run() {
    // Before any thread exists, so every thread inherits the block.
    macos::block_termination_signals();
    if let Ok(language) = std::env::var("DSH_LAUNCHER_LANG") {
        i18n::set_language(language.starts_with("zh"));
    }

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Another copy was launched; this one owns the daemon, so just open the UI.
            if let Some(state) = app.try_state::<Arc<AppState>>() {
                let state = state.inner().clone();
                state.clone().on_main(|state| state.perform(Command::Open));
            }
        }))
        .invoke_handler(tauri::generate_handler![status_ready, status_action])
        .setup(|app| {
            let handle = app.handle().clone();
            let state = AppState::new(&handle);
            let listener = state.clone();
            state.supervisor.on_state_change(move |daemon_state| {
                listener.on_main(move |state| state.daemon_state_changed(daemon_state));
            });
            tray::install(&handle, &state.model.lock().unwrap().clone())?;
            handle.manage(state);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start DSH Launcher");

    app.run(|handle, event| {
        let Some(state) = handle
            .try_state::<Arc<AppState>>()
            .map(|state| state.inner().clone())
        else {
            return;
        };
        match event {
            RunEvent::Ready => state.did_finish_launching(),
            RunEvent::Opened { urls } => {
                for url in urls {
                    state.handle_deep_link(url.as_str());
                }
            }
            // Opening the app again (Finder, Launchpad, Dock) brings back a Dock icon
            // hidden for this session, then opens DSH.
            RunEvent::Reopen { .. } => {
                if !tray::dock_icon_is_visible() {
                    state.set_dock_icon_visible(true);
                    state.log.log("restoring Dock icon from app reopen");
                }
                state.perform(Command::Open);
            }
            RunEvent::MenuEvent(event) => {
                if let Some(command) = Command::from_id(event.id.as_ref()) {
                    state.perform(command);
                }
            }
            // Hovering the status item is the moment before its menu opens, which
            // is when externally changed settings (login item, Dock icon) show up.
            RunEvent::TrayIconEvent(tauri::tray::TrayIconEvent::Enter { .. }) => state.refresh_menus(),
            RunEvent::WindowEvent { label, event, .. } => {
                // The status window is reused; closing it only puts it away.
                if label == status_window::LABEL {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        status_window::hide(&state.app);
                    }
                }
            }
            RunEvent::ExitRequested { api, code, .. } => {
                // Closing the status window must not end the app; only a real quit does.
                if code.is_none() {
                    api.prevent_exit();
                }
            }
            RunEvent::Exit => {
                state.log.log(format!("{} exiting", state.info.display_name));
                state.log.flush();
            }
            _ => {}
        }
    });
}
