//! Build-time identity, baked in by `build.rs` from the values the bundle task
//! computes (version label, commit, build date, repository).

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

pub struct AppInfo {
    pub display_name: String,
    /// Numeric marketing version, e.g. `0.2.0`.
    pub version: String,
    /// Full version with any pre-release suffix, e.g. `0.2.0-beta.1`.
    pub version_label: &'static str,
    pub build: &'static str,
    pub bundle_identifier: String,
    pub home_dir_name: &'static str,
    pub profile: &'static str,
    pub url_scheme: &'static str,
    pub default_port: u16,
    /// Commit of the launcher source, `-dirty` when built from uncommitted changes.
    pub git_commit: Option<&'static str>,
    /// ISO 8601 build time with offset, e.g. `2026-09-19T10:05:10+08:00`.
    pub build_date: Option<&'static str>,
    /// Source repository as an https URL.
    pub repo_url: Option<&'static str>,
    pub payload_directory: Option<PathBuf>,
    pub bundle_path: PathBuf,
}

impl AppInfo {
    pub fn load(app: &AppHandle) -> AppInfo {
        let package = app.package_info();
        let non_empty = |value: &'static str| (!value.is_empty()).then_some(value);
        AppInfo {
            display_name: package.name.clone(),
            version: package.version.to_string(),
            version_label: env!("DSH_LAUNCHER_VERSION_LABEL"),
            build: env!("DSH_LAUNCHER_BUILD_NUMBER"),
            bundle_identifier: app.config().identifier.clone(),
            home_dir_name: env!("DSH_LAUNCHER_HOME_DIR_NAME"),
            profile: env!("DSH_LAUNCHER_PROFILE"),
            url_scheme: env!("DSH_LAUNCHER_URL_SCHEME"),
            default_port: env!("DSH_LAUNCHER_DEFAULT_PORT").parse().unwrap_or(31080),
            git_commit: non_empty(env!("DSH_LAUNCHER_GIT_COMMIT")).filter(|value| *value != "unknown"),
            build_date: non_empty(env!("DSH_LAUNCHER_BUILD_DATE")),
            repo_url: non_empty(env!("DSH_LAUNCHER_REPO_URL")),
            payload_directory: payload_directory(app),
            bundle_path: bundle_path(),
        }
    }

    /// About-dialog line for the repository: `GitHub: Drswith/dsh-launcher`, or
    /// `Repo: host/path` for other hosts so a GitLab repo is never labeled GitHub.
    pub fn repo_entry(&self) -> Option<(&'static str, String)> {
        let url = self.repo_url?;
        let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
        let (host, path) = match rest.split_once('/') {
            Some((host, path)) => (host, path.trim_matches('/')),
            None => (rest, ""),
        };
        if host == "github.com" && !path.is_empty() {
            return Some(("GitHub", path.to_owned()));
        }
        Some((
            "Repo",
            if path.is_empty() {
                host.to_owned()
            } else {
                format!("{host}/{path}")
            },
        ))
    }

    /// Login items registered from a build folder break once the app moves.
    pub fn is_in_stable_location(&self) -> bool {
        let path = self.bundle_path.to_string_lossy().into_owned();
        let user_applications = crate::core::home_dir().join("Applications");
        path.starts_with("/Applications/")
            || path.starts_with(&format!("{}/", user_applications.to_string_lossy()))
    }
}

/// `Contents/Resources/payload`, or `DSH_LAUNCHER_PAYLOAD_DIR` for unbundled development runs.
fn payload_directory(app: &AppHandle) -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("DSH_LAUNCHER_PAYLOAD_DIR").map(PathBuf::from),
        app.path().resource_dir().ok().map(|dir| dir.join("payload")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|dir| dir.join("manifest.json").exists())
}

fn bundle_path() -> PathBuf {
    let executable = std::env::current_exe().unwrap_or_default();
    // …/DSH Launcher.app/Contents/MacOS/dsh-launcher
    executable
        .ancestors()
        .find(|path| {
            path.extension()
                .map(|extension| extension == "app")
                .unwrap_or(false)
        })
        .map(|path| path.to_path_buf())
        .unwrap_or(executable)
}

/// "1 周前" / "1 week ago" for the build date shown in the About dialog.
pub fn relative_age(iso_date: &str) -> Option<String> {
    let then = parse_iso8601(iso_date)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let days = (now - then).max(0) / 86_400;
    Some(match days {
        0 => super::i18n::tr("age.today"),
        1..=13 => super::i18n::tr1("age.days", days),
        14..=59 => super::i18n::tr1("age.weeks", days / 7),
        60..=364 => super::i18n::tr1("age.months", days / 30),
        _ => super::i18n::tr1("age.years", days / 365),
    })
}

/// Parse `2026-09-19T10:05:10+08:00` (or `…Z`) into Unix seconds.
fn parse_iso8601(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let year = number(0..4)?;
    let month = number(5..7)?;
    let day = number(8..10)?;
    let hour = number(11..13)?;
    let minute = number(14..16)?;
    let second = number(17..19)?;
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    parts.tm_year = (year - 1900) as i32;
    parts.tm_mon = (month - 1) as i32;
    parts.tm_mday = day as i32;
    parts.tm_hour = hour as i32;
    parts.tm_min = minute as i32;
    parts.tm_sec = second as i32;
    let utc = unsafe { libc::timegm(&mut parts) } as i64;
    let offset = match text.get(19..) {
        Some(rest) if rest.starts_with('+') || rest.starts_with('-') => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            let hours = rest.get(1..3)?.parse::<i64>().ok()?;
            let minutes = rest
                .get(4..6)
                .or_else(|| rest.get(3..5))
                .and_then(|text| text.parse::<i64>().ok())
                .unwrap_or(0);
            sign * (hours * 3600 + minutes * 60)
        }
        _ => 0,
    };
    Some(utc - offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_offsets_and_utc() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601("1970-01-01T08:00:00+08:00"), Some(0));
        assert_eq!(parse_iso8601("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(parse_iso8601("nope"), None);
    }
}
