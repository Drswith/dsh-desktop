//! The `dsh web:` stdout line is the Web profile's readiness signal: it is printed
//! only after the Loader tree settles, and carries the process launch token
//! (`/?token=…`) that mints the browser cookie.

use url::Url;

pub const PREFIX: &str = "dsh web: ";

/// Extract the authenticated loopback URL from one stdout line.
pub fn authenticated_url(line: &str) -> Option<Url> {
    let clean = strip_ansi(line);
    let clean = clean.trim();
    let rest = clean.strip_prefix(PREFIX)?;
    // The optional ` (LAN: …)` suffix follows the first space.
    let token = rest.split_whitespace().next()?;
    let url = Url::parse(token).ok()?;
    if url.scheme() != "http" {
        return None;
    }
    match url.host_str() {
        Some("127.0.0.1") | Some("localhost") => {}
        _ => return None,
    }
    let has_token = url
        .query_pairs()
        .any(|(name, value)| name == "token" && !value.is_empty());
    has_token.then_some(url)
}

/// The same origin without credentials, safe to display or log.
pub fn clean_url(url: &Url) -> Url {
    let mut clean = url.clone();
    clean.set_query(None);
    clean.set_fragment(None);
    clean
}

/// Replace every `token=` query value so launch tokens never reach log files.
pub fn redact(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"token=") {
            let mut end = index + b"token=".len();
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_' || bytes[end] == b'-')
            {
                end += 1;
            }
            if end > index + b"token=".len() {
                out.push_str("token=<redacted>");
                index = end;
                continue;
            }
        }
        // Text is UTF-8; copying one byte at a time is safe only on boundaries,
        // so step by whole characters instead.
        let char_len = utf8_len(bytes[index]);
        out.push_str(&text[index..index + char_len]);
        index += char_len;
    }
    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Drop CSI escape sequences (`ESC [ … final`), which color the CLI's output.
pub fn strip_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == 0x1B && index + 1 < bytes.len() && bytes[index + 1] == b'[' {
            let mut end = index + 2;
            while end < bytes.len() && matches!(bytes[end], b'0'..=b'9' | b';' | b'?' | b' '..=b'/') {
                end += 1;
            }
            if end < bytes.len() && matches!(bytes[end], b'@'..=b'~') {
                index = end + 1;
                continue;
            }
        }
        let char_len = utf8_len(bytes[index]);
        out.push_str(&text[index..index + char_len]);
        index += char_len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_authenticated_loopback_url() {
        let url = authenticated_url("dsh web: http://127.0.0.1:31080/?token=abc_DEF-123").unwrap();
        assert_eq!(url.port(), Some(31080));
        assert_eq!(clean_url(&url).as_str(), "http://127.0.0.1:31080/");
    }

    #[test]
    fn ignores_lan_suffix_and_color() {
        let line = "\u{1B}[32mdsh web: http://127.0.0.1:4000/?token=t0k (LAN: http://192.168.1.2:4000/?token=t0k)\u{1B}[0m";
        let url = authenticated_url(line).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.port(), Some(4000));
    }

    #[test]
    fn rejects_lines_without_token_or_on_other_hosts() {
        assert!(
            authenticated_url("dsh web: opening the default browser; pass --no-open to disable").is_none()
        );
        assert!(authenticated_url("dsh web: http://127.0.0.1:4000/").is_none());
        assert!(authenticated_url("dsh web: http://evil.example:4000/?token=x").is_none());
        assert!(authenticated_url("web-app: could not open the default browser").is_none());
    }

    #[test]
    fn redacts_every_token() {
        let redacted =
            redact("dsh web: http://127.0.0.1:1/?token=secret (LAN: http://10.0.0.2:1/?token=secret)");
        assert!(!redacted.contains("secret"));
        assert_eq!(redacted.matches("token=<redacted>").count(), 2);
    }

    #[test]
    fn keeps_multibyte_text_intact() {
        assert_eq!(
            redact("启动完成 token=abc 就绪"),
            "启动完成 token=<redacted> 就绪"
        );
        assert_eq!(strip_ansi("\u{1B}[31m错误\u{1B}[0m"), "错误");
    }
}
