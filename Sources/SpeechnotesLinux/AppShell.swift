import Foundation
import Log
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
    @State private var books = BooksStore.shared
    @State private var theme = ThemeController.shared
    @State private var presence = PlaybackPresence.shared

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
        let t0 = DispatchTime.now()
        let content = mainBody
        RenderProbe.recordBodyEval(
            pane: String(describing: pane),
            durationMs: Double(DispatchTime.now().uptimeNanoseconds - t0.uptimeNanoseconds) / 1e6
        )
        return content
    }

    private var mainBody: some View {
        // Idempotent theme sync; re-runs whenever an appearance pref changes.
        let _ = theme.sync(appearance: prefs.appearance, accentChoice: prefs.accentChoice)
        return VStack(spacing: 0) {
            NavigationSplitView(
                sidebar: { sidebar },
                content: { middleColumn },
                detail: { detailColumn }
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // The persistent transport: survives navigation, like the iOS
            // mini-player. Sits under the split view so it never covers
            // content.
            if presence.isVisible {
                Divider()
                MiniPlayerBar()
            }
        }
        .colorScheme(theme.effectiveScheme)
        .task { await runBenchIfRequested() }
    }

    /// SPEECHNOTES_BENCH=1 turns this launch into a pane-switch benchmark:
    /// the same transitions a user clicks, timed end to end, then exit(0).
    private func runBenchIfRequested() async {
        guard RenderProbe.benchRequested else { return }
        RenderProbe.benchRequested = false
        try? await Task.sleep(nanoseconds: 2_000_000_000)
        Log.info("BENCH: starting pane-switch benchmark")

        await benchSwitch("notes→settings") { pane = .settings }
        await benchSwitch("settings→notes") { pane = .notes }
        await benchSwitch("notes→books (tab click)") {
            pane = .books
            books.refresh()
        }
        await benchSwitch("books→settings") { pane = .settings }
        await benchSwitch("settings→notes (2nd)") { pane = .notes }
        if let first = books.books.first {
            await benchSwitch("open book '\(first.title)'") { selectedBookId = first.id }
            await benchSwitch("book→notes") {
                pane = .notes
                selectedBookId = nil
            }
        }
        Log.info("BENCH: complete")
        // The bench instance owns the GTK app id — exit so the real app can
        // run.
        exit(0)
    }

    private func benchSwitch(
        _ label: String,
        _ change: @escaping @MainActor () -> Void
    ) async {
        let start = DispatchTime.now()
        change()
        _ = await RenderProbe.settledLatency()
        let total = Double(DispatchTime.now().uptimeNanoseconds - start.uptimeNanoseconds) / 1e6
        Log.info(String(format: "BENCH: %@ settled in %.0fms", label, total))
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
    //
    // Both columns render through CachedSwitchView: pane switches swap a
    // mounted widget instead of destroying and rebuilding the pane's whole
    // widget tree (SwiftCrossUI's if/else rebuilds — hundreds of ms per
    // click for settings-sized trees). Slot order is load-bearing: each
    // slot must always render the same concrete view type.

    private var middleIndex: Int {
        switch pane {
        case .notes: return 0
        case .books: return 1
        case .recycleBin: return 2
        case .about: return 3
        case .logs: return 4
        case .settings: return 5
        }
    }

    private var detailIndex: Int {
        switch pane {
        case .notes: return 0
        case .books: return 1
        case .recycleBin: return 2
        case .about, .logs: return 3
        case .settings: return 4
        }
    }

    private var middleColumn: some View {
        CachedSwitchView(
            activeIndex: middleIndex,
            branches: [
                AnyView(notesMiddle),
                AnyView(BooksPane(books: books) { book in
                    selectedBookId = book.id
                }),
                AnyView(RecycleBinPane(notes: notes)),
                AnyView(AboutPane()),
                AnyView(LogsPane()),
                AnyView(SettingsPane(prefs: prefs, notes: notes, notebooks: notebooks)),
            ]
        )
    }

    @ViewBuilder
    private var notesMiddle: some View {
        VStack(spacing: 0) {
            if let candidate = resumeCandidate {
                HStack(spacing: 8) {
                    Text("Resume “\(candidate.note.title)”?")
                        .font(.callout)
                    Spacer()
                    Button("Resume") {
                        selectedNoteId = candidate.note.id
                        TTSController.shared.play(
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
    }

    private var detailColumn: some View {
        CachedSwitchView(
            activeIndex: detailIndex,
            branches: [
                AnyView(NotesDetailHost(note: selectedNote, notes: notes)),
                AnyView(BooksDetailHost(book: selectedBook, books: books)),
                AnyView(placeholder(
                    title: "Recycle Bin",
                    detail: "Deleted notes stay here for \(Note.recycleRetentionDays) days before they are purged."
                )),
                AnyView(placeholder(
                    title: "About & Logs",
                    detail: "Shown in the middle column."
                )),
                AnyView(placeholder(title: "Settings", detail: "Preferences are on the left.")),
            ]
        )
    }

    private var selectedNote: Note? {
        guard pane == .notes, let id = selectedNoteId else { return nil }
        return notes.allNotes.first(where: { $0.id == id })
    }

    private var selectedBook: Book? {
        guard pane == .books, let id = selectedBookId else { return nil }
        return books.allBooks.first(where: { $0.id == id })
    }

    private func placeholder(title: String, detail: String) -> some View {
        ContentUnavailableView {
            Text(title)
        } description: {
            Text(detail)
        }
    }
}

/// The notes detail slot: editor when a note is selected, placeholder
/// otherwise. Its concrete type must stay stable across renders — it is a
/// fixed CachedSwitchView slot.
struct NotesDetailHost: View {
    let note: Note?
    let notes: NotesStore

    var body: some View {
        if let note {
            NoteEditorPane(note: note, notes: notes)
        } else {
            ContentUnavailableView {
                Text("No note selected")
            } description: {
                Text("Create or pick a note in the list.")
            }
        }
    }
}

/// The books detail slot (same contract as NotesDetailHost).
struct BooksDetailHost: View {
    let book: Book?
    let books: BooksStore

    var body: some View {
        if let book {
            BookReaderPane(book: book, books: books)
        } else {
            ContentUnavailableView {
                Text("No book selected")
            } description: {
                Text("Import or pick a book in the shelf.")
            }
        }
    }
}
