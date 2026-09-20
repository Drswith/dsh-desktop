//! GUI apps start with launchd's minimal environment. dsh runs the user's tools
//! (git, package managers, language runtimes), so the daemon receives the
//! environment of the user's login shell instead, resolved like VS Code does.

use std::collections::HashMap;
use std::time::Duration;

use crate::core::command::CommandSpec;
use crate::core::{Error, Result};

pub const BEGIN_MARKER: &str = "__DSH_LAUNCHER_ENV_BEGIN__";
pub const END_MARKER: &str = "__DSH_LAUNCHER_ENV_END__";

/// Launch-time variables that describe this app rather than the user's session.
const DROPPED_KEYS: &[&str] = &[
    "__CFBundleIdentifier",
    "XPC_SERVICE_NAME",
    "XPC_FLAGS",
    "OLDPWD",
    "PWD",
    "SHLVL",
    "_",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
];

pub const FALLBACK_PATH: &str =
    "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin";

/// The user's login shell from the passwd database, or `$SHELL`, or zsh.
pub fn login_shell() -> String {
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if !entry.is_null() && !(*entry).pw_shell.is_null() {
            let shell = std::ffi::CStr::from_ptr((*entry).pw_shell)
                .to_string_lossy()
                .into_owned();
            if !shell.is_empty() {
                return shell;
            }
        }
    }
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned())
}

/// Run `$SHELL -l -i -c 'env -0'` and parse the environment between the markers.
pub fn resolve_login_environment(shell: &str, timeout: Duration) -> Result<HashMap<String, String>> {
    let shell = if shell.is_empty() { "/bin/zsh" } else { shell };
    let script = format!("printf '%s' '{BEGIN_MARKER}'; /usr/bin/env -0; printf '%s' '{END_MARKER}'");
    let mut base: HashMap<String, String> = std::env::vars().collect();
    base.insert("DSH_LAUNCHER_RESOLVING_SHELL_ENV".to_owned(), "1".to_owned());
    let result = CommandSpec::new(shell, ["-l", "-i", "-c", script.as_str()])
        .environment(base)
        .timeout(timeout)
        .run()?;
    if result.timed_out {
        return Err(Error::new(format!(
            "login shell timed out after {}s",
            timeout.as_secs()
        )));
    }
    parse(&result.stdout).ok_or_else(|| {
        Error::new(format!(
            "login shell printed no environment (status {})",
            result.status
        ))
    })
}

/// Extract `KEY=VALUE` records (NUL separated) between the markers.
pub fn parse(data: &[u8]) -> Option<HashMap<String, String>> {
    let begin = find(data, BEGIN_MARKER.as_bytes())? + BEGIN_MARKER.len();
    let end = begin + find(&data[begin..], END_MARKER.as_bytes())?;
    let mut environment = HashMap::new();
    for record in data[begin..end].split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(record);
        let Some(equals) = text.find('=') else { continue };
        if equals == 0 {
            continue;
        }
        environment.insert(text[..equals].to_owned(), text[equals + 1..].to_owned());
    }
    (!environment.is_empty()).then_some(environment)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Compose the daemon environment: login shell (or current) values, minus launch
/// artifacts, with a usable PATH, then explicit overrides.
pub fn daemon_environment(
    login: Option<&HashMap<String, String>>,
    current: &HashMap<String, String>,
    overrides: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut environment = login.cloned().unwrap_or_else(|| current.clone());
    for key in DROPPED_KEYS {
        environment.remove(*key);
    }
    environment.remove("DSH_LAUNCHER_RESOLVING_SHELL_ENV");
    if login.is_none() {
        // Without a login shell, the common tool directories come first, as a login PATH would put them.
        let extra = environment.get("PATH").cloned().unwrap_or_default();
        environment.insert("PATH".to_owned(), merge_path(FALLBACK_PATH, &extra));
    } else if environment.get("PATH").map(String::is_empty).unwrap_or(true) {
        environment.insert("PATH".to_owned(), FALLBACK_PATH.to_owned());
    }
    environment
        .entry("HOME".to_owned())
        .or_insert_with(|| crate::core::home_dir().to_string_lossy().into_owned());
    environment
        .entry("LANG".to_owned())
        .or_insert_with(|| "en_US.UTF-8".to_owned());
    for (key, value) in overrides {
        environment.insert(key.clone(), value.clone());
    }
    environment
}

/// The current process environment, the base the daemon falls back to.
pub fn current_environment() -> HashMap<String, String> {
    std::env::vars().collect()
}

fn merge_path(path: &str, extra: &str) -> String {
    let mut seen = Vec::new();
    for entry in path.split(':').chain(extra.split(':')) {
        if !entry.is_empty() && !seen.contains(&entry) {
            seen.push(entry);
        }
    }
    seen.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn parses_nul_separated_environment_between_markers() {
        let mut data = format!("motd noise\n{BEGIN_MARKER}").into_bytes();
        data.extend_from_slice(b"PATH=/opt/homebrew/bin:/usr/bin\0MULTI=a\nb=c\0EMPTY=\0");
        data.extend_from_slice(format!("{END_MARKER}trailing").as_bytes());
        let environment = parse(&data).unwrap();
        assert_eq!(environment["PATH"], "/opt/homebrew/bin:/usr/bin");
        assert_eq!(environment["MULTI"], "a\nb=c");
        assert_eq!(environment["EMPTY"], "");
        assert!(parse(b"no markers").is_none());
    }

    #[test]
    fn daemon_environment_drops_launch_artifacts_and_applies_overrides() {
        let login = map(&[
            ("PATH", "/custom/bin:/usr/bin"),
            ("HOME", "/Users/me"),
            ("__CFBundleIdentifier", "x"),
            ("SHLVL", "2"),
            ("DSH_LAUNCHER_RESOLVING_SHELL_ENV", "1"),
        ]);
        let environment = daemon_environment(Some(&login), &HashMap::new(), &map(&[("FOO", "bar")]));
        assert_eq!(environment["PATH"], "/custom/bin:/usr/bin");
        assert!(!environment.contains_key("__CFBundleIdentifier"));
        assert!(!environment.contains_key("SHLVL"));
        assert!(!environment.contains_key("DSH_LAUNCHER_RESOLVING_SHELL_ENV"));
        assert_eq!(environment["FOO"], "bar");
        assert!(environment.contains_key("LANG"));
    }

    #[test]
    fn fallback_path_extends_the_launchd_path() {
        let current = map(&[("PATH", "/usr/bin:/bin"), ("HOME", "/Users/me")]);
        let environment = daemon_environment(None, &current, &HashMap::new());
        let entries: Vec<&str> = environment["PATH"].split(':').collect();
        assert_eq!(entries.first(), Some(&"/opt/homebrew/bin"));
        assert!(entries.contains(&"/usr/bin"));
        let mut unique = entries.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), entries.len(), "no duplicate PATH entries");
    }

    #[test]
    fn resolves_the_environment_of_a_real_login_shell() {
        let environment =
            resolve_login_environment("/bin/zsh", Duration::from_secs(20)).expect("login shell");
        assert!(environment.contains_key("PATH"), "{environment:?}");
    }
}
