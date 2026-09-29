import Foundation
import SwiftCrossUI
import Data

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
                TextField("Search notes…", text: $searchText)
                Button("Sort: \(sortLabel)") { cycleSort() }
                    .buttonStyle(.borderless)
                Button("＋ New note") { createNote() }
                    .buttonStyle(.bordered)
            }
            if filtered.isEmpty {
                ContentUnavailableView {
                    Text("No notes")
                } description: {
                    Text(searchText.isEmpty
                        ? "Create your first note with ＋ New note."
                        : "Nothing matches “\(searchText)”.")
                }
            } else {
                List(filtered, selection: $selectedNoteId) { note in
                    row(for: note)
                }
            }
        }
        .padding(8)
    }

    // MARK: - Row

    private func row(for note: Note) -> some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(meta(for: note).title)
                    .font(.system(size: 14, weight: note.isPinned ? .medium : .regular))
                Text(caption(for: note))
                    .font(.footnote)
                    .foregroundColor(.gray)
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
