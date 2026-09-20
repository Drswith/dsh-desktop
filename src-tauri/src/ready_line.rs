//! `dsh web:` 这行 stdout 是 Web Profile 的就绪信号，只在它真正能接请求之后才会
//! 打印，并且带着这个进程的启动 token（`/?token=…`）。

use url::Url;

const PREFIX: &str = "dsh web: ";

/// 从一行 stdout 里抠出带 token 的本机地址；不匹配就是 `None`。
pub(crate) fn authenticated_url(line: &str) -> Option<Url> {
    let clean = strip_ansi(line);
    let clean = clean.trim();
    let rest = clean.strip_prefix(PREFIX)?;
    // 可选的 ` (LAN: …)` 后缀跟在第一个空格之后，先切掉。
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

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// 去掉给 CLI 输出上色的 CSI 转义序列（`ESC [ … final`）。
fn strip_ansi(text: &str) -> String {
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
    fn keeps_multibyte_text_intact() {
        assert_eq!(strip_ansi("\u{1B}[31m错误\u{1B}[0m"), "错误");
    }
}
