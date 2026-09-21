use std::process::Command;

fn main() {
    // 测试包和正式包共用源码，但要在编译期选择不同的托盘资源。显式声明
    // rerun-if-env-changed，确保在两种构建之间切换时 Cargo 不复用错误的产物。
    println!("cargo:rerun-if-env-changed=DSH_LAUNCHER_TEST_BUILD");
    println!("cargo:rustc-check-cfg=cfg(dsh_launcher_test_build)");
    if std::env::var_os("DSH_LAUNCHER_TEST_BUILD").is_some() {
        println!("cargo:rustc-cfg=dsh_launcher_test_build");
    }

    // Build identity the bundle task supplies; each one has a development default
    // so `cargo run` and `cargo test` work without the task.
    for (key, fallback) in [
        ("DSH_LAUNCHER_VERSION_LABEL", "0.0.0-dev"),
        ("DSH_LAUNCHER_BUILD_NUMBER", "0"),
        ("DSH_LAUNCHER_GIT_COMMIT", "unknown"),
        ("DSH_LAUNCHER_BUILD_DATE", ""),
        ("DSH_LAUNCHER_REPO_URL", "https://github.com/Drswith/dsh-launcher"),
        ("DSH_LAUNCHER_HOME_DIR_NAME", ".dsh-launcher"),
        ("DSH_LAUNCHER_PROFILE", "launcher"),
        ("DSH_LAUNCHER_URL_SCHEME", "dsh-launcher"),
        ("DSH_LAUNCHER_DEFAULT_PORT", "31080"),
        ("DSH_LAUNCHER_RUNTIME_VERSION", "external"),
    ] {
        println!("cargo:rerun-if-env-changed={key}");
        let value = std::env::var(key)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| generated_identity(key))
            .unwrap_or_else(|| fallback.to_owned());
        println!("cargo:rustc-env={key}={value}");
    }
    tauri_build::build();
}

fn generated_identity(key: &str) -> Option<String> {
    let format = match key {
        "DSH_LAUNCHER_GIT_COMMIT" => "%h",
        "DSH_LAUNCHER_BUILD_DATE" => "%cI",
        _ => return None,
    };
    let output = Command::new("git")
        .args(["show", "-s", &format!("--format={format}"), "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}
