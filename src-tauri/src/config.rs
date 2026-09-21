//! Swift 版 `~/.dsh-launcher/config.json` 的最小跨平台实现。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

const SHIPPED_PROFILES: &[&str] = &["acp", "web", "headless", "sdk", "sdk-minimal"];

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ShellConfig {
    pub port: Option<u16>,
    pub profile: Option<String>,
    #[serde(rename = "dshHome")]
    pub dsh_home: Option<String>,
    #[serde(rename = "extraArgs")]
    pub extra_args: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub runtime: Option<RuntimeConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RuntimeConfig {
    pub node: Option<String>,
    pub entry: Option<String>,
}

impl ShellConfig {
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(path).map_err(|error| format!("读取配置失败：{error}"))?;
        if contents.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&contents).map_err(|error| format!("解析配置失败：{error}"))
    }

    pub fn profile(&self) -> &str {
        self.profile
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("launcher")
    }

    pub fn dsh_home(&self, home: &Path) -> PathBuf {
        let Some(raw) = self
            .dsh_home
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return home.join(".dsh");
        };
        if raw == "~" {
            return home.to_path_buf();
        }
        if let Some(relative) = raw.strip_prefix("~/") {
            return home.join(relative);
        }
        PathBuf::from(raw)
    }

    pub fn environment(&self, home: &Path) -> BTreeMap<String, String> {
        let mut environment = login_environment();
        environment.extend(self.environment.clone());
        if self.dsh_home.is_some() {
            environment.insert("DSH_HOME".to_owned(), self.dsh_home(home).display().to_string());
        }
        environment.insert("HOME".to_owned(), home.display().to_string());
        environment
    }

    pub fn runtime_command(&self) -> (String, Option<String>) {
        let Some(runtime) = self.runtime.as_ref() else {
            return ("dsh".to_owned(), None);
        };
        (
            runtime
                .node
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("node")
                .to_owned(),
            runtime
                .entry
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned),
        )
    }

    pub fn template_json() -> String {
        // `_comment` 会被 serde 忽略，保留在文件里是为了让第一次打开配置的
        // 用户知道每个字段的作用；runtime 明确保持 null，避免误把 dsh 当成
        // Node entry 传给自定义运行时。
        r#"{
  "_comment": "DSH Launcher 配置。修改后选择“重启”生效；启动器不会安装或升级 runtime。",
  "_comment_port": "监听端口；被占用时会按顺序向后尝试最多 20 个端口。",
  "port": 31080,
  "_comment_profile": "dsh profile 名称；默认 launcher。",
  "profile": "launcher",
  "_comment_dshHome": "DSH 数据目录；支持 ~ 和 ~/relative/path。",
  "dshHome": "~/.dsh",
  "_comment_extraArgs": "追加到 dsh 命令末尾的参数。",
  "extraArgs": [],
  "_comment_environment": "传给 dsh 的额外环境变量，会覆盖登录 shell 环境。",
  "environment": {},
  "_comment_runtime": "可选外部运行时，例如 {\"node\":\"/path/to/node\",\"entry\":\"/path/to/dsh.js\"}。",
  "runtime": null
}
"#
        .to_owned()
    }

    pub fn ensure_file(path: &Path) -> Result<(), String> {
        if path.exists() {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("创建配置目录失败：{error}"))?;
        }
        fs::write(path, Self::template_json()).map_err(|error| format!("创建配置文件失败：{error}"))
    }

    pub fn initialize_profile(&self, home: &Path) -> bool {
        !SHIPPED_PROFILES.contains(&self.profile())
            && !self
                .dsh_home(home)
                .join("profiles")
                .join(self.profile())
                .join("package.json")
                .exists()
    }

    /// 与 Swift `DaemonLaunchPlan.arguments` 保持同一顺序：启动器参数优先，
    /// 最后的 `extraArgs` 允许用户覆盖/扩展 dsh 行为。
    pub fn arguments(&self, port: u16, home: &Path) -> Vec<String> {
        let mut arguments = vec!["--profile".to_owned(), self.profile().to_owned()];
        if self.initialize_profile(home) {
            arguments.extend(["--from-default-profile".to_owned(), "web".to_owned()]);
        }
        arguments.extend([
            "--no-open".to_owned(),
            "--host".to_owned(),
            "127.0.0.1".to_owned(),
            "--port".to_owned(),
            port.to_string(),
        ]);
        arguments.extend(self.extra_args.iter().cloned());
        arguments
    }
}

fn login_environment() -> BTreeMap<String, String> {
    let fallback = std::env::vars().collect();
    #[cfg(unix)]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        if let Ok(output) = Command::new(shell).args(["-l", "-i", "-c", "env -0"]).output() {
            if output.status.success() {
                let parsed = output
                    .stdout
                    .split(|byte| *byte == 0)
                    .filter_map(|entry| {
                        let separator = entry.iter().position(|byte| *byte == b'=')?;
                        let (key, value) = entry.split_at(separator);
                        Some((
                            String::from_utf8_lossy(key).into_owned(),
                            String::from_utf8_lossy(&value[1..]).into_owned(),
                        ))
                    })
                    .collect::<BTreeMap<_, _>>();
                if !parsed.is_empty() {
                    return parsed;
                }
            }
        }
    }
    fallback
}

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config: PathBuf,
    pub logs: PathBuf,
    pub preferences: PathBuf,
}

impl AppPaths {
    pub fn new(home: PathBuf) -> Self {
        let root = home.join(".dsh-launcher");
        Self {
            config: root.join("config.json"),
            logs: root.join("logs"),
            preferences: root.join("preferences.json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_swift_compatible_default_arguments() {
        let config = ShellConfig::default();
        let home = Path::new("/tmp/dsh-launcher-test-home");
        assert_eq!(
            config.arguments(31080, home),
            [
                "--profile",
                "launcher",
                "--from-default-profile",
                "web",
                "--no-open",
                "--host",
                "127.0.0.1",
                "--port",
                "31080"
            ]
        );
    }

    #[test]
    fn appends_extra_arguments_and_expands_dsh_home() {
        let config = ShellConfig {
            dsh_home: Some("~/custom-dsh".to_owned()),
            extra_args: vec!["--trusted-host".to_owned(), "example.test".to_owned()],
            ..ShellConfig::default()
        };
        let home = Path::new("/tmp/home");
        assert_eq!(config.dsh_home(home), PathBuf::from("/tmp/home/custom-dsh"));
        assert_eq!(
            config.arguments(31081, home).last(),
            Some(&"example.test".to_owned())
        );
    }

    #[test]
    fn supports_swift_style_runtime_override() {
        let config = ShellConfig {
            runtime: Some(RuntimeConfig {
                node: Some("/usr/local/bin/node".to_owned()),
                entry: Some("/tmp/dsh.js".to_owned()),
            }),
            ..ShellConfig::default()
        };
        assert_eq!(
            config.runtime_command(),
            ("/usr/local/bin/node".to_owned(), Some("/tmp/dsh.js".to_owned()))
        );
    }

    #[test]
    fn template_is_documented_and_loadable() {
        let template = ShellConfig::template_json();
        let config: ShellConfig = serde_json::from_str(&template).unwrap();
        assert_eq!(config.port, Some(31080));
        assert_eq!(config.profile(), "launcher");
        assert!(config.runtime.is_none());
        assert!(template.contains("不会安装或升级 runtime"));
    }
}
