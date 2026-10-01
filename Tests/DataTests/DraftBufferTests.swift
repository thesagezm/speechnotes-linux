import XCTest
import AppPaths
@testable import Data

/// The editor draft's commit rules.
///
/// These are the behaviours that were missing when every keystroke wrote
/// straight through to `NotesStore.update` — which bumped the library
/// version, evicted the row-metadata cache, dropped playback bookmarks and
/// re-sorted the whole list *per character*. The buffer exists to stop that,
/// and these tests are what keep it stopped.
///
/// Each test gets its own XDG home (the `freshHome()` pattern the other Data
/// tests use) rather than a setUp override: on Linux XCTest the base async
/// method is nonisolated, so a `@MainActor` override fails to compile.
@MainActor
final class DraftBufferTests: XCTestCase {
    private func freshHome() throws -> URL {
        let home = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("speechnotes-draft-\(UUID().uuidString)", isDirectory: true)
        setenv("XDG_DATA_HOME", home.path, 1)
        setenv("XDG_CONFIG_HOME", home.path, 1)
        setenv("XDG_CACHE_HOME", home.path, 1)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        return home
    }

    /// A store in a throwaway home, plus one note with known content.
    private func fixture(text: String = "hello", title: String? = nil) throws
        -> (store: NotesStore, note: Note)
    {
        let home = try freshHome()
        _ = AppPaths.ensureDirectories()
        let store = NotesStore()
        var note = store.createNote()
        note.text = text
        note.explicitTitle = title
        store.update(note)
        // Reload so the caller gets the store's own copy.
        return (store, store.allNotes.first { $0.id == note.id }!)
    }

    private func stored(_ store: NotesStore, _ id: UUID) -> Note? {
        store.allNotes.first { $0.id == id }
    }

    // MARK: - Load and no-op

    /// Opening a note and closing it again must not write. A flush that
    /// touches the store bumps `updatedAt`, which re-sorts the list — so a
    /// read-only visit has to be silent.
    func testUneditedDraftDoesNotWrite() throws {
        let (store, note) = try fixture()
        var draft = DraftBuffer()
        draft.load(note: note)

        XCTAssertFalse(draft.hasPendingEdits)
        XCTAssertFalse(draft.flushInto(notes: store))
        XCTAssertEqual(stored(store, note.id)?.updatedAt, note.updatedAt, "an unedited visit moved updatedAt")
    }

    // MARK: - Edits

    func testEditsAreHeldUntilFlush() throws {
        let (store, note) = try fixture()
        var draft = DraftBuffer()
        draft.load(note: note)

        XCTAssertTrue(draft.setText("hello world"))
        XCTAssertTrue(draft.hasPendingEdits)
        // Not written yet — that is the whole point.
        XCTAssertEqual(stored(store, note.id)?.text, "hello")

        XCTAssertTrue(draft.flushInto(notes: store))
        XCTAssertEqual(stored(store, note.id)?.text, "hello world")
        XCTAssertFalse(draft.hasPendingEdits)
    }

    /// Re-setting the same value must not arm the debounce again, or a
    /// re-render of identical text would keep the store dirty forever.
    func testSettingTheSameValueIsNotAnEdit() throws {
        let (_, note) = try fixture()
        var draft = DraftBuffer()
        draft.load(note: note)
        XCTAssertFalse(draft.setText("hello"))
        XCTAssertFalse(draft.setTitle(""))
        XCTAssertFalse(draft.hasPendingEdits)
    }

    func testTitleRoundTrip() throws {
        let (store, note) = try fixture()
        var draft = DraftBuffer()
        draft.load(note: note)

        XCTAssertTrue(draft.setTitle("My title"))
        XCTAssertTrue(draft.flushInto(notes: store))
        XCTAssertEqual(stored(store, note.id)?.explicitTitle, "My title")
    }

    /// An emptied title must fall back to the derived one, matching the
    /// store's contract and the iOS editor's.
    func testEmptyTitleClearsTheExplicitTitle() throws {
        let (store, note) = try fixture(title: "Named")
        var draft = DraftBuffer()
        draft.load(note: note)

        XCTAssertTrue(draft.setTitle("   "))
        XCTAssertTrue(draft.flushInto(notes: store))
        XCTAssertNil(stored(store, note.id)?.explicitTitle)
    }

    /// The debounce and the pane teardown can both fire; the second must not
    /// rewrite, which would move `updatedAt` and re-sort the list.
    func testSecondFlushAfterNoEditsIsANoOp() throws {
        let (store, note) = try fixture()
        var draft = DraftBuffer()
        draft.load(note: note)
        draft.setText("changed")
        XCTAssertTrue(draft.flushInto(notes: store))
        XCTAssertFalse(draft.flushInto(notes: store))
    }

    // MARK: - Ownership

    /// A draft belongs to one note. Loading another re-points it, and the
    /// previous note's text must not follow it.
    func testLoadingAnotherNoteResetsTheDraft() throws {
        let (store, first) = try fixture()
        var second = store.createNote()
        second.text = "second"
        store.update(second)

        var draft = DraftBuffer()
        draft.load(note: first)
        draft.setText("edited first")
        draft.load(note: store.allNotes.first { $0.id == second.id }!)

        XCTAssertEqual(draft.text, "second")
        XCTAssertEqual(draft.noteId, second.id)
        XCTAssertFalse(draft.hasPendingEdits)
        XCTAssertEqual(stored(store, first.id)?.text, "hello", "the first note was written on switch")
    }

    /// A note deleted while its editor was open must not come back when the
    /// pending flush fires.
    func testFlushDoesNotResurrectADeletedNote() throws {
        let (store, note) = try fixture()
        var draft = DraftBuffer()
        draft.load(note: note)
        draft.setText("edited")

        store.purge(noteId: note.id)
        XCTAssertFalse(draft.flushInto(notes: store))
        XCTAssertNil(stored(store, note.id))
    }

    // MARK: - Debounce

    /// The debounce must actually wait, and must fire exactly once. A timer
    /// that fires per keystroke is indistinguishable, from the store's side,
    /// from the bug this replaced.
    func testDebounceFiresOnceAfterTypingStops() async throws {
        let (store, note) = try fixture()
        var draft = DraftBuffer()
        draft.load(note: note)
        draft.setText("typed at the last moment")

        let fired = expectation(description: "debounce fired")
        let sync = DebouncedDraftSync()
        let holder = FlushCounter(store: store)
        holder.onFlush = { [holder] in
            holder.count += 1
            fired.fulfill()
        }

        sync.schedule(after: .milliseconds(40)) { holder.fire(draft) }
        sync.schedule(after: .milliseconds(40)) { holder.fire(draft) }
        sync.schedule(after: .milliseconds(40)) { holder.fire(draft) }

        await fulfillment(of: [fired], timeout: 5)
        // Give a wrongly-surviving cancelled task time to fire.
        try await Task.sleep(for: .milliseconds(250))
        XCTAssertEqual(holder.count, 1, "the debounce fired more than once")
        XCTAssertEqual(stored(store, note.id)?.text, "typed at the last moment")
    }

    /// Leaving the pane must not leave a pending timer that fires against a
    /// store the pane no longer owns.
    func testCancellingStopsThePendingFlush() async throws {
        let (store, note) = try fixture()
        var draft = DraftBuffer()
        draft.load(note: note)
        draft.setText("never committed")

        let holder = FlushCounter(store: store)
        let sync = DebouncedDraftSync()
        sync.schedule(after: .milliseconds(40)) { holder.fire(draft) }
        sync.cancelPending()

        try await Task.sleep(for: .milliseconds(250))
        XCTAssertEqual(holder.count, 0)
        XCTAssertEqual(stored(store, note.id)?.text, "hello")
        XCTAssertTrue(draft.hasPendingEdits, "the pending edit must survive the cancel")
    }
}

/// A class so the debounce's escaping closure can carry mutable state across
/// the hop — a captured `var` in a `@MainActor` closure would be immutable.
@MainActor
private final class FlushCounter {
    private let store: NotesStore
    private var buffer = DraftBuffer()
    var count = 0
    var onFlush: (() -> Void)?

    init(store: NotesStore) { self.store = store }

    func fire(_ draft: DraftBuffer) {
        buffer = draft
        if buffer.flushInto(notes: store) { onFlush?() }
    }
}
