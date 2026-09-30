import Foundation
import SwiftCrossUI
import Data
import TTSEngine
import Appearance

/// What's playing right now, resolved by the shell (which observes the
/// stores) and rendered by the mini-player bar. Note TTS, book TTS and
/// audiobook playback are mutually exclusive, so at most one is ever active.
enum ActivePlayback {
    case noteTTS(note: Note, fraction: Double, paused: Bool)
    case bookTTS(book: Book, fraction: Double, paused: Bool)
    case audiobook(book: Book, chapterIndex: Int, seconds: Double, fraction: Double, paused: Bool)

    var title: String {
        switch self {
        case .noteTTS(let note, _, _):
            return note.title
        case .bookTTS(let book, _, _):
            return book.title
        case .audiobook(let book, let chapterIndex, _, _, _):
            return "\(book.title) · Chapter \(chapterIndex + 1)"
        }
    }

    var kindLabel: String {
        switch self {
        case .noteTTS: return "Note · text-to-speech"
        case .bookTTS: return "Book · read-aloud"
        case .audiobook: return "Audiobook"
        }
    }

    var isPaused: Bool {
        switch self {
        case .noteTTS(_, _, let paused),
             .bookTTS(_, _, let paused),
             .audiobook(_, _, _, _, let paused):
            return paused
        }
    }

    /// True while the run has started but no audio position has arrived yet
    /// — the engine is still loading/synthesizing the first chunk (Kokoro
    /// can take a couple of seconds). The spinner replaces a frozen "0%".
    var isPreparing: Bool {
        switch self {
        case .noteTTS(_, let fraction, let paused),
             .bookTTS(_, let fraction, let paused),
             .audiobook(_, _, _, let fraction, let paused):
            return !paused && fraction == 0
        }
    }

    var progressLabel: String {
        switch self {
        case .noteTTS(_, let fraction, _),
             .bookTTS(_, let fraction, _):
            return "\(Int((fraction * 100).rounded()))%"
        case .audiobook(_, _, let seconds, let fraction, _):
            return "\(Self.clock(seconds)) · \(Int((fraction * 100).rounded()))%"
        }
    }

    private static func clock(_ seconds: Double) -> String {
        let s = Int(seconds.rounded())
        return String(format: "%d:%02d", s / 60, s % 60)
    }
}

/// The persistent transport under the shell: playback survives navigation,
/// mirroring the iOS mini-player. Pause/resume/stop route to whichever
/// controller owns the active run. Self-contained: it observes the
/// controllers and stores directly, so the shell only flips on the rare
/// visibility transitions (via PlaybackPresence) — never on position ticks.
struct MiniPlayerBar: View {
    @State private var tts = TTSController.shared
    @State private var audio = AudioBookController.shared
    @State private var notes = NotesStore.shared
    @State private var books = BooksStore.shared
    @State private var theme = ThemeController.shared

    /// The active run, resolved locally: audiobook first (it and TTS are
    /// mutually exclusive by controller contract), then TTS — a note id
    /// resolves through NotesStore, anything else through the shelf.
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

    var body: some View {
        if let playback = activePlayback {
            content(playback)
        }
    }

    private func content(_ playback: ActivePlayback) -> some View {
        HStack(spacing: 10) {
            Text(playback.isPaused ? "❙❙" : "▸")
                .font(.system(size: 12, weight: .medium))
                .foregroundColor(theme.accent)
                .frame(width: 24, height: 24)
                .background(theme.accent.opacity(0.14))
                .cornerRadius(12)
            VStack(alignment: .leading, spacing: 1) {
                Text(playback.title)
                    .font(.system(size: 13, weight: .medium))
                Text(playback.kindLabel)
                    .font(.caption2)
                    .foregroundColor(theme.text)
            }
            Spacer()
            if playback.isPreparing {
                ProgressView()
            }
            Text(playback.progressLabel)
                .font(.footnote)
                .foregroundColor(theme.text)
            if playback.isPaused {
                Button("▶ Resume") { control(.resume, playback) }
                    .buttonStyle(.bordered)
            } else {
                Button("❙❙ Pause") { control(.pause, playback) }
                    .buttonStyle(.bordered)
            }
            Button("■ Stop") { control(.stop, playback) }
                .buttonStyle(.bordered)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(barBackground)
    }

    private var barBackground: Color {
        switch theme.effectiveScheme {
        case .light: return Color(white: 0.0, opacity: 0.05)
        case .dark: return Color(white: 1.0, opacity: 0.045)
        }
    }

    private enum TransportAction {
        case pause, resume, stop
    }

    private func control(_ action: TransportAction, _ playback: ActivePlayback) {
        switch playback {
        case .noteTTS, .bookTTS:
            switch action {
            case .pause: tts.pause()
            case .resume: tts.resume()
            case .stop: tts.stop()
            }
        case .audiobook:
            switch action {
            case .pause: audio.pause()
            case .resume: audio.resume()
            case .stop: audio.stop()
            }
        }
    }
}
