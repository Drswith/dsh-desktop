//! The status item's menu, the application menu, and the Dock icon's menu, all
//! rebuilt from one model whenever it changes.

use muda::{MenuItem as DockItem, PredefinedMenuItem as DockPredefined};
use tauri::menu::{
    CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder,
};
use tauri::{AppHandle, Wry};

use crate::app::i18n::{tr, tr1};
use crate::app::macos::LaunchAtLoginStatus;
use crate::core::daemon::DaemonState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Open,
    CopyUrl,
    Restart,
    Stop,
    Start,
    ToggleLaunchAtLogin,
    OpenLoginItems,
    ToggleDock,
    ToggleKeepPreviousRuntime,
    OpenLogs,
    OpenDshHome,
    EditConfig,
    Repair,
    About,
    Quit,
}

impl Command {
    pub fn id(self) -> &'static str {
        match self {
            Command::Open => "open",
            Command::CopyUrl => "copyURL",
            Command::Restart => "restart",
            Command::Stop => "stop",
            Command::Start => "start",
            Command::ToggleLaunchAtLogin => "toggleLaunchAtLogin",
            Command::OpenLoginItems => "openLoginItems",
            Command::ToggleDock => "toggleDock",
            Command::ToggleKeepPreviousRuntime => "toggleKeepPreviousRuntime",
            Command::OpenLogs => "openLogs",
            Command::OpenDshHome => "openDshHome",
            Command::EditConfig => "editConfig",
            Command::Repair => "repair",
            Command::About => "about",
            Command::Quit => "quit",
        }
    }

    pub fn from_id(id: &str) -> Option<Command> {
        [
            Command::Open,
            Command::CopyUrl,
            Command::Restart,
            Command::Stop,
            Command::Start,
            Command::ToggleLaunchAtLogin,
            Command::OpenLoginItems,
            Command::ToggleDock,
            Command::ToggleKeepPreviousRuntime,
            Command::OpenLogs,
            Command::OpenDshHome,
            Command::EditConfig,
            Command::Repair,
            Command::About,
            Command::Quit,
        ]
        .into_iter()
        .find(|command| command.id() == id)
    }
}

#[derive(Debug, Clone)]
pub struct MenuModel {
    pub app_name: String,
    pub state: DaemonState,
    pub installing: bool,
    pub runtime_summary: Option<String>,
    pub launch_at_login: LaunchAtLoginStatus,
    pub shows_dock_icon: bool,
    pub can_repair: bool,
    pub keeps_previous_runtime: bool,
}

impl MenuModel {
    pub fn new(app_name: String) -> MenuModel {
        MenuModel {
            app_name,
            state: DaemonState::Idle,
            installing: false,
            runtime_summary: None,
            launch_at_login: LaunchAtLoginStatus::Disabled,
            shows_dock_icon: true,
            can_repair: false,
            keeps_previous_runtime: true,
        }
    }

    pub fn status_text(&self) -> String {
        if self.installing {
            return tr("status.installing");
        }
        match &self.state {
            DaemonState::Idle => tr("status.idle"),
            DaemonState::Starting { attempt } if *attempt <= 1 => tr("status.starting"),
            DaemonState::Starting { attempt } => tr1("status.restarting", attempt),
            DaemonState::Running(info) => tr1("status.running", format!("127.0.0.1:{}", info.port)),
            DaemonState::Stopping => tr("status.stopping"),
            DaemonState::Stopped => tr("status.stopped"),
            DaemonState::Failed(_) => tr("status.failed"),
        }
    }

    pub fn tooltip(&self) -> String {
        format!("{} — {}", self.app_name, self.status_text())
    }

    pub fn is_running(&self) -> bool {
        self.state.info().is_some()
    }

    fn is_busy(&self) -> bool {
        self.installing || self.state == DaemonState::Stopping
    }
}

/// The status item's menu: status, the open and service actions, the toggles,
/// the folders, and About/Quit.
pub fn build_tray_menu(app: &AppHandle, model: &MenuModel) -> tauri::Result<Menu<Wry>> {
    let mut builder = MenuBuilder::new(app);
    builder = builder.item(&disabled(app, &model.status_text())?);
    if let DaemonState::Failed(message) = &model.state {
        builder = builder.item(&disabled(app, &tr1("menu.error", truncate(message, 72)))?);
    }
    if let Some(summary) = model.runtime_summary.as_ref().filter(|text| !text.is_empty()) {
        builder = builder.item(&disabled(app, &tr1("menu.runtime", summary))?);
    }
    builder = builder.separator();
    for item in open_items(app, model, true)? {
        builder = builder.item(&item);
    }
    builder = builder.separator();
    for item in service_items(app, model, true)? {
        builder = builder.item(&item);
    }
    builder = builder.separator();

    let login_title = if model.launch_at_login == LaunchAtLoginStatus::RequiresApproval {
        tr("menu.launchAtLoginNeedsApproval")
    } else {
        tr("menu.launchAtLogin")
    };
    let login = CheckMenuItemBuilder::with_id(Command::ToggleLaunchAtLogin.id(), login_title)
        .checked(model.launch_at_login != LaunchAtLoginStatus::Disabled)
        .build(app)?;
    builder = builder.item(&login);
    if model.launch_at_login == LaunchAtLoginStatus::RequiresApproval {
        builder = builder.item(&action(
            app,
            Command::OpenLoginItems,
            &tr("menu.openLoginItemsSettings"),
            None,
            true,
        )?);
    }
    let dock_title = tr(if model.shows_dock_icon {
        "menu.hideDock"
    } else {
        "menu.showDock"
    });
    builder = builder.item(&action(app, Command::ToggleDock, &dock_title, None, true)?);
    if model.can_repair {
        let keep = CheckMenuItemBuilder::with_id(
            Command::ToggleKeepPreviousRuntime.id(),
            tr("menu.keepPreviousRuntime"),
        )
        .checked(model.keeps_previous_runtime)
        .build(app)?;
        builder = builder.item(&keep);
    }
    builder = builder.separator();

    builder = builder.item(&action(app, Command::OpenLogs, &tr("menu.openLogs"), None, true)?);
    builder = builder.item(&action(
        app,
        Command::OpenDshHome,
        &tr("menu.openDshHome"),
        None,
        true,
    )?);
    builder = builder.item(&action(
        app,
        Command::EditConfig,
        &tr("menu.editConfig"),
        None,
        true,
    )?);
    if model.can_repair {
        builder = builder.item(&action(
            app,
            Command::Repair,
            &tr("menu.repair"),
            None,
            !model.is_busy(),
        )?);
    }
    builder = builder.separator();
    builder = builder.item(&action(
        app,
        Command::About,
        &tr1("menu.about", &model.app_name),
        None,
        true,
    )?);
    builder = builder.item(&action(
        app,
        Command::Quit,
        &tr1("menu.quit", &model.app_name),
        Some("CmdOrCtrl+Q"),
        true,
    )?);
    builder.build()
}

/// App and Edit menus: shown when the Dock icon is on, and they give the status
/// window's selectable text the standard copy shortcuts.
pub fn build_app_menu(app: &AppHandle, model: &MenuModel) -> tauri::Result<Menu<Wry>> {
    let name = &model.app_name;
    let app_menu = SubmenuBuilder::new(app, name)
        .item(&action(
            app,
            Command::About,
            &tr1("menu.about", name),
            None,
            true,
        )?)
        .separator()
        .item(&PredefinedMenuItem::hide(app, Some(&tr1("menu.hide", name)))?)
        .separator()
        .item(&action(
            app,
            Command::Quit,
            &tr1("menu.quit", name),
            Some("CmdOrCtrl+Q"),
            true,
        )?)
        .build()?;
    let edit_menu = SubmenuBuilder::new(app, tr("menu.edit"))
        .item(&PredefinedMenuItem::copy(app, Some(&tr("menu.copy")))?)
        .item(&PredefinedMenuItem::select_all(app, Some(&tr("menu.selectAll")))?)
        .build()?;
    MenuBuilder::new(app).items(&[&app_menu, &edit_menu]).build()
}

/// Right-click menu of the Dock icon: status plus the service actions. macOS
/// appends its own Options, Show All Windows, Hide and Quit items below.
///
/// Built with muda directly because Tauri keeps the underlying `NSMenu` private;
/// its events still arrive through the same menu-event handler.
pub fn build_dock_menu(model: &MenuModel) -> muda::Menu {
    let menu = muda::Menu::new();
    let disabled = DockItem::new(model.status_text(), false, None);
    let _ = menu.append(&disabled);
    let _ = menu.append(&DockPredefined::separator());
    let _ = menu.append(&DockItem::with_id(
        Command::Open.id(),
        tr("menu.open"),
        !model.installing,
        None,
    ));
    let _ = menu.append(&DockItem::with_id(
        Command::CopyUrl.id(),
        tr("menu.copyURL"),
        model.is_running(),
        None,
    ));
    let _ = menu.append(&DockPredefined::separator());
    let _ = menu.append(&DockItem::with_id(
        Command::Restart.id(),
        tr("menu.restart"),
        !model.is_busy() && model.state != DaemonState::Idle,
        None,
    ));
    let (command, title) = match model.state {
        DaemonState::Running(_) | DaemonState::Starting { .. } => (Command::Stop, tr("menu.stop")),
        _ => (Command::Start, tr("menu.start")),
    };
    let _ = menu.append(&DockItem::with_id(command.id(), title, !model.is_busy(), None));
    menu
}

fn open_items(
    app: &AppHandle,
    model: &MenuModel,
    shortcuts: bool,
) -> tauri::Result<Vec<tauri::menu::MenuItem<Wry>>> {
    Ok(vec![
        action(
            app,
            Command::Open,
            &tr("menu.open"),
            shortcuts.then_some("CmdOrCtrl+O"),
            !model.installing,
        )?,
        action(
            app,
            Command::CopyUrl,
            &tr("menu.copyURL"),
            None,
            model.is_running(),
        )?,
    ])
}

fn service_items(
    app: &AppHandle,
    model: &MenuModel,
    shortcuts: bool,
) -> tauri::Result<Vec<tauri::menu::MenuItem<Wry>>> {
    let restart = action(
        app,
        Command::Restart,
        &tr("menu.restart"),
        shortcuts.then_some("CmdOrCtrl+R"),
        !model.is_busy() && model.state != DaemonState::Idle,
    )?;
    let (command, title) = match model.state {
        DaemonState::Running(_) | DaemonState::Starting { .. } => (Command::Stop, tr("menu.stop")),
        _ => (Command::Start, tr("menu.start")),
    };
    Ok(vec![
        restart,
        action(app, command, &title, None, !model.is_busy())?,
    ])
}

fn action(
    app: &AppHandle,
    command: Command,
    title: &str,
    accelerator: Option<&str>,
    enabled: bool,
) -> tauri::Result<tauri::menu::MenuItem<Wry>> {
    let mut builder = MenuItemBuilder::with_id(command.id(), title).enabled(enabled);
    if let Some(accelerator) = accelerator {
        builder = builder.accelerator(accelerator);
    }
    builder.build(app)
}

fn disabled(app: &AppHandle, title: &str) -> tauri::Result<tauri::menu::MenuItem<Wry>> {
    MenuItemBuilder::new(title).enabled(false).build(app)
}

pub fn truncate(text: &str, limit: usize) -> String {
    let single = text.replace('\n', " ");
    if single.chars().count() <= limit {
        return single;
    }
    single.chars().take(limit - 1).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_round_trips_through_its_id() {
        for command in [
            Command::Open,
            Command::Quit,
            Command::ToggleKeepPreviousRuntime,
            Command::Repair,
        ] {
            assert_eq!(Command::from_id(command.id()), Some(command));
        }
        assert_eq!(Command::from_id("nope"), None);
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate("短", 3), "短");
        assert_eq!(truncate("一二三四", 3), "一二…");
        assert_eq!(truncate("a\nb", 8), "a b");
    }
}
