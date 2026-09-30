import Foundation
import SwiftCrossUI
import Data
import TTSEngine
import Appearance

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
    @State private var audio = AudioBookController.shared
    @State private var theme = ThemeController.shared

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
        case about
        case logs
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
        // Idempotent theme sync; re-runs whenever an appearance pref changes.
        let _ = theme.sync(appearance: prefs.appearance, accentChoice: prefs.accentChoice)
        VStack(spacing: 0) {
            NavigationSplitView(
                sidebar: { sidebar },
                content: { middleColumn },
                detail: { detailColumn }
            )
            // The persistent transport: survives navigation, like the iOS
            // mini-player. Sits under the split view so it never covers
            // content.
            if let playback = activePlayback {
                Divider()
                MiniPlayerBar(playback: playback)
            }
        }
        .colorScheme(theme.effectiveScheme)
    }

    /// The active run, resolved for the mini-player: audiobook first (it and
    /// TTS are mutually exclusive by controller contract), then TTS — a note
    /// id resolves through NotesStore, anything else through the shelf
    /// (book TTS runs under the book's id).
    private var activePlayback: ActivePlayback? {
        if let pos = audio.position, audio.isBusy,
           let book = books.allBooks.first(where: { $0.id == pos.bookId }) {
            return .audiobook(
                book: book,
                chapterIndex: pos.chapterIndex,
                seconds: pos.seconds,
                fraction: pos.fraction,
                paused: audio.state == .paused
            )
        }
        if let pos = tts.position, tts.isBusy {
            if let note = notes.allNotes.first(where: { $0.id == pos.noteId }) {
                return .noteTTS(note: note, fraction: pos.fraction, paused: tts.state == .paused)
            }
            if let book = books.allBooks.first(where: { $0.id == pos.noteId }) {
                return .bookTTS(book: book, fraction: pos.fraction, paused: tts.state == .paused)
            }
        }
        return nil
    }

    // MARK: - Sidebar

    @ViewBuilder
    private var sidebar: some View {
        VStack(spacing: 2) {
            HStack(spacing: 8) {
                Text("Speechnotes")
                    .font(.title2.weight(.semibold))
                Spacer()
            }
            .padding(.bottom, 12)

            sectionLabel("Library")
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
            sectionLabel("More")
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
            sidebarButton(title: "About", isActive: pane == .about) {
                pane = .about
            }
            sidebarButton(title: "Logs", isActive: pane == .logs) {
                pane = .logs
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

    private func sectionLabel(_ title: String) -> some View {
        Text(title.uppercased())
            .font(.caption.weight(.medium))
            .foregroundColor(theme.text)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.top, 8)
            .padding(.bottom, 4)
    }

    private func sidebarButton(
        title: String,
        isActive: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button {
            action()
        } label: {
            HStack(spacing: 8) {
                if isActive {
                    Text(title)
                        .font(.system(size: 14, weight: .medium))
                        .foregroundColor(theme.accent)
                } else {
                    Text(title)
                        .font(.system(size: 14, weight: .regular))
                }
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(theme.accent.opacity(isActive ? 0.14 : 0))
            .cornerRadius(6)
        }
        .buttonStyle(.borderless)
    }

    // MARK: - Columns

    @ViewBuilder
    private var middleColumn: some View {
        if pane == .notes {
            VStack(spacing: 0) {
                if let candidate = resumeCandidate {
                    HStack(spacing: 8) {
                        Text("Resume “\(candidate.note.title)”?")
                            .font(.callout)
                        Spacer()
                        Button("Resume") {
                            selectedNoteId = candidate.note.id
                            tts.play(
                                note: candidate.note,
                                engineKind: EngineKind(rawValue: prefs.engineKind) ?? .espeak,
                                speed: Float(prefs.rateMultiplier),
                                voice: prefs.voiceForEngine(EngineKind(rawValue: prefs.engineKind) ?? .espeak),
                                resumeFromUTF16: candidate.offset
                            )
                            resumeCandidate = nil
                        }
                        .buttonStyle(.bordered)
                        Button("Dismiss") { resumeCandidate = nil }
                            .buttonStyle(.borderless)
                    }
                    .padding(10)
                    .background(theme.accent.opacity(0.08))
                    .cornerRadius(6)
                    .padding(.horizontal, 8)
                    .padding(.top, 8)
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
        } else if pane == .about {
            AboutPane()
        } else if pane == .logs {
            LogsPane()
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
        } else if pane == .about || pane == .logs {
            placeholder(
                title: pane == .about ? "About" : "Logs",
                detail: "Shown in the middle column."
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
