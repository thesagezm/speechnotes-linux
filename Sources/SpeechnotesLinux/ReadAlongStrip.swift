import Foundation
import SwiftCrossUI
import SpeechLogic
import Data
import TTSEngine
import Appearance

/// The read-along surface: the note's text with the sentence currently
/// sounding marked.
///
/// SwiftCrossUI has no `AttributedString`, so the iOS implementation (one
/// `Text` with an in-place highlight) is not portable. What is portable is
/// the effect: the text is split into paragraphs, and the paragraph holding
/// the sounding sentence is tinted with the accent. That is enough to keep
/// a reader's place, which is the whole point of the surface.
///
/// The sentence itself comes from `TTSController.currentSentence`, which
/// publishes at sentence granularity — the same source iOS's read-along
/// uses, and the one that fixed the original bug (highlighting at chunk
/// schedule time ran seconds ahead of the audio).
struct ReadAlongStrip: View {
    let text: String
    let scale: Double

    @State private var tts = TTSController.shared
    @State private var theme = ThemeController.shared
    /// Paragraph start offsets, so a sentence can be located without
    /// re-splitting the text on every tick.
    @State private var paragraphs: [Paragraph] = []

    private struct Paragraph: Equatable {
        let start: Int
        let text: String
        var end: Int { start + text.utf16.count }
    }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                ForEach(Array(paragraphs.enumerated()), id: \.offset) { _, paragraph in
                    Text(paragraph.text)
                        .font(.system(size: 16.0 * clampedScale))
                        .foregroundColor(accent(paragraph))
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(background(paragraph))
                        .cornerRadius(5)
                }
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task { splitOnce() }
    }

    private var clampedScale: Double {
        min(1.5, max(0.75, scale))
    }

    /// The sounding sentence's UTF-16 range in `text`, or nil.
    private var activeRange: Range<Int>? {
        guard let sentence = tts.currentSentence, !sentence.isEmpty else { return nil }
        guard let range = text.range(of: sentence) else { return nil }
        let lower = text.utf16.distance(from: text.utf16.startIndex, to: range.lowerBound)
        return lower..<(lower + sentence.utf16.count)
    }

    private func splitOnce() {
        guard paragraphs.isEmpty, !text.isEmpty else { return }
        var result: [Paragraph] = []
        var start = 0
        for raw in text.split(separator: "\n", omittingEmptySubsequences: false) {
            let piece = String(raw)
            result.append(Paragraph(start: start, text: piece))
            // +1 for the newline that `split` consumed.
            start += piece.utf16.count + 1
        }
        paragraphs = result
    }

    private func isActive(_ paragraph: Paragraph) -> Bool {
        guard let range = activeRange else { return false }
        return range.lowerBound < paragraph.end && range.upperBound > paragraph.start
    }

    private func accent(_ paragraph: Paragraph) -> Color {
        isActive(paragraph) ? theme.accent : theme.text
    }

    private func background(_ paragraph: Paragraph) -> Color {
        isActive(paragraph) ? theme.accent.opacity(0.10) : .clear
    }
}
