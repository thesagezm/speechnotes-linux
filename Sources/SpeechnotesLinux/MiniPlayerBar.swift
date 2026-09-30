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
/// controller owns the active run.
struct MiniPlayerBar: View {
    let playback: ActivePlayback

    @State private var tts = TTSController.shared
    @State private var audio = AudioBookController.shared
    @State private var theme = ThemeController.shared

    var body: some View {
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
                    .foregroundColor(.gray)
            }
            Spacer()
            Text(playback.progressLabel)
                .font(.footnote)
                .foregroundColor(.gray)
            if playback.isPaused {
                Button("▶ Resume") { control(.resume) }
                    .buttonStyle(.bordered)
            } else {
                Button("❙❙ Pause") { control(.pause) }
                    .buttonStyle(.bordered)
            }
            Button("■ Stop") { control(.stop) }
                .buttonStyle(.bordered)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(barBackground)
    }

    private var barBackground: Color {
        switch theme.effectiveScheme {
        case .light: return Color(white: 0.0, opacity: 0.035)
        case .dark: return Color(white: 1.0, opacity: 0.045)
        }
    }

    private enum TransportAction {
        case pause, resume, stop
    }

    private func control(_ action: TransportAction) {
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
