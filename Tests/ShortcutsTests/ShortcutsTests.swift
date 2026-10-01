import XCTest
@testable import Shortcuts

/// The keysym matcher decides whether a keypress belongs to the app or to
/// whatever widget has focus. Both directions matter: a table entry that no
/// key reaches is a dead shortcut, and a bare key the matcher claims is a
/// hijacked editor. These tests pin both, plus the guarantee that the help
/// pane's table and the matcher can never disagree.
final class ShortcutsTests: XCTestCase {
    private func mods(ctrl: Bool = false, shift: Bool = false) -> Shortcuts.Modifiers {
        var raw: UInt = 0
        if ctrl { raw |= Shortcuts.Modifiers.controlMask }
        if shift { raw |= Shortcuts.Modifiers.shiftMask }
        return Shortcuts.Modifiers(raw: raw)
    }

    private func command(_ keyval: UInt, ctrl: Bool = false, shift: Bool = false) -> Shortcuts.Command? {
        Shortcuts.command(keyval: keyval, modifiers: mods(ctrl: ctrl, shift: shift))
    }

    private func keyval(_ letter: Character) -> UInt {
        keysymFor(letter)
    }

    /// 'a' is the first letter key. Written with explicit Int math because
    /// `UInt(big) - UInt(a)` would trap on a capital letter — which the
    /// table's own keys ("Ctrl+Shift+N") contain.
    private func keysymFor(_ letter: Character) -> UInt {
        Shortcuts.Keysym.a + UInt(Int(letter.lowercased().first!.asciiValue!) - Int(Character("a").asciiValue!))
    }

    /// Top-row digit keysym ('1' is 0x031).
    private func digit(_ character: Character) -> UInt {
        0x031 + UInt(character.asciiValue! - Character("1").asciiValue!)
    }

    // MARK: - The help table matches the matcher

    /// Every advertised binding must resolve to the command its title names,
    /// and no two bindings may share a key. This is the test that keeps the
    /// Shortcuts pane honest.
    func testTableIsCompleteConsistentAndUnique() {
        XCTAssertEqual(Set(Shortcuts.table.map(\.keys)).count, Shortcuts.table.count,
                       "two bindings advertise the same chord")
        for binding in Shortcuts.table {
            XCTAssertNotNil(
                parse(binding.keys), "\(binding.keys) (\(binding.title)) is not in the test vocabulary"
            )
        }
        XCTAssertFalse(Shortcuts.table.isEmpty)
    }

    /// The table's keys, re-parsed with the same grammar the help pane
    /// prints, must produce exactly the documented command set.
    func testTableKeysResolveToTheDocumentedCommands() {
        let expected: Set<Shortcuts.Command> = [
            .newNote, .newNotebook, .focusSearch, .togglePin, .toggleStar, .deleteNote,
            .openNotes, .openBooks, .openRecycleBin, .openSettings,
            .showAbout, .showLogs, .showShortcuts, .closeWindow,
            .toggleSpeech, .stopSpeech, .renderSelectedToWav,
        ]
        let resolved = Set(Shortcuts.table.compactMap { binding -> Shortcuts.Command? in
            guard let (keyval, modifiers) = parse(binding.keys) else { return nil }
            return Shortcuts.command(keyval: keyval, modifiers: modifiers)
        })
        XCTAssertEqual(resolved, expected,
                       "a binding in the help table does not reach the matcher, or vice versa")
    }

    /// "Ctrl+Shift+N" → keysym n with the Ctrl|Shift bits. The key name is
    /// lowercased before lookup because the table prints letters in the
    /// conventional capitalised form ("Ctrl+Shift+N"); the keysym itself is
    /// the bare letter, so "Ctrl+N" and "Ctrl+Shift+N" differ only in the
    /// shift bit — which is exactly what the matcher keys off.
    private func parse(_ keys: String) -> (UInt, Shortcuts.Modifiers)? {
        var raw: UInt = 0
        var keyName = ""
        for part in keys.split(separator: "+") {
            switch part {
            case "Ctrl": raw |= Shortcuts.Modifiers.controlMask
            case "Shift": raw |= Shortcuts.Modifiers.shiftMask
            default: keyName = String(part)
            }
        }
        let keyval: UInt
        switch keyName {
        case "Space": keyval = Shortcuts.Keysym.space
        case ",": keyval = Shortcuts.Keysym.comma
        case ".": keyval = Shortcuts.Keysym.period
        case "Enter": keyval = Shortcuts.Keysym.returnKey
        default:
            guard keyName.count == 1,
                  let ascii = keyName.lowercased().first?.asciiValue
            else {
                // A key name this grammar does not know is a table bug, not
                // something to guess at — fail the test, never trap.
                XCTFail("unparsable key name “\(keyName)” in \(keys)")
                return nil
            }
            switch ascii {
            case 0x61...0x7a: keyval = Shortcuts.Keysym.a + UInt(Int(ascii) - 0x61)  // a–z
            case 0x31...0x39: keyval = 0x031 + UInt(Int(ascii) - 0x31)                // 1–9
            default:
                XCTFail("no keysym for “\(keyName)” in \(keys)")
                return nil
            }
        }
        return (keyval, Shortcuts.Modifiers(raw: raw))
    }

    // MARK: - Each binding fires

    func testLetterBindings() {
        XCTAssertEqual(command(keyval("n"), ctrl: true), .newNote)
        XCTAssertEqual(command(keyval("n"), ctrl: true, shift: true), .newNotebook)
        XCTAssertEqual(command(keyval("f"), ctrl: true), .focusSearch)
        XCTAssertEqual(command(keyval("p"), ctrl: true, shift: true), .togglePin)
        XCTAssertEqual(command(keyval("s"), ctrl: true, shift: true), .toggleStar)
        XCTAssertEqual(command(keyval("d"), ctrl: true, shift: true), .deleteNote)
        XCTAssertEqual(command(keyval("k"), ctrl: true, shift: true), .showShortcuts)
        XCTAssertEqual(command(keyval("o"), ctrl: true, shift: true), .showAbout)
        XCTAssertEqual(command(keyval("l"), ctrl: true, shift: true), .showLogs)
        XCTAssertEqual(command(keyval("w"), ctrl: true), .closeWindow)
        XCTAssertEqual(command(digit("1"), ctrl: true), .openNotes)
        XCTAssertEqual(command(digit("2"), ctrl: true), .openBooks)
        XCTAssertEqual(command(digit("3"), ctrl: true), .openRecycleBin)
    }

    func testPunctuationBindings() {
        XCTAssertEqual(command(Shortcuts.Keysym.comma, ctrl: true), .openSettings)
        XCTAssertEqual(command(Shortcuts.Keysym.period, ctrl: true), .stopSpeech)
        XCTAssertEqual(command(Shortcuts.Keysym.space, ctrl: true), .toggleSpeech)
        XCTAssertEqual(command(Shortcuts.Keysym.returnKey, ctrl: true, shift: true), .renderSelectedToWav)
        XCTAssertEqual(command(Shortcuts.Keysym.keypadEnter, ctrl: true, shift: true), .renderSelectedToWav)
    }

    // MARK: - Nothing else is claimed

    /// The editor has no way to declare focus, so a bare key must always
    /// reach the widget. This is the test that keeps typing working.
    func testBareKeysAreNeverClaimed() {
        for letter in "abcdefghijklmnopqrstuvwxyz".map(Character.init) {
            XCTAssertNil(command(keyval(letter)), "bare \(letter) must reach the editor")
        }
        XCTAssertNil(command(Shortcuts.Keysym.space), "bare space must reach the editor")
        XCTAssertNil(command(Shortcuts.Keysym.returnKey), "bare Enter must reach the editor")
        XCTAssertNil(command(Shortcuts.Keysym.comma), "bare comma must reach the editor")
        XCTAssertNil(command(Shortcuts.Keysym.returnKey, shift: true),
                     "Shift+Enter alone must reach the editor")
    }

    /// Chords that are not in the table — including ones a user might
    /// expect from another app — must fall through rather than be guessed at.
    func testUnboundChordsFallThrough() {
        XCTAssertNil(command(keyval("w"), ctrl: true, shift: true), "Ctrl+Shift+W is unbound")
        XCTAssertNil(command(keyval("q"), ctrl: true), "Ctrl+Q is unbound")
        XCTAssertNil(command(keyval("z"), ctrl: true, shift: true))
        XCTAssertNil(command(Shortcuts.Keysym.space, ctrl: true, shift: true))
        XCTAssertNil(command(digit("4"), ctrl: true))
        XCTAssertNil(command(Shortcuts.Keysym.escape, ctrl: true))
    }

    /// Modifier bits outside Ctrl/Shift are ignored entirely. GTK hands the
    /// full state — including CapsLock and the mouse buttons — and a window
    /// manager's Alt chord must never be swallowed by the app.
    func testForeignModifierBitsDoNotAffectMatching() {
        var raw: UInt = Shortcuts.Modifiers.controlMask
        raw |= 0x2    // GDK_LOCK_MASK (CapsLock)
        raw |= 0x100  // GDK_BUTTON1_MASK
        raw |= 0x8    // GDK_ALT_MASK
        raw |= 1 << 26  // GDK_SUPER_MASK
        let state = Shortcuts.Modifiers(raw: raw)
        XCTAssertEqual(Shortcuts.command(keyval: keyval("n"), modifiers: state), .newNote)
        // And a bare key stays bare no matter how many other bits are set.
        XCTAssertNil(Shortcuts.command(keyval: keyval("n"), modifiers: Shortcuts.Modifiers(raw: raw & ~Shortcuts.Modifiers.controlMask)))
    }

    // MARK: - Keysym decoding

    func testLetterMapping() {
        XCTAssertEqual(Shortcuts.letter(for: Shortcuts.Keysym.a), "a")
        XCTAssertEqual(Shortcuts.letter(for: Shortcuts.Keysym.z), "z")
        XCTAssertNil(Shortcuts.letter(for: 0), "keysym 0 is not a letter")
        XCTAssertNil(Shortcuts.letter(for: Shortcuts.Keysym.space))
        XCTAssertNil(Shortcuts.letter(for: 0x0100_0001), "a dead/composed key")
        // Capital letters are the same keysym on the keyboard; the keyval is
        // the letter, not the case.
        XCTAssertEqual(Shortcuts.letter(for: keyval("K")), "k")
    }

    // MARK: - Grouping for the help pane

    func testGroupingPreservesOrderAndCoversEveryBinding() {
        let grouped = Shortcuts.grouped
        XCTAssertEqual(
            Set(grouped.map(\.group)), Set(Shortcuts.table.map(\.group)),
            "grouping lost or invented a group"
        )
        XCTAssertEqual(
            grouped.flatMap(\.bindings).count, Shortcuts.table.count,
            "grouping lost a binding"
        )
        // Groups appear in first-seen order, which is the order the table is
        // written in — the help pane depends on it.
        XCTAssertEqual(grouped.map(\.group), ["Notes", "Navigate", "Speech"])
    }
}
