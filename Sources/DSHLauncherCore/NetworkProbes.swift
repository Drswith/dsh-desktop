import Foundation

/// Loopback port availability, checked with the same SO_REUSEADDR bind libuv uses.
public enum PortProbe {
    public static func isAvailable(port: Int) -> Bool {
        guard (1...65535).contains(port) else { return false }
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }
        var reuse: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))
        var address = loopback(port: port)
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        return bound == 0
    }

    /// The preferred port, or the next free one within `attempts`.
    public static func firstAvailable(startingAt port: Int, attempts: Int = 20) -> Int? {
        for candidate in port..<(port + max(1, attempts)) where candidate <= 65535 && isAvailable(port: candidate) {
            return candidate
        }
        return nil
    }

    static func loopback(port: Int) -> sockaddr_in {
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = in_port_t(UInt16(port).bigEndian)
        address.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))
        return address
    }
}

public enum HealthResult: Equatable, Sendable {
    /// Any HTTP status proves the server loop answers (an unauthenticated `/` is 401).
    case healthy(status: Int)
    case unhealthy(reason: String)

    public var isHealthy: Bool {
        if case .healthy = self { return true }
        return false
    }
}

/// Liveness probe over a raw loopback socket: no URLSession, so no ATS, proxy,
/// or cookie handling can interfere, and no credentials are sent.
public enum HealthProbe {
    public static func check(port: Int, timeout: TimeInterval = 5) -> HealthResult {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { return .unhealthy(reason: "socket: \(String(cString: strerror(errno)))") }
        defer { close(fd) }
        var noSigPipe: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSigPipe, socklen_t(MemoryLayout<Int32>.size))
        var interval = timeval(tv_sec: Int(timeout), tv_usec: Int32((timeout - floor(timeout)) * 1_000_000))
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &interval, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &interval, socklen_t(MemoryLayout<timeval>.size))

        var address = PortProbe.loopback(port: port)
        let connected = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard connected == 0 else { return .unhealthy(reason: "connect: \(String(cString: strerror(errno)))") }

        let request = "HEAD / HTTP/1.1\r\nHost: 127.0.0.1:\(port)\r\nUser-Agent: dsh-launcher-health\r\nConnection: close\r\n\r\n"
        let sent = request.withCString { send(fd, $0, strlen($0), 0) }
        guard sent > 0 else { return .unhealthy(reason: "send: \(String(cString: strerror(errno)))") }

        var buffer = [UInt8](repeating: 0, count: 512)
        let received = recv(fd, &buffer, buffer.count, 0)
        guard received > 0 else {
            return .unhealthy(reason: received == 0 ? "connection closed" : "recv: \(String(cString: strerror(errno)))")
        }
        let head = String(decoding: buffer[0..<received], as: UTF8.self)
        guard let status = parseStatus(head) else { return .unhealthy(reason: "not an HTTP response") }
        return .healthy(status: status)
    }

    static func parseStatus(_ head: String) -> Int? {
        guard head.hasPrefix("HTTP/") else { return nil }
        let parts = head.split(separator: " ", maxSplits: 2)
        guard parts.count >= 2, let status = Int(parts[1]), (100...599).contains(status) else { return nil }
        return status
    }
}
