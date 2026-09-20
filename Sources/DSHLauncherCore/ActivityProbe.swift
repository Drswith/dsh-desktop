import Darwin
import Foundation

/// Whether the dsh service is doing work, judged without its private API, which
/// changes with every dsh release: a child process (a tool command, a terminal)
/// or a session log written recently. An idle daemon has neither.
public enum ActivityProbe {
    /// Why the service looks busy, or nil when it looks idle.
    public static func busyReason(daemonPid: pid_t, sessionsRoot: URL, quietPeriod: TimeInterval, now: Date = Date()) -> String? {
        let children = childProcesses(of: daemonPid)
        if !children.isEmpty {
            return "\(children.count) child process\(children.count == 1 ? "" : "es") running"
        }
        if let written = latestSessionWrite(under: sessionsRoot), now.timeIntervalSince(written) < quietPeriod {
            return "a session log was written \(Int(now.timeIntervalSince(written)))s ago"
        }
        return nil
    }

    public static func childProcesses(of pid: pid_t) -> [pid_t] {
        var pids = [pid_t](repeating: 0, count: 256)
        let count = proc_listchildpids(pid, &pids, Int32(pids.count * MemoryLayout<pid_t>.size))
        guard count > 0 else { return [] }
        return pids.prefix(Int(count)).filter { $0 > 0 }
    }

    /// The newest modification time among session logs (`*.jsonl*`) under `root`.
    public static func latestSessionWrite(under root: URL) -> Date? {
        let keys: [URLResourceKey] = [.contentModificationDateKey, .isRegularFileKey]
        guard let files = FileManager.default.enumerator(at: root, includingPropertiesForKeys: keys, options: [.skipsHiddenFiles]) else {
            return nil
        }
        var latest: Date?
        for case let file as URL in files where file.lastPathComponent.contains(".jsonl") {
            guard let values = try? file.resourceValues(forKeys: Set(keys)), values.isRegularFile == true,
                  let modified = values.contentModificationDate else { continue }
            if latest.map({ modified > $0 }) ?? true { latest = modified }
        }
        return latest
    }
}
