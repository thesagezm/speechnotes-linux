import Foundation
import AppPaths
import Log

/// Per-item playback bookmark, persisted as `bookmarks.json`.
///
/// The old single-slot bookmark (one UserDefaults blob) meant starting book
/// B destroyed note A's resume position. Here every note (key
/// `note:<uuid>`) and every book chapter (key `book:<uuid>:<chapter>`) owns
/// its slot, capped at `maxEntries` most-recently-used.
///
/// Ported from the iOS app's BookmarkStore; `SpeechPlayer.PlaybackBookmark`
/// became the free-standing `PlaybackBookmark` so the data layer doesn't
/// depend on audio code.
@MainActor
final class BookmarkStore {
    static let shared = BookmarkStore()

    private(set) var bookmarks: [String: PlaybackBookmark] = [:]
    /// Keys touched this session, most recent first — drives LRU eviction.
    private var recency: [String] = []

    private static let maxEntries = 100

    private var fileURL: URL {
        AppPaths.dataDir.appendingPathComponent("bookmarks.json")
    }

    private init() {
        load()
    }

    static func noteKey(_ id: UUID) -> String { "note:\(id.uuidString)" }
    static func bookKey(_ id: String, chapter: Int) -> String { "book:\(id):\(chapter)" }

    func get(_ key: String) -> PlaybackBookmark? {
        guard let mark = bookmarks[key] else { return nil }
        touch(key)
        return mark
    }

    func set(_ key: String, _ mark: PlaybackBookmark) {
        bookmarks[key] = mark
        touch(key)
        schedulePersist()
    }

    func remove(_ key: String) {
        guard bookmarks[key] != nil else { return }
        bookmarks[key] = nil
        recency.removeAll { $0 == key }
        schedulePersist()
    }

    /// Marks the slot stale without dropping other items (a note's text
    /// changed, so its bookmark can no longer match).
    func removeAll(forNote id: UUID) {
        remove(Self.noteKey(id))
    }

    /// The most recent note-keyed bookmark if it was saved within `maxAge`
    /// seconds — the single auto-resume candidate when the app starts. Book
    /// bookmarks are never candidates (they resume from the reader's play).
    func mostRecentNoteBookmark(within maxAge: TimeInterval) -> (key: String, mark: PlaybackBookmark)? {
        guard let key = recency.first(where: { $0.hasPrefix("note:") }),
              let mark = bookmarks[key],
              mark.savedAt >= Date().addingTimeInterval(-maxAge) else { return nil }
        return (key, mark)
    }

    // MARK: - Private

    private func touch(_ key: String) {
        recency.removeAll { $0 == key }
        recency.insert(key, at: 0)
        while recency.count > Self.maxEntries {
            let evicted = recency.removeLast()
            bookmarks[evicted] = nil
        }
    }

    private var persistTask: Task<Void, Never>?
    private var lastPersist = Date.distantPast

    /// Throttle with a GUARANTEED trailing write (min interval 1 s). Playback
    /// ticks fire every ~0.3 s; a pure debounce would cancel the pending write
    /// on every tick and the file would never land mid-playback.
    private func schedulePersist() {
        let sinceLast = Date().timeIntervalSince(lastPersist)
        if sinceLast >= 1.0 {
            persistNow()
            return
        }
        guard persistTask == nil else { return } // trailing write already armed
        let wait = UInt64((1.0 - sinceLast) * 1_000_000_000)
        persistTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: wait)
            guard !Task.isCancelled else { return }
            self?.persistTask = nil
            self?.persistNow()
        }
    }

    func persistNow() {
        persistTask?.cancel()
        persistTask = nil
        do {
            let data = try JSONEncoder().encode(bookmarks)
            try data.write(to: fileURL, options: .atomic)
        } catch {
            Log.error("BookmarkStore: persist failed: \(error)")
        }
    }

    private func load() {
        guard FileManager.default.fileExists(atPath: fileURL.path) else { return }
        do {
            bookmarks = try JSONDecoder().decode(
                [String: PlaybackBookmark].self,
                from: try Data(contentsOf: fileURL)
            )
            recency = bookmarks.keys.sorted {
                (bookmarks[$0]?.savedAt ?? .distantPast) > (bookmarks[$1]?.savedAt ?? .distantPast)
            }
        } catch {
            // A bookmarks.json that fails to decode is worthless but not
            // precious — quarantine it and start fresh rather than crashing.
            Log.error("BookmarkStore: load failed: \(error) — quarantining bookmarks.json")
            let quarantine = fileURL.appendingPathExtension("corrupt-\(Int(Date().timeIntervalSince1970))")
            try? FileManager.default.moveItem(at: fileURL, to: quarantine)
        }
    }
}
