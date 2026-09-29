import Foundation
import SwiftCrossUI
import Data
import TTSEngine

/// Three-column shell: destinations and notebook scopes on the left, the
/// filtered note list in the middle, the editor on the right. The iOS app
/// used a TabView; NavigationSplitView is the desktop equivalent — and the
/// layout dsnote's GTK reader already proved familiar.
struct AppShell: View {
    @State private var notes = NotesStore.shared
    @State private var notebooks = NotebooksStore.shared
    @State private var prefs = Prefs.shared
    @State private var tts = TTSController.shared
    @State private var books = BooksStore.shared

    @State private var pane: Pane = .notes
    @State private var selectedNoteId: UUID?
    @State private var selectedBookId: UUID?
    @State private var searchText = ""
    @State private var newNotebookName = ""
    @State private var resumeCandidate: ResumeCandidate?

    /// The one auto-resume offer per launch: the most recent note bookmark
    /// saved within the last few hours whose note still exists.
    private struct ResumeCandidate: Equatable {
        let note: Note
        let offset: Int
    }

    enum Pane: Equatable {
        case notes
        case books
        case recycleBin
        case settings
    }

    init() {
        if let (key, mark) = BookmarkStore.shared.mostRecentNoteBookmark(within: 6 * 3600),
           key.hasPrefix("note:"),
           let id = UUID(uuidString: String(key.dropFirst("note:".count))),
           let note = NotesStore.shared.notes.first(where: { $0.id == id }) {
            _resumeCandidate = State(wrappedValue: ResumeCandidate(note: note, offset: mark.textOffset))
        }
    }

    var body: some View {
        NavigationSplitView(
            sidebar: { sidebar },
            content: { middleColumn },
            detail: { detailColumn }
        )
    }

    // MARK: - Sidebar

    @ViewBuilder
    private var sidebar: some View {
        VStack(spacing: 4) {
            sidebarButton(
                title: "All notes",
                isActive: pane == .notes && prefs.activeNotebookScope == "all"
            ) {
                pane = .notes
                prefs.activeNotebookScope = "all"
            }
            sidebarButton(
                title: "Unfiled",
                isActive: pane == .notes && prefs.activeNotebookScope == "unfiled"
            ) {
                pane = .notes
                prefs.activeNotebookScope = "unfiled"
            }
            ForEach(notebooks.notebooks) { notebook in
                sidebarButton(
                    title: notebook.name,
                    isActive: pane == .notes && prefs.activeNotebookScope == notebook.id.uuidString
                ) {
                    pane = .notes
                    prefs.activeNotebookScope = notebook.id.uuidString
                }
            }
            Divider()
            sidebarButton(title: "Books", isActive: pane == .books) {
                pane = .books
                books.refresh()
            }
            sidebarButton(title: "Recycle Bin", isActive: pane == .recycleBin) {
                pane = .recycleBin
            }
            sidebarButton(title: "Settings", isActive: pane == .settings) {
                pane = .settings
            }
            Spacer()
            HStack(spacing: 8) {
                TextField("New notebook…", text: $newNotebookName)
                Button("Add") {
                    if notebooks.create(name: newNotebookName) != nil {
                        newNotebookName = ""
                    }
                }
            }
        }
        .padding(12)
    }

    private func sidebarButton(
        title: String,
        isActive: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(isActive ? "●  \(title)" : "○  \(title)") {
            action()
        }
    }

    // MARK: - Columns

    @ViewBuilder
    private var middleColumn: some View {
        if pane == .notes {
            VStack(spacing: 0) {
                if let candidate = resumeCandidate {
                    HStack(spacing: 8) {
                        Text("Resume “\(candidate.note.title)”?")
                        Spacer()
                        Button("Resume") {
                            selectedNoteId = candidate.note.id
                            tts.play(
                                note: candidate.note,
                                engineKind: EngineKind(rawValue: prefs.engineKind) ?? .espeak,
                                speed: Float(prefs.rateMultiplier),
                                voice: prefs.voice,
                                resumeFromUTF16: candidate.offset
                            )
                            resumeCandidate = nil
                        }
                        Button("Dismiss") { resumeCandidate = nil }
                    }
                    .padding(8)
                }
                NotesListPane(
                    notes: notes,
                    notebooks: notebooks,
                    prefs: prefs,
                    selectedNoteId: $selectedNoteId,
                    searchText: $searchText
                )
            }
        } else if pane == .books {
            BooksPane(books: books) { book in
                selectedBookId = book.id
            }
        } else if pane == .recycleBin {
            RecycleBinPane(notes: notes)
        } else {
            SettingsPane(prefs: prefs, notes: notes, notebooks: notebooks)
        }
    }

    @ViewBuilder
    private var detailColumn: some View {
        if pane == .notes {
            if let id = selectedNoteId,
               let note = notes.allNotes.first(where: { $0.id == id }) {
                NoteEditorPane(note: note, notes: notes)
            } else {
                placeholder(title: "No note selected", detail: "Create or pick a note in the list.")
            }
        } else if pane == .books {
            if let id = selectedBookId,
               let book = books.allBooks.first(where: { $0.id == id }) {
                BookReaderPane(book: book, books: books)
            } else {
                placeholder(title: "No book selected", detail: "Import or pick a book in the shelf.")
            }
        } else if pane == .recycleBin {
            placeholder(
                title: "Recycle Bin",
                detail: "Deleted notes stay here for \(Note.recycleRetentionDays) days before they are purged."
            )
        } else {
            placeholder(title: "Settings", detail: "Preferences are on the left.")
        }
    }

    private func placeholder(title: String, detail: String) -> some View {
        ContentUnavailableView {
            Text(title)
        } description: {
            Text(detail)
        }
    }
}
