//! Swift 版 `~/.dsh-launcher/config.json` 的最小跨平台实现。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

const SHIPPED_PROFILES: &[&str] = &["acp", "web", "headless", "sdk", "sdk-minimal"];
const DEFAULT_RUNNER_COMMAND: &str = "pnpm";
const DEFAULT_DSH_PACKAGE: &str = "@deepseek-ai/dsh@0.1.5-rc.2";
const PRODUCTION_DATA_DIR_NAME: &str = ".dsh-launcher";
const DEVELOPMENT_DATA_DIR_NAME: &str = ".dsh-launcher-dev";
const DATA_DIR_ENV: &str = "DSH_LAUNCHER_DATA_DIR";
const CONFIG_KEY_ORDER: &[&str] = &[
    "_comment",
    "_comment_port",
    "_comment_profile",
    "_comment_dshHome",
    "_comment_extraArgs",
    "_comment_environment",
    "_comment_runner",
    "port",
    "profile",
    "dshHome",
    "extraArgs",
    "environment",
    "runner",
    "_comment_runtime",
    "runtime",
];
const RUNNER_KEY_ORDER: &[&str] = &["command", "package", "allowBuild", "allowBuildFor"];

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
    /// 外部包管理器 Runner。未填写时使用 pnpm dlx 启动固定版本的 DSH。
    pub runner: Option<RunnerConfig>,
    /// 旧版外部 Node + entry 配置，保留用于兼容已有配置；runner 优先。
    pub runtime: Option<RuntimeConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct RunnerConfig {
    /// 可以填写 `pnpm`、`npx` 或可执行文件绝对路径。
    pub command: String,
    /// 必须是固定版本，例如 `@deepseek-ai/dsh@0.1.5-rc.2`。
    pub package: String,
    /// 用户确认后允许执行 postinstall 的依赖包名。
    #[serde(rename = "allowBuild")]
    pub allow_build: Vec<String>,
    /// `allowBuild` 针对的精确 DSH 包规格；版本变化后会重新预检。
    #[serde(rename = "allowBuildFor")]
    pub allow_build_for: Option<String>,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            command: DEFAULT_RUNNER_COMMAND.to_owned(),
            package: DEFAULT_DSH_PACKAGE.to_owned(),
            allow_build: Vec::new(),
            allow_build_for: None,
        }
    }
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

    pub fn effective_runner(&self) -> RunnerConfig {
        self.runner.clone().unwrap_or_default()
    }

    pub fn uses_pnpm_runner(&self) -> bool {
        let runner = self.effective_runner();
        let command_name = Path::new(&runner.command)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&runner.command)
            .to_ascii_lowercase();
        command_name == "pnpm" || command_name == "pnpm.exe"
    }

    /// 只有用户已经确认过同一个精确 DSH 包规格，才跳过构建许可预检。
    pub fn needs_build_preflight(&self) -> bool {
        if self.runner.is_none() && self.runtime.is_some() {
            return false;
        }
        let runner = self.effective_runner();
        self.uses_pnpm_runner() && runner.allow_build_for.as_deref() != Some(runner.package.as_str())
    }

    /// 预检只解析并安装到 pnpm 的临时缓存，不启动 Web 服务。
    pub fn runner_preflight_command(&self) -> Result<(String, Vec<String>), String> {
        let runner = self.effective_runner();
        validate_runner(&runner)?;
        if !self.uses_pnpm_runner() {
            return Err(
                "只有 pnpm Runner 支持自动发现 build 许可；请改用 pnpm 或直接配置 allowBuild。".to_owned(),
            );
        }
        Ok((
            runner.command,
            vec![
                "dlx".to_owned(),
                "--reporter".to_owned(),
                "append-only".to_owned(),
                runner.package,
                "--version".to_owned(),
            ],
        ))
    }

    /// 将用户确认的许可写回配置，同时绑定当前精确 DSH 包规格。
    pub fn persist_build_approval(
        &mut self,
        path: &Path,
        package: &str,
        dependencies: &[String],
    ) -> Result<(), String> {
        let mut document = if path.exists() {
            let contents = fs::read_to_string(path).map_err(|error| format!("读取配置失败：{error}"))?;
            serde_json::from_str::<serde_json::Value>(&contents)
                .map_err(|error| format!("解析配置失败：{error}"))?
        } else {
            serde_json::from_str::<serde_json::Value>(&Self::template_json())
                .map_err(|error| format!("生成配置模板失败：{error}"))?
        };
        let mut runner = self.effective_runner();
        runner.allow_build = dependencies.to_vec();
        runner.allow_build_for = Some(package.to_owned());
        let runner_value =
            serde_json::to_value(&runner).map_err(|error| format!("序列化 Runner 配置失败：{error}"))?;
        document
            .as_object_mut()
            .ok_or_else(|| "配置根节点必须是 JSON 对象。".to_owned())?
            .insert("runner".to_owned(), runner_value);
        reorder_config_document(&mut document);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("创建配置目录失败：{error}"))?;
        }
        let contents =
            serde_json::to_string_pretty(&document).map_err(|error| format!("格式化配置失败：{error}"))?;
        fs::write(path, format!("{contents}\n")).map_err(|error| format!("写入配置失败：{error}"))?;
        self.runner = Some(runner);
        Ok(())
    }

    /// 构造真正交给 `Command` 的程序和参数。
    ///
    /// Runner 是默认路径；旧的 `runtime.node`/`runtime.entry` 只在没有显式
    /// runner 时作为兼容回退。这样既不要求全局安装 dsh，也不会破坏已有的
    /// Swift 风格外部 entry 配置。
    pub fn launch_command(&self, port: u16, home: &Path) -> Result<(String, Vec<String>), String> {
        let dsh_arguments = self.arguments(port, home);
        if self.runner.is_some() || self.runtime.is_none() {
            let runner = self.effective_runner();
            validate_runner(&runner)?;
            let command_name = Path::new(&runner.command)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&runner.command)
                .to_ascii_lowercase();
            let mut arguments = Vec::new();
            if command_name == "pnpm" || command_name == "pnpm.exe" {
                arguments.push("dlx".to_owned());
                for dependency in &runner.allow_build {
                    arguments.push(format!("--allow-build={dependency}"));
                }
                arguments.push(runner.package);
            } else if command_name == "npx" || command_name == "npx.cmd" {
                arguments.push("--yes".to_owned());
                arguments.push(runner.package);
            } else {
                return Err(format!(
                    "不支持的 Runner 命令：{}。目前只支持 pnpm 或 npx。",
                    runner.command
                ));
            }
            arguments.extend(dsh_arguments);
            return Ok((runner.command, arguments));
        }

        let (executable, entry) = self.runtime_command();
        let mut arguments = Vec::new();
        if let Some(entry) = entry {
            arguments.push(entry);
        }
        arguments.extend(dsh_arguments);
        Ok((executable, arguments))
    }

    pub fn template_json() -> String {
        // `_comment` 会被 serde 忽略，保留在文件里是为了让第一次打开配置的
        // 用户知道每个字段的作用；runner 是默认启动链路，runtime 仅为旧配置保留。
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
  "_comment_runner": "默认通过外部 pnpm dlx 启动 DSH；首次或版本变化时会自动预检并请求构建许可。",
  "runner": {
    "command": "pnpm",
    "package": "@deepseek-ai/dsh@0.1.5-rc.2",
    "_comment_allowBuild": "只填写用户确认过的依赖；不要手工猜测，启动器会在首次启动时自动发现。",
    "allowBuild": [],
    "allowBuildFor": null
  },
  "_comment_runtime": "旧版兼容配置；仅在 runner 缺省时使用，例如 {\"node\":\"/path/to/node\",\"entry\":\"/path/to/dsh.js\"}。",
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

fn validate_runner(runner: &RunnerConfig) -> Result<(), String> {
    if runner.command.trim().is_empty() {
        return Err("Runner command 不能为空。".to_owned());
    }
    let prefix = "@deepseek-ai/dsh@";
    let Some(version) = runner.package.strip_prefix(prefix) else {
        return Err(format!(
            "Runner package 必须是固定版本的 {prefix}<version>，当前为：{}",
            runner.package
        ));
    };
    let version_core = version.split(['-', '+']).next().unwrap_or_default();
    let segments = version_core.split('.').collect::<Vec<_>>();
    if segments.len() != 3
        || segments
            .iter()
            .any(|segment| segment.is_empty() || !segment.chars().all(|c| c.is_ascii_digit()))
        || version
            .chars()
            .any(|character| matches!(character, '^' | '~' | '*' | '>' | '<' | '=' | ' '))
    {
        return Err(format!(
            "Runner package 必须使用精确版本，不能使用 latest、^、~ 或范围：{}",
            runner.package
        ));
    }
    if runner
        .allow_build
        .iter()
        .any(|name| name.trim().is_empty() || name.chars().any(|character| matches!(character, ',' | ' ')))
    {
        return Err("runner.allowBuild 中每项必须是单个依赖包名，不能包含逗号或空格。".to_owned());
    }
    Ok(())
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
    pub root: PathBuf,
    pub config: PathBuf,
    pub logs: PathBuf,
    pub preferences: PathBuf,
}

impl AppPaths {
    pub fn new(home: PathBuf) -> Self {
        let root = data_root(&home);
        Self {
            root: root.clone(),
            config: root.join("config.json"),
            logs: root.join("logs"),
            preferences: root.join("preferences.json"),
        }
    }
}

fn data_root(home: &Path) -> PathBuf {
    if let Some(raw) = std::env::var_os(DATA_DIR_ENV) {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() {
            return if path.is_absolute() { path } else { home.join(path) };
        }
    }

    // release 优化的测试包也必须保持隔离，不能只以 debug_assertions 判断身份。
    let name = if cfg!(debug_assertions) || cfg!(dsh_launcher_test_build) {
        DEVELOPMENT_DATA_DIR_NAME
    } else {
        PRODUCTION_DATA_DIR_NAME
    };
    home.join(name)
}

fn reorder_config_document(document: &mut serde_json::Value) {
    let Some(object) = document.as_object_mut() else {
        return;
    };

    if let Some(runner) = object
        .get_mut("runner")
        .and_then(serde_json::Value::as_object_mut)
    {
        reorder_object(runner, RUNNER_KEY_ORDER);
    }
    reorder_object(object, CONFIG_KEY_ORDER);
}

fn reorder_object(object: &mut serde_json::Map<String, serde_json::Value>, order: &[&str]) {
    let original = std::mem::take(object);
    let mut reordered = serde_json::Map::new();

    for key in order {
        if let Some(value) = original.get(*key).cloned() {
            reordered.insert((*key).to_owned(), value);
        }
    }
    for (key, value) in original {
        if !order.iter().any(|known| *known == key) {
            reordered.insert(key, value);
        }
    }

    *object = reordered;
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
    fn builds_pnpm_runner_command_with_multiple_build_permissions() {
        let config = ShellConfig {
            runner: Some(RunnerConfig {
                command: "/opt/homebrew/bin/pnpm".to_owned(),
                package: "@deepseek-ai/dsh@0.1.5-rc.2".to_owned(),
                allow_build: vec!["esbuild".to_owned(), "sharp".to_owned()],
                allow_build_for: None,
            }),
            ..ShellConfig::default()
        };
        let (command, arguments) = config.launch_command(31080, Path::new("/tmp/home")).unwrap();
        assert_eq!(command, "/opt/homebrew/bin/pnpm");
        assert_eq!(
            arguments,
            [
                "dlx",
                "--allow-build=esbuild",
                "--allow-build=sharp",
                "@deepseek-ai/dsh@0.1.5-rc.2",
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
    fn default_config_uses_pinned_pnpm_runner() {
        let (command, arguments) = ShellConfig::default()
            .launch_command(31080, Path::new("/tmp/home"))
            .unwrap();
        assert_eq!(command, "pnpm");
        assert!(arguments.contains(&"@deepseek-ai/dsh@0.1.5-rc.2".to_owned()));
        assert!(!arguments
            .iter()
            .any(|argument| argument.starts_with("--allow-build=")));
        assert!(ShellConfig::default().needs_build_preflight());
    }

    #[test]
    fn builds_pnpm_preflight_command_without_build_permissions() {
        let config = ShellConfig::default();
        let (command, arguments) = config.runner_preflight_command().unwrap();
        assert_eq!(command, "pnpm");
        assert_eq!(
            arguments,
            [
                "dlx",
                "--reporter",
                "append-only",
                "@deepseek-ai/dsh@0.1.5-rc.2",
                "--version"
            ]
        );
    }

    #[test]
    fn rejects_unpinned_runner_package() {
        let config = ShellConfig {
            runner: Some(RunnerConfig {
                package: "@deepseek-ai/dsh@latest".to_owned(),
                ..RunnerConfig::default()
            }),
            ..ShellConfig::default()
        };
        let error = config.launch_command(31080, Path::new("/tmp/home")).unwrap_err();
        assert!(error.contains("精确版本"), "{error}");
    }

    #[test]
    fn template_is_documented_and_loadable() {
        let template = ShellConfig::template_json();
        let config: ShellConfig = serde_json::from_str(&template).unwrap();
        assert_eq!(config.port, Some(31080));
        assert_eq!(config.profile(), "launcher");
        assert!(config.runtime.is_none());
        assert_eq!(
            config.runner.as_ref().map(|runner| runner.package.as_str()),
            Some("@deepseek-ai/dsh@0.1.5-rc.2")
        );
        assert_eq!(
            config
                .runner
                .as_ref()
                .and_then(|runner| runner.allow_build_for.as_deref()),
            None
        );
        assert!(template.contains("不会安装或升级 runtime"));
    }

    #[test]
    fn persists_approval_and_binds_it_to_package_version() {
        let path = std::env::temp_dir().join(format!(
            "dsh-launcher-preflight-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_file(&path);
        fs::write(
            &path,
            r#"{"_custom":"kept","runner":{"command":"pnpm","package":"@deepseek-ai/dsh@0.1.5-rc.2"}}"#,
        )
        .unwrap();
        let mut config = ShellConfig::default();
        let dependencies = vec!["node-pty".to_owned(), "koffi".to_owned()];
        config
            .persist_build_approval(&path, "@deepseek-ai/dsh@0.1.5-rc.2", &dependencies)
            .unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("_custom"));
        let loaded = ShellConfig::load(&path).unwrap();
        let runner = loaded.runner.as_ref().unwrap();
        assert_eq!(runner.allow_build, dependencies);
        assert_eq!(
            runner.allow_build_for.as_deref(),
            Some("@deepseek-ai/dsh@0.1.5-rc.2")
        );
        assert!(!loaded.needs_build_preflight());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn persists_config_in_documented_key_order() {
        let path = std::env::temp_dir().join(format!(
            "dsh-launcher-order-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_file(&path);
        fs::write(
            &path,
            r#"{"runtime":null,"profile":"launcher","runner":{"allowBuildFor":null,"package":"@deepseek-ai/dsh@0.1.5-rc.2","allowBuild":[],"command":"pnpm"},"port":31080}"#,
        )
        .unwrap();

        let mut config = ShellConfig::default();
        config
            .persist_build_approval(&path, "@deepseek-ai/dsh@0.1.5-rc.2", &["node-pty".to_owned()])
            .unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.find("\"port\"").unwrap() < saved.find("\"profile\"").unwrap());
        assert!(saved.find("\"profile\"").unwrap() < saved.find("\"runner\"").unwrap());
        assert!(saved.find("\"command\"").unwrap() < saved.find("\"package\"").unwrap());
        assert!(saved.find("\"package\"").unwrap() < saved.find("\"allowBuild\"").unwrap());
        assert!(saved.find("\"allowBuild\"").unwrap() < saved.find("\"allowBuildFor\"").unwrap());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn uses_development_data_directory_for_debug_or_test_builds() {
        let paths = AppPaths::new(PathBuf::from("/tmp/dsh-launcher-home"));
        let expected = if cfg!(debug_assertions) || cfg!(dsh_launcher_test_build) {
            "/tmp/dsh-launcher-home/.dsh-launcher-dev"
        } else {
            "/tmp/dsh-launcher-home/.dsh-launcher"
        };
        assert_eq!(paths.root, PathBuf::from(expected));
    }
}
