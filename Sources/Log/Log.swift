import Foundation
import AppPaths

/// Minimal leveled logger: a ring buffer in memory + a bounded tail file on
/// disk so the Settings → Logs screen survives a crash.
///
/// Mirrors the iOS app's LogStore: same shape (ring + on-disk tail), same
/// purposes (crash forensics from a user's device), no OS-specific APIs.
public enum Log {
    private static let lock = NSLock()
    nonisolated(unsafe) private static var lines: [String] = []
    private static let capacity = 300
    private static let maxFileBytes = 2 * 1024 * 1024

    private static var fileURL: URL? {
        AppPaths.logFile
    }

    public static func info(_ message: @autoclosure () -> String) { append("INFO", message()) }
    public static func error(_ message: @autoclosure () -> String) { append("ERROR", message()) }
    public static func debug(_ message: @autoclosure() -> String) { append("DEBUG", message()) }

    /// All buffered lines, oldest first (safe to call from the UI).
    public static func recent() -> [String] {
        lock.lock(); defer { lock.unlock() }
        return lines
    }

    private static func append(_ level: String, _ message: String) {
        let stamp = Self.timestamp()
        let line = "[\(stamp)] \(level): \(message)"
        lock.lock()
        lines.append(line)
        if lines.count > capacity { lines.removeFirst(lines.count - capacity) }
        lock.unlock()
        FileHandle.standardError.write((line + "\n").data(using: .utf8)!)
        appendToDisk(line)
    }

    private static func appendToDisk(_ line: String) {
        guard let url = fileURL else { return }
        guard let data = (line + "\n").data(using: .utf8) else { return }
        if let handle = try? FileHandle(forWritingTo: url) {
            defer { try? handle.close() }
            if (try? handle.seekToEnd()) != nil {
                try? handle.write(contentsOf: data)
            }
        } else {
            try? data.write(to: url)
        }
        // Trim if over cap (checked lazily: reading the size is cheap).
        if let size = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? Int),
           size > maxFileBytes {
            trim(url, keepBytes: maxFileBytes / 2)
        }
    }

    private static func trim(_ url: URL, keepBytes: Int) {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return }
        defer { try? handle.close() }
        guard let size = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? Int),
              size > keepBytes else { return }
        try? handle.seek(toOffset: UInt64(size - keepBytes))
        let tail = (try? handle.readToEnd()) ?? Data()
        try? tail.write(to: url)
    }

    nonisolated(unsafe) private static let formatter: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime]
        return f
    }()

    private static func timestamp() -> String {
        formatter.string(from: Date())
    }
}
