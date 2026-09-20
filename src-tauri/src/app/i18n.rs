//! English and Simplified Chinese strings, picked from the user's preferred
//! languages. `{}` marks where an argument goes, in order.

use std::sync::OnceLock;

/// key, English, 简体中文
const TABLE: &[(&str, &str, &str)] = &[
    // Status line
    ("status.idle", "DSH · Not started", "DSH · 未启动"),
    ("status.installing", "DSH · Installing runtime…", "DSH · 正在安装运行时…"),
    ("status.starting", "DSH · Starting…", "DSH · 正在启动…"),
    ("status.restarting", "DSH · Restarting (attempt {})…", "DSH · 正在重启（第 {} 次）…"),
    ("status.running", "DSH · Running on {}", "DSH · 运行中 {}"),
    ("status.stopping", "DSH · Stopping…", "DSH · 正在停止…"),
    ("status.stopped", "DSH · Stopped", "DSH · 已停止"),
    ("status.failed", "DSH · Failed to start", "DSH · 启动失败"),
    // Menu
    ("menu.error", "Error: {}", "错误：{}"),
    ("menu.runtime", "Runtime: {}", "运行时：{}"),
    ("menu.open", "Open DSH", "打开 DSH"),
    ("menu.copyURL", "Copy Access Link", "复制访问链接"),
    ("menu.restart", "Restart Service", "重启服务"),
    ("menu.stop", "Stop Service", "停止服务"),
    ("menu.start", "Start Service", "启动服务"),
    ("menu.launchAtLogin", "Launch at Login", "登录时启动"),
    ("menu.launchAtLoginNeedsApproval", "Launch at Login (Needs Approval)", "登录时启动（需要批准）"),
    ("menu.openLoginItemsSettings", "Open Login Items Settings…", "打开登录项设置…"),
    ("menu.showDock", "Show Dock Icon", "显示 Dock 图标"),
    ("menu.hideDock", "Hide Dock Icon", "隐藏 Dock 图标"),
    ("menu.keepPreviousRuntime", "Keep Previous Runtime After Upgrades", "升级后保留上一个运行时"),
    ("menu.openLogs", "Show Logs in Finder", "在访达中显示日志"),
    ("menu.openDshHome", "Open DSH Data Folder", "打开 DSH 数据目录"),
    ("menu.editConfig", "Edit Configuration…", "编辑配置…"),
    ("menu.repair", "Reinstall Bundled Runtime…", "重新安装内置运行时…"),
    ("menu.about", "About {}", "关于 {}"),
    ("menu.hide", "Hide {}", "隐藏 {}"),
    ("menu.quit", "Quit {}", "退出 {}"),
    ("menu.edit", "Edit", "编辑"),
    ("menu.copy", "Copy", "拷贝"),
    ("menu.selectAll", "Select All", "全选"),
    // Status window
    ("window.title", "Preparing DSH", "正在准备 DSH"),
    (
        "window.installing",
        "Installing the DSH runtime. This happens once per version and takes a few seconds.",
        "正在安装 DSH 运行时。每个版本只需安装一次，约需几秒钟。",
    ),
    ("window.starting", "Starting the DSH service…", "正在启动 DSH 服务…"),
    ("window.ready", "DSH is ready. Opening it in your browser…", "DSH 已就绪，正在浏览器中打开…"),
    ("action.retry", "Retry", "重试"),
    ("action.openLogs", "Show Logs", "查看日志"),
    ("action.close", "Close", "关闭"),
    // Dialogs
    ("dialog.failed.title", "DSH could not start", "DSH 无法启动"),
    ("dialog.cancel", "Cancel", "取消"),
    ("dialog.quit.title", "Quit {}?", "退出 {}？"),
    (
        "dialog.quit.body",
        "This stops the DSH background service. Work running in open sessions will be interrupted.",
        "这会停止 DSH 后台服务，正在进行的会话任务将被中断。",
    ),
    ("dialog.quit.action", "Quit", "退出"),
    ("dialog.repair.title", "Reinstall the bundled runtime?", "重新安装内置运行时？"),
    (
        "dialog.repair.body",
        "The service stops, the runtime bundled with this app (DSH {}) is extracted again, and the service restarts. Sessions, settings and credentials in your DSH data folder are not touched.",
        "服务将停止，本应用内置的运行时（DSH {}）会重新解压，然后服务重新启动。DSH 数据目录中的会话、设置和凭据不受影响。",
    ),
    ("dialog.repair.action", "Reinstall", "重新安装"),
    ("dialog.launchAtLogin.title", "Approval needed", "需要批准"),
    (
        "dialog.launchAtLogin.approval",
        "Allow {} in System Settings › General › Login Items to launch it at login.",
        "请在“系统设置 › 通用 › 登录项”中允许 {}，以便登录时启动。",
    ),
    ("dialog.launchAtLogin.failed", "Could not change Launch at Login: {}", "无法更改“登录时启动”：{}"),
    // About
    ("about.copy", "Copy", "复制"),
    ("about.ok", "OK", "确定"),
    // Relative build age, shown after the build date
    ("age.today", "today", "今天"),
    ("age.days", "{} days ago", "{} 天前"),
    ("age.weeks", "{} weeks ago", "{} 周前"),
    ("age.months", "{} months ago", "{} 个月前"),
    ("age.years", "{} years ago", "{} 年前"),
];

static CHINESE: OnceLock<bool> = OnceLock::new();

fn chinese() -> bool {
    *CHINESE.get_or_init(super::macos::prefers_simplified_chinese)
}

/// Force a language; tests and `DSH_LAUNCHER_LANG=en` use it.
pub fn set_language(chinese: bool) {
    let _ = CHINESE.set(chinese);
}

pub fn tr(key: &str) -> String {
    template(key).to_owned()
}

pub fn tr1(key: &str, argument: impl std::fmt::Display) -> String {
    fill(template(key), &[argument.to_string()])
}

pub fn tr_args(key: &str, arguments: &[String]) -> String {
    fill(template(key), arguments)
}

fn template(key: &str) -> &'static str {
    match TABLE.iter().find(|(name, _, _)| *name == key) {
        Some((_, english, chinese_text)) => {
            if chinese() {
                chinese_text
            } else {
                english
            }
        }
        // A missing key shows itself rather than an empty menu entry.
        None => Box::leak(key.to_owned().into_boxed_str()),
    }
}

fn fill(template: &str, arguments: &[String]) -> String {
    let mut text = template.to_owned();
    for argument in arguments {
        match text.find("{}") {
            Some(index) => text.replace_range(index..index + 2, argument),
            None => break,
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_arguments_in_order() {
        assert_eq!(
            fill("Restarting (attempt {})…", &["3".to_owned()]),
            "Restarting (attempt 3)…"
        );
        assert_eq!(fill("{} and {}", &["a".to_owned(), "b".to_owned()]), "a and b");
        assert_eq!(fill("no slots", &["x".to_owned()]), "no slots");
    }

    #[test]
    fn every_key_carries_both_languages() {
        for (key, english, chinese_text) in TABLE {
            assert!(!english.is_empty(), "{key} has no English text");
            assert!(!chinese_text.is_empty(), "{key} has no Chinese text");
            assert_eq!(
                english.matches("{}").count(),
                chinese_text.matches("{}").count(),
                "{key} has a different number of slots in each language"
            );
        }
    }
}
