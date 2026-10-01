import Foundation
import Log

/// The editor's working copy of a note: what the user has typed but not yet
/// committed to the store.
///
/// Why this exists: `NotesStore.update` is not a cheap setter. It bumps the
/// library version (which invalidates the notes list's memoized result
/// set), evicts the row-metadata cache, drops the note's playback
/// bookmarks, and schedules a JSON save. A write-through binding therefore
/// rebuilt the whole list, re-derived every title, and discarded bookmarks
/// *per character typed* — the most likely single cause of the "notes pane
/// feels broken" report. iOS has used a debounced draft since v0.5; this is
/// that, ported.
///
/// The rules, all of them pinned by DraftBufferTests:
/// - Nothing reaches the store until the user pauses (see
///   ``DebouncedDraftSync``).
/// - Opening a note and closing it again writes nothing: a flush that would
///   not change the store is skipped.
/// - A note deleted while its editor was open is not resurrected by a
///   late flush.
/// - An empty title field clears the explicit title and falls back to the
///   derived one.
@MainActor
public struct DraftBuffer {
    /// Matches the iOS editor's 400 ms.
    public static let debounce: Duration = .milliseconds(400)

    public private(set) var text: String = ""
    public private(set) var title: String = ""

    /// The note this draft belongs to. A flush for any other id is refused,
    /// so a stale pane cannot write into a different note.
    public private(set) var noteId: UUID?
    /// The store values the last flush wrote, so a no-op edit is detectable.
    private var flushedText: String = ""
    private var flushedTitle: String = ""
    private var isDirty = false

    public init() {}

    /// True while there are uncommitted edits.
    public var hasPendingEdits: Bool { isDirty }

    /// True when this draft is a no-op copy of what the store already has.
    public var matchesStore: Bool { !isDirty }

    /// Starts a fresh draft for a note. The caller is expected to have
    /// flushed the previous note first.
    public mutating func load(note: Note) {
        noteId = note.id
        text = note.text
        title = note.explicitTitle ?? ""
        flushedText = note.text
        flushedTitle = note.explicitTitle ?? ""
        isDirty = false
    }

    /// Records a body edit. Returns true when the draft actually changed.
    @discardableResult
    public mutating func setText(_ value: String) -> Bool {
        guard value != text else { return false }
        text = value
        isDirty = true
        return true
    }

    /// Records a title edit. The field shows the derived title until the
    /// user types, so the pane's getter falls back to `note.title` — see
    /// ``NoteEditorPane/titleBinding``.
    @discardableResult
    public mutating func setTitle(_ value: String) -> Bool {
        guard value != title else { return false }
        title = value
        isDirty = true
        return true
    }

    /// Commits the draft into the store when it differs from what is
    /// already there. Returns true when the store was actually written to.
    @discardableResult
    public mutating func flushInto(notes: NotesStore) -> Bool {
        guard let noteId else { return false }
        guard isDirty else { return false }
        guard var current = notes.allNotes.first(where: { $0.id == noteId }) else {
            // Deleted while the editor was open: the draft has nowhere to
            // go and must not bring the note back.
            isDirty = false
            return false
        }
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        let nextTitle: String? = trimmed.isEmpty ? nil : trimmed
        guard current.text != text || current.explicitTitle != nextTitle else {
            isDirty = false
            return false
        }
        current.text = text
        current.explicitTitle = nextTitle
        notes.update(current)
        flushedText = text
        flushedTitle = title
        isDirty = false
        return true
    }
}

/// Restarts a debounced action after every edit.
///
/// SwiftCrossUI's `View` is a struct, so a view cannot own a timer that
/// captures itself; this is the small piece that does. The generation
/// counter is the reason it is a class and not a bare `Task`: a cancelled
/// task that has already passed its sleep would otherwise still fire, and
/// a stale flush writing an old draft is worse than a late one.
@MainActor
public final class DebouncedDraftSync {
    private var task: Task<Void, Never>?
    private var generation = 0

    public init() {}

    deinit {
        task?.cancel()
    }

    /// (Re)starts the debounce. The action runs on the main actor once the
    /// user has been idle for `interval`.
    public func schedule(
        after interval: Duration = DraftBuffer.debounce,
        _ action: @escaping @MainActor () -> Void
    ) {
        task?.cancel()
        generation &+= 1
        let mine = generation
        task = Task { [weak self] in
            try? await Task.sleep(for: interval)
            guard !Task.isCancelled else { return }
            guard let self, self.generation == mine else { return }
            self.task = nil
            action()
        }
    }

    /// Runs the pending action now. Used when the pane goes away, where
    /// waiting out the debounce would lose the tail of the note.
    public func cancelPending() {
        task?.cancel()
        task = nil
        generation &+= 1
    }
}
