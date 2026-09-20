fn main() {
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
        let value = std::env::var(key).unwrap_or_else(|_| fallback.to_owned());
        println!("cargo:rustc-env={key}={value}");
    }
    tauri_build::build();
}
