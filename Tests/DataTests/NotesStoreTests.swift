import XCTest
@testable import Data
@testable import AppPaths

/// Store tests against a throwaway XDG data dir. AppPaths reads XDG_* on
/// every access, so pointing the env at a temp dir isolates every test from
/// the user's real notes.
///
/// Linux XCTest has no isolated setUp overrides, so each test claims its own
/// home via `freshHome()` and cleans up with `defer`.
final class NotesStoreTests: XCTestCase {

    private func freshHome() throws -> URL {
        let home = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("speechnotes-tests-\(UUID().uuidString)", isDirectory: true)
        setenv("XDG_DATA_HOME", home.path, 1)
        setenv("XDG_CONFIG_HOME", home.path, 1)
        setenv("XDG_CACHE_HOME", home.path, 1)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        return home
    }

    @MainActor
    private func freshStore() -> NotesStore {
        NotesStore()
    }

    @MainActor
    func testCreateNotesAndListActiveOnly() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()
        let store = freshStore()
        let a = store.createNote()
        var b = store.createNote()
        b.text = "second"
        store.update(b)

        XCTAssertEqual(store.notes.count, 2)
        store.delete(noteId: b.id)
        XCTAssertEqual(store.notes.map(\.id), [a.id])
        XCTAssertEqual(store.deletedNotes.map(\.id), [b.id])
    }

    @MainActor
    func testPinFavoriteMoveAndNotebookScope() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()
        let store = freshStore()
        let notebookId = UUID()
        let note = store.createNote(notebookId: notebookId)

        store.setPinned(true, noteId: note.id)
        store.setFavorite(true, noteId: note.id)
        XCTAssertTrue(store.notes[0].isPinned)
        XCTAssertTrue(store.notes[0].isFavorite)

        store.move(noteId: note.id, to: nil)
        XCTAssertNil(store.notes[0].notebookId)
        XCTAssertEqual(store.notes(inNotebook: nil).count, 1)
        XCTAssertEqual(store.notes(inNotebook: notebookId).count, 0)

        store.clearNotebook(UUID()) // unknown id: no-op
        XCTAssertEqual(store.notes.count, 1)
    }

    @MainActor
    func testRecoverPurgeAndEmptyRecycleBin() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()
        let store = freshStore()
        let a = store.createNote()
        let b = store.createNote()
        let c = store.createNote()
        store.delete(noteId: a.id)
        store.delete(noteId: b.id)
        store.delete(noteId: c.id)
        XCTAssertEqual(store.deletedNotes.count, 3)

        store.recover(noteId: a.id)
        XCTAssertEqual(store.notes.count, 1)
        XCTAssertEqual(store.deletedNotes.count, 2)

        store.purge(noteId: b.id)
        XCTAssertEqual(store.deletedNotes.map(\.id), [c.id])

        store.emptyRecycleBin()
        XCTAssertTrue(store.deletedNotes.isEmpty)
        // The recovered note `a` must survive the bin purge.
        XCTAssertEqual(store.notes.map(\.id), [a.id])
    }

    @MainActor
    func testRowMetadataDerivesTitlePreviewWordCount() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()
        let store = freshStore()
        var note = store.createNote()
        note.text = "Title line\nBody words that should preview here and keep going past one hundred characters so the prefix kicks in cleanly for the row."
        store.update(note)

        let fresh = store.notes[0]
        let meta = store.metadata(for: fresh)
        XCTAssertEqual(meta.title, "Title line")
        XCTAssertTrue(meta.preview.hasPrefix("Body words"))
        XCTAssertLessThanOrEqual(meta.preview.count, 120)
        XCTAssertEqual(meta.wordCount, fresh.wordCount)
        XCTAssertEqual(meta.listenMinutes, fresh.estimatedListenMinutes)
    }

    @MainActor
    func testFlushNowWritesNotesJSON() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()
        let store = freshStore()
        let note = store.createNote()
        var updated = note
        updated.text = "persisted body"
        store.update(updated)
        store.flushNow()

        let url = AppPaths.dataDir.appendingPathComponent("notes.json")
        XCTAssertTrue(FileManager.default.fileExists(atPath: url.path))
        let decoded = try JSONDecoder().decode([Note].self, from: Data(contentsOf: url))
        XCTAssertEqual(decoded.map(\.text), ["persisted body"])
    }

    @MainActor
    func testCorruptNotesJSONIsQuarantinedWithoutBackup() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()
        let url = AppPaths.dataDir.appendingPathComponent("notes.json")
        try Data("not json at all".utf8).write(to: url)

        let store = freshStore()
        XCTAssertTrue(store.notes.isEmpty)

        let quarantined = try FileManager.default.contentsOfDirectory(atPath: AppPaths.dataDir.path)
            .filter { $0.hasPrefix("notes.json.corrupt-") }
        XCTAssertEqual(quarantined.count, 1, "bad file must be preserved, not overwritten")
    }

    @MainActor
    func testCorruptNotesJSONRecoversFromBackup() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()
        let good = [Note(), Note()].map { note -> Note in
            var n = note
            n.text = "backup survivor"
            return n
        }
        let backupURL = AppPaths.dataDir.appendingPathComponent("notes.backup.json")
        try JSONEncoder().encode(good).write(to: backupURL)
        try Data("  garbage".utf8).write(
            to: AppPaths.dataDir.appendingPathComponent("notes.json")
        )

        let store = freshStore()
        XCTAssertEqual(store.notes.count, 2)
        XCTAssertEqual(store.notes[0].text, "backup survivor")
    }

    @MainActor
    func testUpdateInvalidatesPlaybackBookmark() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()
        let store = freshStore()
        let note = store.createNote()

        let key = BookmarkStore.noteKey(note.id)
        BookmarkStore.shared.set(key, PlaybackBookmark(noteId: note.id, textOffset: 42))
        XCTAssertNotNil(BookmarkStore.shared.get(key))

        store.update(note)  // text changed → saved position is stale
        XCTAssertNil(BookmarkStore.shared.get(key))
    }

    @MainActor
    func testExpiredDeletedNotesPrunedOnInit() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()
        let stale = Note()
        var expired = stale
        expired.deletedAt = Date().addingTimeInterval(-31 * 24 * 3600)
        let live = Note()
        try JSONEncoder().encode([expired, live]).write(
            to: AppPaths.dataDir.appendingPathComponent("notes.json")
        )

        let store = freshStore()
        XCTAssertEqual(store.allNotes.map(\.id), [live.id])
        XCTAssertTrue(store.deletedNotes.isEmpty)
    }
}
