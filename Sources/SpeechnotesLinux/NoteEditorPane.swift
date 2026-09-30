import Foundation
import SwiftCrossUI
import Data
import AppPaths
import TTSEngine
import Appearance

/// The note editor: optional explicit title, the body TextEditor, the
/// pin/star/delete controls, and the speak bar driving the TTS tier.
/// SwiftCrossUI's TextEditor is a plain Binding<String> (no attributed
/// ranges), so formatting flows into the text at the end — same constraint
/// the plan recorded for dsnote parity.
struct NoteEditorPane: View {
    let note: Note
    let notes: NotesStore

    @State private var tts = TTSController.shared
    @State private var prefs = Prefs.shared
    @State private var theme = ThemeController.shared

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
                    Button("▶ Speak") {
                        tts.play(
                            note: note,
                            engineKind: EngineKind(rawValue: prefs.engineKind) ?? .espeak,
                            speed: Float(prefs.rateMultiplier),
                            voice: prefs.voiceForEngine(EngineKind(rawValue: prefs.engineKind) ?? .espeak)
                        )
                    }
                    .buttonStyle(.bordered)
                    .foregroundColor(theme.accent)
                }
                Divider()
                Button("Delete") {
                    tts.stop()
                    notes.delete(noteId: note.id)
                }
                .buttonStyle(.borderless)
                Button("⤓ WAV") {
                    exportStatus = "rendering…"
                    Task { await renderWav() }
                }
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
            TextEditor(text: textBinding)
                .font(.system(size: editorFontSize))
        }
        .padding(12)
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

    @State private var exportStatus: String?

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

    /// Writes through the store so every keystroke bumps updatedAt and hits
    /// the debounced save — the editor never owns note state.
    private var textBinding: Binding<String> {
        Binding(
            get: {
                notes.allNotes.first(where: { $0.id == note.id })?.text ?? ""
            },
            set: { newText in
                guard var current = notes.allNotes.first(where: { $0.id == note.id }) else {
                    return
                }
                current.text = newText
                notes.update(current)
            }
        )
    }

    /// Empty string clears the explicit title (falls back to derived).
    private var titleBinding: Binding<String> {
        Binding(
            get: {
                notes.allNotes.first(where: { $0.id == note.id })?.explicitTitle ?? ""
            },
            set: { newTitle in
                guard var current = notes.allNotes.first(where: { $0.id == note.id }) else {
                    return
                }
                let trimmed = newTitle.trimmingCharacters(in: .whitespacesAndNewlines)
                current.explicitTitle = trimmed.isEmpty ? nil : trimmed
                notes.update(current)
            }
        )
    }
}
