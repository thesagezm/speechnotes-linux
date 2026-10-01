import Foundation
import SwiftCrossUI
import Data
import Appearance

/// The notes list: scope + search + sort + pin/star, feeding the editor
/// column. Ported from speechnotes-ios NotesListView; swipe actions and the
/// import/share dialogs are not portable to SwiftCrossUI 0.9 and land later
/// as toolbar menus.
///
/// Performance note — why the result set is memoized. SwiftCrossUI lays out
/// the whole tree on every update, and this pane lives inside the shell, so
/// its body re-runs on every keystroke and on every playback tick. The
/// obvious implementation re-walks the library and re-sorts it each time.
/// That work now lives in ``NotesQuery`` (tested headlessly) and the result
/// is cached against the store's version counter plus the three inputs that
/// actually change it, so it is rebuilt only when one of them moves.
struct NotesListPane: View {
    let notes: NotesStore
    let notebooks: NotebooksStore
    let prefs: Prefs
    @Binding var selectedNoteId: UUID?
    @Binding var searchText: String
    /// The shell's focus-search counter, bumped by the Ctrl+F shortcut.
    /// SwiftCrossUI has no focus API, so the pane can only clear the box;
    /// the window-level key handler does the rest.
    var focusSearchRequest: Int = 0

    @State private var theme = ThemeController.shared
    @State private var derived = Derived()

    /// The memoized result set plus the inputs it was derived from.
    struct Derived {
        var rows: [Note] = []
        private var libraryVersion = -1
        private var scope = ""
        private var query = ""
        private var sort: NotesQuery.Sort?
        private var notebookCount = -1

        mutating func refresh(
            notes: [Note],
            scope: String,
            query: String,
            sort: NotesQuery.Sort,
            libraryVersion: Int,
            notebookCount: Int
        ) {
            guard rows.isEmpty
                || libraryVersion != self.libraryVersion
                || scope != self.scope
                || query != self.query
                || sort != self.sort
                || notebookCount != self.notebookCount
            else { return }
            self.libraryVersion = libraryVersion
            self.scope = scope
            self.query = query
            self.sort = sort
            self.notebookCount = notebookCount
            // A scope that is neither "all", "unfiled" nor a UUID can only
            // come from a stale preference. Fall back to the whole library
            // rather than an empty list the user cannot explain.
            let (id, all) = NotesQuery.notebookId(forScope: scope) ?? (nil, true)
            rows = NotesQuery.apply(
                notes, notebookId: id, allNotebooks: all, query: query, sort: sort
            )
        }
    }

    private var sortOrder: NotesQuery.Sort {
        NotesQuery.Sort(rawValue: prefs.notesSortOrder) ?? .edited
    }

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                // The search entry is the Ctrl+F target. SwiftCrossUI has
                // no focus API, so focus is grabbed straight on the GTK
                // entry (`.inspect` hands over the live widget) — that is
                // the only way a keyboard-driven user can start typing a
                // search without clicking first.
                TextField("Search notes…", text: searchField)
                    .inspect { entry in
                        SearchFieldFocus.attach(to: entry, request: focusSearchRequest)
                    }
                Button("Sort: \(sortLabel)") { cycleSort() }
                    .buttonStyle(.borderless)
                Button("＋ New note") { createNote() }
                    .buttonStyle(.bordered)
            }
            if KeyboardHelp.hintsVisible {
                Text(KeyboardHelp.hintLine)
                    .font(.caption2)
                    .foregroundColor(theme.text)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if derived.rows.isEmpty {
                ContentUnavailableView {
                    Text("No notes")
                } description: {
                    Text(searchText.isEmpty
                        ? "Create your first note with ＋ New note."
                        : "Nothing matches “\(searchText)”.")
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(derived.rows, selection: $selectedNoteId) { note in
                    row(for: note)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .onAppear(perform: refreshDerived)
        .onChange(of: prefs.activeNotebookScope) { refreshDerived() }
        .onChange(of: prefs.notesSortOrder) { refreshDerived() }
        .onChange(of: searchText) { refreshDerived() }
        .onChange(of: focusSearchRequest) { SearchFieldFocus.request(focusSearchRequest) }
        // The store's version counter is the cheapest signal that a note was
        // created, edited, pinned, starred, moved or deleted.
        .onChange(of: notes.version) { refreshDerived() }
    }

    private func refreshDerived() {
        derived.refresh(
            notes: notes.notes,
            scope: prefs.activeNotebookScope,
            query: searchText,
            sort: sortOrder,
            libraryVersion: notes.version,
            notebookCount: notebooks.notebooks.count
        )
    }

    // MARK: - Row

    private func row(for note: Note) -> some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(meta(for: note).title)
                    .font(.system(size: 14, weight: note.isPinned ? .medium : .regular))
                    .lineLimit(1)
                Text(caption(for: note))
                    .font(.footnote)
                    .foregroundColor(theme.text)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
            Button(note.isPinned ? "📌" : "○") {
                notes.setPinned(!note.isPinned, noteId: note.id)
            }
            .buttonStyle(.borderless)
            Button(note.isFavorite ? "★" : "☆") {
                notes.setFavorite(!note.isFavorite, noteId: note.id)
            }
            .buttonStyle(.borderless)
        }
        .padding(.vertical, 2)
    }

    private func caption(for note: Note) -> String {
        let meta = meta(for: note)
        var parts: [String] = []
        if let notebookName = notebooks.name(for: note.notebookId) {
            parts.append(notebookName)
        }
        parts.append("\(meta.wordCount) words")
        if !meta.preview.isEmpty {
            parts.append(meta.preview)
        }
        return parts.joined(separator: " · ")
    }

    private func meta(for note: Note) -> NotesStore.RowMetadata {
        notes.metadata(for: note)
    }

    private var sortLabel: String {
        switch sortOrder {
        case .edited: return "edited"
        case .created: return "created"
        case .title: return "title"
        }
    }

    private func cycleSort() {
        let all = NotesQuery.Sort.allCases
        let next = all[(all.firstIndex(of: sortOrder).map { $0 + 1 } ?? 0) % all.count]
        prefs.notesSortOrder = next.rawValue
    }

    private var searchField: Binding<String> {
        Binding(
            get: { searchText },
            set: { searchText = $0 }
        )
    }

    private func createNote() {
        let scope = prefs.activeNotebookScope
        let notebookId = (scope != "all" && scope != "unfiled")
            ? UUID(uuidString: scope)
            : nil
        let note = notes.createNote(notebookId: notebookId)
        selectedNoteId = note.id
        searchText = ""
    }
}
