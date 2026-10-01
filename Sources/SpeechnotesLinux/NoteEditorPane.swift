import Foundation
import SwiftCrossUI
import Data
import AppPaths
import TTSEngine
import SpeechLogic
import Appearance

/// The note editor: optional explicit title, the body TextEditor, the
/// pin/star/delete controls, and the speak bar driving the TTS tier.
/// SwiftCrossUI's TextEditor is a plain Binding<String> (no attributed
/// ranges), so formatting flows into the text at the end — same constraint
/// the plan recorded for dsnote parity.
///
/// Two behaviours here are load-bearing, and both were wrong before:
///
/// - **Writes are debounced, not per keystroke.** `NotesStore.update`
///   bumps the library version, evicts the row-metadata cache and
///   re-sorts the whole list, so a write-through binding rebuilt the
///   entire notes list on every character typed. iOS debounces at 400 ms
///   and flushes on disappear; this pane does the same, via
///   ``DraftBuffer``.
/// - **Delete asks first.** The button used to soft-delete immediately,
///   and the recycle bin is a 30-day wait.
struct NoteEditorPane: View {
    let note: Note
    let notes: NotesStore
    /// Bumped by the Ctrl+Shift+Enter shortcut. The pane renders the WAV
    /// itself, so the request arrives as a counter rather than a callback:
    /// a closure held in @State would be re-created every render and the
    /// "did it change" test would always fire.
    var wavRenderRequest: Int = 0

    @State private var tts = TTSController.shared
    @State private var prefs = Prefs.shared
    @State private var theme = ThemeController.shared
    @State private var exportStatus: String?
    @State private var confirmingDelete = false
    /// Which surface the editor is showing. Preview is the default, as on
    /// iOS: a note is something you read back as often as you write.
    @State private var previewMode = true
    @State private var readAlongEnabled = true
    /// The request counter value the current run started for, so a stale
    /// re-render cannot restart a render that already finished.
    @State private var lastHandledRenderRequest = -1
    /// The in-progress text and title (DraftBuffer, in Data), plus the
    /// timer that pushes them into the store. A View is a struct, so the
    /// timer cannot be a captured `self`.
    @State private var draft = DraftBuffer()
    @State private var sync = DebouncedDraftSync()

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                Button(note.isPinned ? "Unpin" : "Pin") {
                    notes.setPinned(!note.isPinned, noteId: note.id)
                }
                .foregroundColor(note.isPinned ? theme.accent : .gray)
                Button(note.isFavorite ? "Unstar" : "Star") {
                    notes.setFavorite(!note.isFavorite, noteId: note.id)
                }
                .foregroundColor(note.isFavorite ? theme.accent : .gray)
                Spacer()
                Text(progressText)
                    .font(.footnote)
                    .foregroundColor(theme.text)
            }
            HStack(spacing: 8) {
                if tts.isPlaying(noteId: note.id) {
                    if tts.state == .paused {
                        Button("▶ Resume") { tts.resume() }
                            .buttonStyle(.bordered)
                    } else {
                        Button("❙❙ Pause") { tts.pause() }
                            .buttonStyle(.bordered)
                    }
                    Button("■ Stop") { tts.stop() }
                        .buttonStyle(.bordered)
                } else {
                    Button("▶ Speak") { speak() }
                        .buttonStyle(.bordered)
                        .foregroundColor(theme.accent)
                }
                Divider()
                Button("Delete") { confirmingDelete = true }
                    .buttonStyle(.borderless)
                Button("⤓ WAV") { renderNow() }
                    .buttonStyle(.borderless)
                Spacer()
            }
            if let status = exportStatus {
                Text(status)
                    .font(.footnote)
                    .foregroundColor(theme.text)
            }
            if tts.isPlaying(noteId: note.id), let sentence = tts.currentSentence {
                Text("▸ \(sentence)")
                    .font(.callout)
                    .foregroundColor(theme.accent)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
                    .background(theme.accent.opacity(0.08))
                    .cornerRadius(6)
            }
            TextField("Title (optional)", text: titleBinding)
                .font(.title2.weight(.semibold))
            if prefs.renderMarkdown {
                // iOS's three-surface editor: preview (rendered markdown),
                // read-along, and raw editing. Only the surface switch is
                // ported here — the formatting bar and slash menu need the
                // caret, which SwiftCrossUI does not expose.
                HStack(spacing: 8) {
                    Picker(
                        of: ["Edit", "Preview"],
                        selection: surfaceSelection
                    )
                    .pickerStyle(.segmented)
                    Spacer()
                    Toggle("Read along", isOn: readAlongBinding)
                        .toggleStyle(.switch)
                }
            }
            if prefs.renderMarkdown, previewMode {
                MarkdownPreview(text: draft.text, textScale: prefs.readerTextScale)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if prefs.renderMarkdown && readAlongEnabled && tts.isPlaying(noteId: note.id) {
                ReadAlongStrip(text: draft.text, scale: prefs.readerTextScale)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                TextEditor(text: textBinding)
                    .font(.system(size: editorFontSize))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .task(id: note.id) {
            // Seed the draft from the note this pane was built for, and
            // speak the draft rather than the store copy so a run started
            // mid-typing hears the last few seconds of edits too.
            draft.load(note: note)
            readAlongEnabled = prefs.readAlongEnabled
            // The note the pane was built for changed under us (the user
            // picked another note); the next render hands over a new pane.
            guard lastHandledRenderRequest != wavRenderRequest else { return }
            lastHandledRenderRequest = wavRenderRequest
            guard wavRenderRequest > 0 else { return }
            await renderWav()
        }
        .onDisappear {
            // The debounce can still be pending here; without this flush a
            // note closed within 400 ms of the last keystroke loses it.
            sync.cancelPending()
            flushDraft()
        }
        // SwiftCrossUI's alert carries a title and plain labelled buttons —
        // no message body, no roles — so the retention window is spelled
        // into the action label rather than left unsaid.
        .alert(
            "Move this note to the Recycle Bin? It stays for \(Note.recycleRetentionDays) days.",
            isPresented: $confirmingDelete
        ) {
            Button("Move to bin") {
                sync.cancelPending()
                flushDraft()
                tts.stop()
                notes.delete(noteId: note.id)
            }
            Button("Cancel") {}
        }
    }

    /// Commits the draft. Split out because three call sites need it (the
    /// debounce fire, pane teardown, and the delete confirmation) and each
    /// needs the same "mutate the @State copy, write it back" dance.
    private func flushDraft() {
        var buffer = draft
        buffer.flushInto(notes: notes)
        draft = buffer
    }

    /// Speaks the current draft.
    ///
    /// When markdown rendering is on and the note is being read in preview
    /// mode, the engine is handed the syntax-stripped text — hearing
    /// "hashtag heading" aloud is the symptom this avoids. This mirrors
    /// iOS's `rendersForSpeech`.
    private func speak() {
        let kind = EngineKind(rawValue: prefs.engineKind) ?? .espeak
        tts.playText(
            id: note.id,
            text: speechText,
            engineKind: kind,
            speed: Float(prefs.rateMultiplier),
            voice: prefs.voiceForEngine(kind),
            bookmarkKey: BookmarkStore.noteKey(note.id)
        )
    }

    private var speechText: String {
        prefs.renderMarkdown && previewMode
            ? MarkdownText.plainText(draft.text)
            : draft.text
    }

    private var editorFontSize: Double {
        15.0 * min(1.5, max(0.75, prefs.readerTextScale))
    }

    private var progressText: String {
        var parts = ["\(note.wordCount) words"]
        if let minutes = note.estimatedListenMinutes {
            parts.append("~\(minutes) min listen")
        }
        if let pos = tts.position, pos.noteId == note.id {
            parts.append("\(Int((pos.fraction * 100).rounded()))%")
        }
        if let error = tts.lastError {
            parts.append("last TTS error: \(error)")
        }
        return parts.joined(separator: " · ")
    }

    /// The button and the Ctrl+Shift+Enter shortcut land here, so they can
    /// never diverge.
    private func renderNow() {
        exportStatus = "rendering…"
        Task { await renderWav() }
    }

    /// Renders the whole note to one WAV under Exports/ — chunk → synthesize
    /// → concatenate, mirroring the iOS renderWAV flow. Soft-fails like the
    /// tier: a failed render leaves a status line, never a crash.
    private func renderWav() async {
        let engineKind = EngineKind(rawValue: prefs.engineKind) ?? .espeak
        let speed = Float(prefs.rateMultiplier)
        let text = notes.allNotes.first(where: { $0.id == note.id })?.text ?? note.text
        let chunks = TTSChunker.planChunks(noteId: note.id, text: text)
        guard !chunks.isEmpty else {
            exportStatus = "nothing to render"
            return
        }
        let outDir = AppPaths.exportsDir
        try? FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)
        let safeTitle = note.title.replacingOccurrences(of: "/", with: "-")
        let outFile = outDir.appendingPathComponent("\(safeTitle.prefix(60)).wav")

        let result: Result<Int, Error> = await Task.detached(priority: .userInitiated) {
            let engine = try EngineFactory.make(kind: engineKind)
            guard engine.modelCreated() else {
                throw TTSError.synthesisFailed("\(engineKind.displayName) is not ready")
            }
            var all: [Int16] = []
            var rate = 22_050
            for chunk in chunks {
                let wavURL = TTSChunker.cacheFile(noteId: note.id, index: chunk.index)
                _ = try engine.encodeSpeechImpl(
                    text: chunk.text, speed: speed, outFile: wavURL, abort: { false }
                )
                let (samples, chunkRate) = try WAVFile.read(at: wavURL)
                rate = chunkRate
                all.append(contentsOf: samples)
                try? FileManager.default.removeItem(at: wavURL)
            }
            try WAVFile.write(samples: all, sampleRate: rate, to: outFile)
            return rate
        }.result

        switch result {
        case .success:
            exportStatus = "rendered → Exports/\(outFile.lastPathComponent)"
        case .failure(let error):
            exportStatus = "render failed: \(error)"
        }
    }

    /// The body binding. Writes land in the draft, not the store — see
    /// ``DraftBuffer`` for why that matters — and the store is updated once
    /// the user pauses.
    private var textBinding: Binding<String> {
        Binding(
            get: { draft.text },
            set: { newText in
                if draft.setText(newText) {
                    draft = draft
                    sync.schedule { flushDraft() }
                }
            }
        )
    }

    /// The title field. While the draft has no explicit title the field
    /// shows the DERIVED title (iOS's `titleBinding`), and the first
    /// keystroke makes it explicit; an emptied field falls back to derived.
    private var titleBinding: Binding<String> {
        Binding(
            get: {
                draft.title.isEmpty ? note.title : draft.title
            },
            set: { newTitle in
                if draft.setTitle(newTitle) {
                    draft = draft
                    sync.schedule { flushDraft() }
                }
            }
        )
    }

    private var surfaceSelection: Binding<String?> {
        Binding(
            get: { previewMode ? "Preview" : "Edit" },
            set: { name in previewMode = (name != "Edit") }
        )
    }

    private var readAlongBinding: Binding<Bool> {
        Binding(
            get: { readAlongEnabled },
            set: {
                readAlongEnabled = $0
                prefs.readAlongEnabled = $0
            }
        )
    }
}
