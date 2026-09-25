import Foundation

/// Where the app stores its data, per the XDG Base Directory Specification.
///
///   data   ~/.local/share/speechnotes    notes.json, notebooks.json, books/, models/
///   cache  ~/.cache/speechnotes          expendable caches
///   config ~/.config/speechnotes         preferences (if not UserDefaults)
///
/// Honors XDG_*_HOME overrides. This is the only place with filesystem roots
/// so nothing else in the app hardcodes paths.
public enum AppPaths {
    private static let appID = "speechnotes"

    private static func envOr(_ key: String, fallback: String) -> String {
        if let v = ProcessInfo.processInfo.environment[key], !v.isEmpty, v.hasPrefix("/") {
            return v
        }
        return NSHomeDirectory() + "/" + fallback
    }

    public static var dataDir: URL {
        URL(fileURLWithPath: envOr("XDG_DATA_HOME", fallback: ".local/share") + "/\(appID)", isDirectory: true)
    }

    public static var cacheDir: URL {
        URL(fileURLWithPath: envOr("XDG_CACHE_HOME", fallback: ".cache") + "/\(appID)", isDirectory: true)
    }

    public static var configDir: URL {
        URL(fileURLWithPath: envOr("XDG_CONFIG_HOME", fallback: ".config") + "/\(appID)", isDirectory: true)
    }

    public static var booksDir: URL {
        dataDir.appendingPathComponent("Books", isDirectory: true)
    }

    public static var modelsDir: URL {
        dataDir.appendingPathComponent("models", isDirectory: true)
    }

    public static var exportsDir: URL {
        dataDir.appendingPathComponent("Exports", isDirectory: true)
    }

    public static var noteImagesDir: URL {
        dataDir.appendingPathComponent("note-images", isDirectory: true)
    }

    public static var logFile: URL {
        dataDir.appendingPathComponent("speechnotes.log")
    }

    /// Set when a directory could not be created; the caller logs it.
    @MainActor public static var lastError: String?

    /// Creates every directory the app will write to. Safe to call repeatedly.
    /// Failure is not fatal: a read-only home still opens the UI, and stores
    /// report their own write failures when they happen.
    @MainActor
    @discardableResult
    public static func ensureDirectories() -> String? {
        lastError = nil
        let fm = FileManager.default
        for dir in [dataDir, cacheDir, configDir, booksDir, modelsDir, exportsDir, noteImagesDir] {
            if !fm.fileExists(atPath: dir.path) {
                do {
                    try fm.createDirectory(at: dir, withIntermediateDirectories: true)
                } catch {
                    // The directory-creation failure surfaces at the point of
                    // use (a store failing to write), so AppPaths stays
                    // dependency-free and only reports via the return value.
                    lastError = "\(dir.path): \(error.localizedDescription)"
                }
            }
        }
        return lastError
    }
}
