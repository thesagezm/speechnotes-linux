import Foundation
import SwiftCrossUI
import Shortcuts

/// Whether the in-app keyboard reference is offered, and whether the window
/// shows its own hint row.
///
/// Why it is a setting rather than always-on: the hint row is chrome over
/// the top of the window, and some people want every pixel back. It defaults
/// OFF for exactly that reason — a shortcut nobody has been told about is
/// only useful if it is discoverable from somewhere, and the Shortcuts pane
/// in the sidebar is that somewhere. Turning it on costs one row.
@MainActor
enum KeyboardHelp {
    private static let hintsKey = "showKeyboardHints"

    /// Reads the pref once per render; cheap, and a Bool read is not what
    /// makes this pane slow.
    static var hintsVisible: Bool {
        UserDefaults.standard.bool(forKey: hintsKey)
    }

    static var visibleBinding: Binding<Bool> {
        Binding(
            get: { UserDefaults.standard.bool(forKey: hintsKey) },
            set: { UserDefaults.standard.set($0, forKey: hintsKey) }
        )
    }

    /// The one-line reminder shown under the toolbar when enabled. Built from
    /// the first entry of each group so it cannot drift from the table.
    static var hintLine: String {
        let picks = Shortcuts.table
            .filter { ["Notes", "Speech"].contains($0.group) }
            .prefix(2)
            .map { "\($0.keys) \($0.title.lowercased())" }
        return picks.joined(separator: " · ") + " · Ctrl+Shift+K for all"
    }
}
