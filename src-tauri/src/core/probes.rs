//! Loopback port availability and a liveness probe over a raw socket: no HTTP
//! client, so no proxy, ATS or cookie handling can interfere, and no credentials
//! are ever sent.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::time::Duration;

pub fn loopback(port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
}

/// Whether a loopback listener can take this port right now. `TcpListener` binds
/// with `SO_REUSEADDR`, the same option libuv uses, so the answer matches what
/// the daemon will find.
pub fn port_is_available(port: u16) -> bool {
    port != 0 && TcpListener::bind(loopback(port)).is_ok()
}

/// The preferred port, or the next free one within `attempts`.
pub fn first_available_port(start: u16, attempts: u16) -> Option<u16> {
    (start..=u16::MAX)
        .take(attempts.max(1) as usize)
        .find(|port| port_is_available(*port))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthResult {
    /// Any HTTP status proves the server loop answers (an unauthenticated `/` is 401).
    Healthy {
        status: u16,
    },
    Unhealthy {
        reason: String,
    },
}

impl HealthResult {
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthResult::Healthy { .. })
    }

    pub fn reason(&self) -> String {
        match self {
            HealthResult::Healthy { status } => format!("HTTP {status}"),
            HealthResult::Unhealthy { reason } => reason.clone(),
        }
    }
}

pub fn health_check(port: u16, timeout: Duration) -> HealthResult {
    let unhealthy = |reason: String| HealthResult::Unhealthy { reason };
    let mut stream = match TcpStream::connect_timeout(&loopback(port), timeout) {
        Ok(stream) => stream,
        Err(error) => return unhealthy(format!("connect: {error}")),
    };
    if stream.set_read_timeout(Some(timeout)).is_err() || stream.set_write_timeout(Some(timeout)).is_err() {
        return unhealthy("could not arm the socket timeouts".to_owned());
    }
    let request = format!(
        "HEAD / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: dsh-launcher-health\r\nConnection: close\r\n\r\n"
    );
    if let Err(error) = stream.write_all(request.as_bytes()) {
        return unhealthy(format!("send: {error}"));
    }
    let mut buffer = [0u8; 512];
    match stream.read(&mut buffer) {
        Ok(0) => unhealthy("connection closed".to_owned()),
        Ok(read) => match parse_status(&String::from_utf8_lossy(&buffer[..read])) {
            Some(status) => HealthResult::Healthy { status },
            None => unhealthy("not an HTTP response".to_owned()),
        },
        Err(error) => unhealthy(format!("recv: {error}")),
    }
}

fn parse_status(head: &str) -> Option<u16> {
    if !head.starts_with("HTTP/") {
        return None;
    }
    let status: u16 = head.split(' ').nth(1)?.parse().ok()?;
    (100..=599).contains(&status).then_some(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::thread;

    /// Minimal loopback listener that answers every connection with `response`.
    struct TinyServer {
        port: u16,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl TinyServer {
        fn new(response: &'static str) -> TinyServer {
            let listener = TcpListener::bind(loopback(0)).expect("listener");
            let port = listener.local_addr().unwrap().port();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = stop.clone();
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if flag.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let Ok(mut stream) = stream else { return };
                    let mut scratch = [0u8; 1024];
                    let _ = stream.read(&mut scratch);
                    let _ = stream.write_all(response.as_bytes());
                }
            });
            TinyServer { port, stop }
        }
    }

    impl Drop for TinyServer {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = TcpStream::connect(loopback(self.port));
        }
    }

    #[test]
    fn health_probe_accepts_any_http_status() {
        let server = TinyServer::new("HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n");
        assert_eq!(
            health_check(server.port, Duration::from_secs(2)),
            HealthResult::Healthy { status: 401 }
        );
        assert!(!port_is_available(server.port));
        assert_ne!(first_available_port(server.port, 5), Some(server.port));
    }

    #[test]
    fn health_probe_rejects_non_http_and_closed_ports() {
        let server = TinyServer::new("SSH-2.0-OpenSSH\r\n");
        assert!(!health_check(server.port, Duration::from_secs(2)).is_healthy());
        let free = first_available_port(43_000, 200).expect("a free port");
        assert!(!health_check(free, Duration::from_secs(1)).is_healthy());
    }
}
