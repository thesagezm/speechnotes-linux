import CGTK4
import Foundation
import Gtk
import SwiftCrossUI
import Log

/// Keyboard focus for the notes list's search field.
///
/// SwiftCrossUI 0.9 has no focus API at all — no `FocusState`, no
/// `.focused()`, and the GTK backend never binds `gtk_widget_grab_focus` —
/// so a Ctrl+F shortcut could not otherwise put the caret in the search box.
/// `.inspect` on a `TextField` hands over the live `Gtk.Entry`, and GTK's
/// own C API is available through `CGTK4`, so the grab is one call.
///
/// The request is a counter, not a flag: a Bool cannot distinguish "asked
/// again while already asked" from "never asked", and the second Ctrl+F
/// would silently do nothing.
@MainActor
enum SearchFieldFocus {
    private static var handledRequest = 0
    private static var attachedEntry: UnsafeMutableRawPointer?

    /// Called from `.inspect(.onCreate)`, i.e. once when the entry is built.
    static func attach(to entry: Gtk.Entry, request: Int) {
        attachedEntry = UnsafeMutableRawPointer(entry.widgetPointer)
        // A request that arrived before the entry existed is honoured now.
        if request > handledRequest {
            handledRequest = request
            grab()
        }
    }

    /// Called by the pane when its `focusSearchRequest` changes.
    static func request(_ value: Int) {
        guard value > handledRequest else { return }
        handledRequest = value
        grab()
    }

    private static func grab() {
        guard let attachedEntry else {
            Log.info("Ctrl+F: the search field is not on screen yet")
            return
        }
        // GtkEntry* reinterprets to GtkWidget* exactly like GTK's GTK_WIDGET
        // macro; grab_focus is on the base class.
        gtk_widget_grab_focus(
            attachedEntry.assumingMemoryBound(to: GtkWidget.self)
        )
    }
}
