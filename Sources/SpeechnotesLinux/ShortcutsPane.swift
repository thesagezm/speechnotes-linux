import SwiftCrossUI
import Shortcuts
import Appearance

/// The keyboard-shortcut reference. Renders ``Shortcuts/table`` verbatim —
/// the same table the key matcher honours — so this pane cannot drift from
/// what the keys actually do.
struct ShortcutsPane: View {
    @State private var theme = ThemeController.shared

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("Keyboard shortcuts")
                    .font(.title3.weight(.semibold))
                Text("Every shortcut is Ctrl-based, so typing in the editor is never intercepted.")
                    .font(.callout)
                    .foregroundColor(theme.text)
                ForEach(Shortcuts.grouped, id: \.group) { group in
                    section(group.group, group.bindings)
                }
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func section(_ title: String, _ bindings: [Shortcuts.Binding]) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title.uppercased())
                .font(.caption.weight(.medium))
                .foregroundColor(theme.text)
                .padding(.bottom, 2)
            ForEach(bindings) { binding in
                HStack(spacing: 12) {
                    Text(binding.keys)
                        .font(.system(size: 12, design: .monospaced))
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(chipTint)
                        .cornerRadius(4)
                        .frame(width: 170, alignment: .leading)
                    Text(binding.title)
                        .font(.callout)
                    Spacer(minLength: 0)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var chipTint: Color {
        switch theme.effectiveScheme {
        case .light: return Color(white: 0.0, opacity: 0.06)
        case .dark: return Color(white: 1.0, opacity: 0.07)
        }
    }
}
