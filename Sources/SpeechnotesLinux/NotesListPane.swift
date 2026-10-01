import Foundation
import SwiftCrossUI
import Data
import Appearance

/// The notes list: scope + search + sort + pin/star, feeding the editor
/// column. Ported from speechnotes-ios NotesListView; swipe actions and the
/// import/share dialogs are not portable to SwiftCrossUI 0.9 and land later
/// as toolbar menus.
struct NotesListPane: View {
    let notes: NotesStore
    let notebooks: NotebooksStore
    let prefs: Prefs
    @Binding var selectedNoteId: UUID?
    @Binding var searchText: String
    /// The shell's focus-search counter. Non-zero means "put the caret in
    /// the search field" — the same flag iOS's searchable list uses. Swift
    /// CrossUI has no focus API, so the pane can only pre-fill and select
    /// the text; typing then lands in the field because GTK gives a
    /// newly-selected entry the keyboard grab on the next click, and the
    /// shortcut is also wired to focus via the window-level handler.
    var focusSearchRequest: Int = 0

    @State private var theme = ThemeController.shared

    private enum SortOrder: String {
        case edited
        case created
        case title
    }

    private var sortOrder: SortOrder {
        SortOrder(rawValue: prefs.notesSortOrder) ?? .edited
    }

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                TextField("Search notes…", text: searchField)
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
            if filtered.isEmpty {
                ContentUnavailableView {
                    Text("No notes")
                } description: {
                    Text(searchText.isEmpty
                        ? "Create your first note with ＋ New note."
                        : "Nothing matches “\(searchText)”.")
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(filtered, selection: $selectedNoteId) { note in
                    row(for: note)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
    }

    // MARK: - Row

    private func row(for note: Note) -> some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(meta(for: note).title)
                    .font(.system(size: 14, weight: note.isPinned ? .medium : .regular))
                Text(caption(for: note))
                    .font(.footnote)
                    .foregroundColor(theme.text)
            }
            Spacer()
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
        parts.append("\(note.wordCount) words")
        if !meta.preview.isEmpty {
            parts.append(meta.preview)
        }
        return parts.joined(separator: " · ")
    }

    private func meta(for note: Note) -> NotesStore.RowMetadata {
        notes.metadata(for: note)
    }

    // MARK: - Filtering + sorting

    private var scoped: [Note] {
        switch prefs.activeNotebookScope {
        case "all":
            return notes.notes
        case "unfiled":
            return notes.notes(inNotebook: nil)
        default:
            return UUID(uuidString: prefs.activeNotebookScope)
                .map { notes.notes(inNotebook: $0) } ?? notes.notes
        }
    }

    private var filtered: [Note] {
        var result = scoped
        if !searchText.isEmpty {
            result = result.filter {
                $0.text.range(of: searchText, options: .caseInsensitive) != nil
            }
        }
        result.sort { lhs, rhs in
            if lhs.isPinned != rhs.isPinned { return lhs.isPinned }
            switch sortOrder {
            case .edited: return lhs.updatedAt > rhs.updatedAt
            case .created: return lhs.createdAt > rhs.createdAt
            case .title: return meta(for: lhs).title.localizedCompare(meta(for: rhs).title) == .orderedAscending
            }
        }
        return result
    }

    private var sortLabel: String {
        switch sortOrder {
        case .edited: return "edited"
        case .created: return "created"
        case .title: return "title"
        }
    }

    private func cycleSort() {
        switch sortOrder {
        case .edited: prefs.notesSortOrder = SortOrder.created.rawValue
        case .created: prefs.notesSortOrder = SortOrder.title.rawValue
        case .title: prefs.notesSortOrder = SortOrder.edited.rawValue
        }
    }

    /// The search field's binding, plus the Ctrl+F behaviour: a non-zero
    /// request counter clears the box and marks the text for selection, so
    /// the next keystroke replaces the query instead of appending to it.
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
