import Foundation

/// The app's keyboard bindings, as data.
///
/// This lives in its own module — not in the executable — for one reason:
/// it is pure, so it is the one part of the keyboard layer that can be
/// tested. The matcher below is what decides whether a keypress belongs to
/// the app or to whatever widget has focus, and getting that wrong means
/// either dead shortcuts or a hijacked editor.
///
/// Why an explicit table instead of decorator syntax on buttons: the GTK
/// backend installs no key controller and SwiftCrossUI 0.9 has no
/// `onKeyPress`, so the bindings have to be matched in one place against
/// raw keysyms — which means they can also be printed from one place. The
/// "Keyboard shortcuts" pane renders ``table`` verbatim, so the help cannot
/// drift from the behaviour.
public enum Shortcuts {
    /// One binding. `id` doubles as the ForEach identity in the help pane.
    public struct Binding: Equatable, Identifiable, Sendable {
        public let id: String
        public let title: String
        public let group: String
        /// Human-readable form, e.g. "Ctrl+Shift+N". Also the key the
        /// matcher is written against, which keeps the two in lockstep.
        public let keys: String

        public init(keys: String, title: String, group: String) {
            self.keys = keys
            self.title = title
            self.group = group
            id = keys
        }
    }

    /// What a key does. The app maps these onto its own actions; the names
    /// here are the user-facing vocabulary.
    public enum Command: Equatable, Sendable {
        case newNote
        case newNotebook
        case focusSearch
        case togglePin
        case toggleStar
        case deleteNote
        case openNotes
        case openBooks
        case openRecycleBin
        case openSettings
        case showAbout
        case showLogs
        case showShortcuts
        case closeWindow
        case toggleSpeech
        case stopSpeech
        case renderSelectedToWav
    }

    /// Every binding, in the order the help pane lists them.
    public static let table: [Binding] = [
        .init(keys: "Ctrl+N", title: "New note", group: "Notes"),
        .init(keys: "Ctrl+Shift+N", title: "New notebook", group: "Notes"),
        .init(keys: "Ctrl+F", title: "Search notes", group: "Notes"),
        .init(keys: "Ctrl+Shift+P", title: "Pin or unpin the selected note", group: "Notes"),
        .init(keys: "Ctrl+Shift+S", title: "Star or unstar the selected note", group: "Notes"),
        .init(keys: "Ctrl+Shift+D", title: "Move the selected note to the bin", group: "Notes"),
        .init(keys: "Ctrl+1", title: "All notes", group: "Navigate"),
        .init(keys: "Ctrl+2", title: "Books", group: "Navigate"),
        .init(keys: "Ctrl+3", title: "Recycle Bin", group: "Navigate"),
        .init(keys: "Ctrl+,", title: "Settings", group: "Navigate"),
        .init(keys: "Ctrl+Shift+O", title: "About", group: "Navigate"),
        .init(keys: "Ctrl+Shift+L", title: "Logs", group: "Navigate"),
        .init(keys: "Ctrl+Shift+K", title: "Keyboard shortcuts", group: "Navigate"),
        .init(keys: "Ctrl+W", title: "Close the window", group: "Navigate"),
        .init(keys: "Ctrl+Space", title: "Speak or pause the selected note", group: "Speech"),
        .init(keys: "Ctrl+.", title: "Stop speech", group: "Speech"),
        .init(keys: "Ctrl+Shift+Enter", title: "Render the selected note to WAV", group: "Speech"),
    ]

    /// The bindings grouped for display, preserving table order.
    public static var grouped: [(group: String, bindings: [Binding])] {
        var order: [String] = []
        var buckets: [String: [Binding]] = [:]
        for binding in table {
            if buckets[binding.group] == nil {
                buckets[binding.group] = []
                order.append(binding.group)
            }
            buckets[binding.group]?.append(binding)
        }
        return order.map { (group: $0, bindings: buckets[$0] ?? []) }
    }

    // MARK: - Matching

    /// GDK keysyms, raw. SwiftCrossUI's generated bindings do not expose
    /// them and GTK's header defines them as macros, so they are spelled
    /// out here rather than pulled in through a new dependency.
    public enum Keysym {
        public static let a: UInt = 0x061
        public static let z: UInt = 0x07a
        public static let one: UInt = 0x031
        public static let nine: UInt = 0x039
        public static let space: UInt = 0x020
        public static let comma: UInt = 0x02c
        public static let period: UInt = 0x02e
        public static let escape: UInt = 0xff1b
        public static let returnKey: UInt = 0xff0d
        public static let keypadEnter: UInt = 0xff8d
    }

    /// Modifier bits from `GdkModifierType`, which imports as a plain C
    /// enum rather than an OptionSet. Only Ctrl and Shift are ever
    /// consulted: Alt and Super chords belong to the window manager, and
    /// eating them would break the desktop underneath the app.
    public struct Modifiers: Equatable, Sendable {
        public static let shiftMask: UInt = 0x1
        public static let controlMask: UInt = 0x4

        public let raw: UInt
        public init(raw: UInt) { self.raw = raw }
        public var control: Bool { raw & Self.controlMask != 0 }
        public var shift: Bool { raw & Self.shiftMask != 0 }
    }

    /// Maps one keypress to a command, or nil when it is not ours.
    ///
    /// Every binding is Ctrl-based on purpose. SwiftCrossUI has no caret or
    /// focus API — `TextEditor` is a bare `Binding<String>` — so there is no
    /// way to ask "does the editor have focus" and exempt its keys. The only
    /// safe rule is that a bare key is never the app's.
    ///
    /// Shift is part of each binding rather than a modifier we ignore: a
    /// binding only fires on exactly the shift state its table row names, so
    /// Ctrl+Shift+N (new notebook) cannot double as Ctrl+N (new note) and
    /// Ctrl+Space cannot fire on Ctrl+Shift+Space.
    public static func command(keyval: UInt, modifiers: Modifiers) -> Command? {
        guard modifiers.control else { return nil }

        if let letter = letter(for: keyval) {
            switch (letter, modifiers.shift) {
            case ("n", false): return .newNote
            case ("n", true): return .newNotebook
            case ("f", false): return .focusSearch
            case ("p", true): return .togglePin
            case ("s", true): return .toggleStar
            case ("d", true): return .deleteNote
            case ("w", false): return .closeWindow
            case ("k", true): return .showShortcuts
            case ("o", true): return .showAbout
            case ("l", true): return .showLogs
            default: return nil
            }
        }

        // Top-row digits: '1'–'9' are keysyms 0x31–0x39, i.e. not letters.
        if let digit = digit(for: keyval) {
            switch (digit, modifiers.shift) {
            case (1, false): return .openNotes
            case (2, false): return .openBooks
            case (3, false): return .openRecycleBin
            default: return nil
            }
        }

        switch keyval {
        case Keysym.comma: return modifiers.shift ? nil : .openSettings
        case Keysym.period: return modifiers.shift ? nil : .stopSpeech
        case Keysym.space: return modifiers.shift ? nil : .toggleSpeech
        case Keysym.returnKey, Keysym.keypadEnter:
            return modifiers.shift ? .renderSelectedToWav : nil
        default: return nil
        }
    }

    /// The lowercase ASCII letter a keysym represents, if any.
    public static func letter(for keyval: UInt) -> Character? {
        guard keyval >= Keysym.a, keyval <= Keysym.z else { return nil }
        return Character(UnicodeScalar(UInt8(keyval - Keysym.a + 97)))
    }

    /// The top-row digit a keysym represents, 1–9. Keysyms '1'–'9' happen to
    /// equal their ASCII codes, but digits live outside the letter range and
    /// must be matched on their own — folding them into `letter` silently
    /// makes Ctrl+1..3 dead.
    public static func digit(for keyval: UInt) -> Int? {
        guard keyval >= Keysym.one, keyval <= Keysym.nine else { return nil }
        return Int(keyval - Keysym.one) + 1
    }
}
