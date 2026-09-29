import Foundation
import SwiftCrossUI
import Data
import AppPaths
import TTSEngine

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

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                Button(note.isPinned ? "Unpin" : "Pin") {
                    notes.setPinned(!note.isPinned, noteId: note.id)
                }
                Button(note.isFavorite ? "Unstar" : "Star") {
                    notes.setFavorite(!note.isFavorite, noteId: note.id)
                }
                Spacer()
                Text(progressText)
                if tts.isPlaying(noteId: note.id) {
                    if tts.state == .paused {
                        Button("▶ Resume") { tts.resume() }
                    } else {
                        Button("❙❙ Pause") { tts.pause() }
                    }
                    Button("■ Stop") { tts.stop() }
                } else {
                    Button("▶ Speak") {
                        tts.play(
                            note: note,
                            engineKind: EngineKind(rawValue: prefs.engineKind) ?? .espeak,
                            speed: Float(prefs.rateMultiplier),
                            voice: prefs.voice
                        )
                    }
                }
                Button("Delete") {
                    tts.stop()
                    notes.delete(noteId: note.id)
                }
                Button("⤓ WAV") {
                    exportStatus = "rendering…"
                    Task { await renderWav() }
                }
            }
            if let status = exportStatus {
                Text(status).foregroundColor(.gray)
            }
            if tts.isPlaying(noteId: note.id), let sentence = tts.currentSentence {
                Text("▸ \(sentence)")
                    .foregroundColor(.gray)
            }
            TextField("Title (optional)", text: titleBinding)
            TextEditor(text: textBinding)
        }
        .padding(8)
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
