import Foundation
import SwiftCrossUI
import SpeechLogic
import Data
import Appearance

/// The markdown reading surface: the note's blocks rendered with real
/// typography instead of raw syntax.
///
/// This is the piece the iOS `MarkdownPreviewView` is, minus the parts that
/// need an `AttributedString` or a network image fetch. `MarkdownText`
/// (798 lines, ported and unit-tested in SpeechLogicTests) has had no call
/// site until now; the parser was already doing the hard work.
///
/// Deliberately not ported: inline emphasis within a paragraph (needs
/// attributed strings), publisher CSS, and image rendering (needs a GTK
/// image widget behind a representable). Image tokens are still shown, as
/// their alt text, so a note does not silently lose content.
struct MarkdownPreview: View {
    let text: String
    let textScale: Double

    @State private var theme = ThemeController.shared
    /// Parsed once per text, not per render: the body re-evaluates on every
    /// playback tick when read-along is on, and re-parsing a long note then
    /// is exactly the freeze the editor used to have.
    @State private var blocks: [MarkdownText.MarkdownBlock] = []
    @State private var parsedSource: String = ""

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 10) {
                ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                    row(for: block)
                }
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task { parseIfNeeded() }
        .onChange(of: text) { parseIfNeeded() }
    }

    private func parseIfNeeded() {
        guard parsedSource != text else { return }
        blocks = MarkdownText.blocks(text)
        parsedSource = text
    }

    private var body1: Double { 16.0 * clampedScale }

    private var clampedScale: Double {
        min(1.5, max(0.75, textScale))
    }

    // MARK: - Blocks

    @ViewBuilder
    private func row(for block: MarkdownText.MarkdownBlock) -> some View {
        switch block {
        case .heading(let level, let text):
            Text(text)
                .font(headingFont(level))
                .foregroundColor(theme.text)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, level <= 2 ? 10 : 4)

        case .paragraph(let text):
            Text(text)
                .font(.system(size: body1))
                .foregroundColor(theme.text)
                .frame(maxWidth: .infinity, alignment: .leading)

        case .bulletList(let items), .orderedList(let items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                    HStack(spacing: 6) {
                        Text(marker(for: item, index: index, ordered: isOrdered(block)))
                            .foregroundColor(theme.text)
                        Text(item.text)
                            .font(.system(size: body1))
                            .foregroundColor(theme.text)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    // Indentation is padding, not nesting: SCU has no
                    // indent modifier and a nested VStack per level would
                    // multiply the node count for a cosmetic indent.
                    .padding(.leading, 14 * item.level)
                }
            }

        case .quote(let text):
            HStack(alignment: .top, spacing: 8) {
                Rectangle()
                    .fill(theme.accent.opacity(0.5))
                    .frame(width: 3)
                Text(text)
                    .font(.system(size: body1))
                    .foregroundColor(theme.text)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .fixedSize(horizontal: false, vertical: true)

        case .code(_, let code):
            // Monospace via the system design; SCU has no syntax colouring.
            Text(code)
                .font(.system(size: body1 - 1, design: .monospaced))
                .foregroundColor(theme.text)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(8)
                .background(theme.surface.card)
                .cornerRadius(5)

        case .divider:
            Divider()

        case .image(let alt, _):
            // The image itself needs a GTK image widget; until that exists
            // the alt text is shown so nothing silently disappears.
            Text(alt.isEmpty ? "[image]" : "[image: \(alt)]")
                .font(.footnote)
                .foregroundColor(theme.text)
                .frame(maxWidth: .infinity, alignment: .leading)

        case .table(let headers, let rows):
            // Rendered as aligned monospace columns: SCU has no
            // horizontally scrolling container, so a wide table is
            // truncated rather than scrollable.
            VStack(alignment: .leading, spacing: 2) {
                Text(headers.joined(separator: "  ·  "))
                    .font(.system(size: body1 - 1, weight: .semibold))
                    .foregroundColor(theme.text)
                Divider()
                ForEach(Array(rows.enumerated()), id: \.offset) { _, row in
                    Text(row.joined(separator: "  ·  "))
                        .font(.system(size: body1 - 1))
                        .foregroundColor(theme.text)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .padding(8)
            .background(theme.surface.card)
            .cornerRadius(5)
        }
    }

    private func isOrdered(_ block: MarkdownText.MarkdownBlock) -> Bool {
        if case .orderedList = block { return true }
        return false
    }

    private func marker(
        for item: MarkdownText.ListItem, index: Int, ordered: Bool
    ) -> String {
        if item.isTask { return item.isDone ? "☑" : "☐" }
        return ordered ? "\(index + 1)." : "•"
    }

    private func headingFont(_ level: Int) -> Font {
        let scales: [Double] = [1.6, 1.4, 1.2, 1.1, 1.0, 1.0]
        return .system(size: body1 * scales[min(max(level, 1), 6) - 1], weight: .semibold)
    }
}
