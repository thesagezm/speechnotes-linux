import CGTK4
import Foundation
import GtkBackend
import Gtk
import SwiftCrossUI
import Shortcuts
import Log

/// The GTK half of the keyboard layer: it owns the key controller and turns
/// keypresses into ``Shortcuts/Command``s.
///
/// SwiftCrossUI 0.9 has no keyboard API at all — `View` has no
/// `onKeyPress`, `MenuItem` has no accelerator slot, and the GTK backend
/// installs no key controller on the window, so key events belong entirely to
/// whichever widget has focus. The supported way in is
/// `View.inspectWindow`, which hands over the live `Gtk.ApplicationWindow`;
/// from there a `Gtk.EventControllerKey` in the BUBBLE phase sees every
/// keypress before any widget does. An unhandled key returns without
/// stopping the event, so it still reaches the editor.
///
/// This type does no interpretation: ``Shortcuts/Shortcuts`` decides what a
/// key means, and ``AppCommands`` decides what to do about it.
@MainActor
final class ShortcutHub {
    static let shared = ShortcutHub()

    /// GTK takes its own reference when a controller is added to a widget,
    /// but holding the Swift wrapper keeps the closures (and the commands
    /// singleton they capture) alive for the life of the window.
    private var retained: [EventControllerKey] = []

    private init() {}

    /// Installs the key controller. Called once, from the window lifecycle
    /// hook, when the window actually exists.
    func attach(to window: Gtk.ApplicationWindow) {
        let controller = EventControllerKey()
        controller.propagationPhase = .bubble
        controller.propagationLimit = .none
        controller.keyPressed = { [weak self] _, keyval, _, state in
            MainActor.assumeIsolated {
                self?.dispatch(keyval: keyval, state: state)
            }
        }
        window.addEventController(controller)
        retained.append(controller)
        Log.info("Keyboard: \(Shortcuts.table.count) bindings attached")
    }

    private func dispatch(keyval: UInt, state: GdkModifierType) {
        // GdkModifierType imports as a plain C enum, so the bits are read by
        // hand — Shortcuts only ever consults Ctrl and Shift.
        let modifiers = Shortcuts.Modifiers(raw: UInt(state.rawValue))
        guard let command = Shortcuts.command(keyval: keyval, modifiers: modifiers) else { return }
        AppCommands.shared.perform(command)
    }
}
