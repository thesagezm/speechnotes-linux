import CGTK4
import Foundation
import SwiftCrossUI
import Shortcuts
import Data
import TTSEngine
import Log

/// What a shortcut does, and where the app is right now.
///
/// The shell owns the selection and the current pane, so it — not the
/// keyboard layer — is what knows how to act. ``ShortcutHub`` turns a
/// keypress into a ``Shortcuts/Command`` and hands it here; every action is
/// the same code the equivalent button runs, which is why a shortcut can
/// never drift from the control printed beside it.
///
/// Deliberately a singleton with one context slot: there is exactly one
/// shell, and keeping the reference here is cheaper than threading a closure
/// through the GTK window-lifecycle hook, which is a C callback.
@MainActor
final class AppCommands {
    static let shared = AppCommands()

    /// Everything the actions need from the live shell.
    struct Context {
        var pane: AppShell.Pane = .notes
        var selectedNoteId: UUID?
        var selectedBookId: UUID?
        var searchText: String = ""
        var newNotebookName: String = ""
    }

    /// Escape hatches the shell installs for commands that need it.
    struct Hooks {
        var focusSearch: () -> Void = {}
        var renderSelectedToWav: () -> Void = {}
    }
    var hooks = Hooks()

    /// The shell's live state, replaced wholesale on every render so a
    /// shortcut always acts on what the user can see.
    private(set) var context = Context()

    private init() {}

    /// Called by the shell on every body evaluation.
    func publish(_ context: Context) {
        self.context = context
    }

    // MARK: - Dispatch

    func perform(_ command: Shortcuts.Command) {
        switch command {
        case .newNote: newNote()
        case .newNotebook: newNotebook()
        case .focusSearch: hooks.focusSearch()
        case .togglePin: togglePin()
        case .toggleStar: toggleStar()
        case .deleteNote: deleteNote()
        case .openNotes: context.pane = .notes
        case .openBooks: context.pane = .books
        case .openRecycleBin: context.pane = .recycleBin
        case .openSettings: context.pane = .settings
        case .showAbout: context.pane = .about
        case .showLogs: context.pane = .logs
        case .showShortcuts: context.pane = .shortcuts
        case .closeWindow: closeWindow()
        case .toggleSpeech: toggleSpeech()
        case .stopSpeech: TTSController.shared.stop()
        case .renderSelectedToWav: hooks.renderSelectedToWav()
        }
    }

    // MARK: - Actions

    private func newNote() {
        let prefs = Prefs.shared
        let scope = prefs.activeNotebookScope
        let notebookId = (scope != "all" && scope != "unfiled")
            ? UUID(uuidString: scope) : nil
        let note = NotesStore.shared.createNote(notebookId: notebookId)
        context.selectedNoteId = note.id
        context.searchText = ""
        context.pane = .notes
    }

    private func newNotebook() {
        let name = context.newNotebookName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return }
        if NotebooksStore.shared.create(name: name) != nil {
            context.newNotebookName = ""
        }
    }

    private func togglePin() {
        guard let (id, note) = selectedNote else { return }
        NotesStore.shared.setPinned(!note.isPinned, noteId: id)
    }

    private func toggleStar() {
        guard let (id, note) = selectedNote else { return }
        NotesStore.shared.setFavorite(!note.isFavorite, noteId: id)
    }

    private func deleteNote() {
        guard let (id, _) = selectedNote else { return }
        if TTSController.shared.isPlaying(noteId: id) {
            TTSController.shared.stop()
        }
        NotesStore.shared.delete(noteId: id)
    }

    private func toggleSpeech() {
        let tts = TTSController.shared
        guard let (_, note) = selectedNote else { return }
        if tts.isPlaying(noteId: note.id) {
            if tts.state == .paused { tts.resume() } else { tts.pause() }
            return
        }
        let prefs = Prefs.shared
        let kind = EngineKind(rawValue: prefs.engineKind) ?? .espeak
        tts.play(
            note: note,
            engineKind: kind,
            speed: Float(prefs.rateMultiplier),
            voice: prefs.voiceForEngine(kind)
        )
    }

    /// A bin/restore pair would race if the note vanished between the
    /// shortcut and the read, so the lookup is done once, here.
    private var selectedNote: (UUID, Note)? {
        guard let id = context.selectedNoteId else { return nil }
        guard let note = NotesStore.shared.allNotes.first(where: { $0.id == id }) else { return nil }
        return (id, note)
    }

    private func closeWindow() {
        if let app = g_application_get_default() {
            g_application_quit(app)
        }
    }
}
