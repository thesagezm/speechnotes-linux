import CGTK4
import Foundation
import GtkBackend
import SwiftCrossUI

/// Desktop-lifecycle glue for the GTK side of the app.
///
/// Two traps in SwiftCrossUI 0.9's application handling made the desktop
/// launcher look dead (2026-09-30): a second launch signals "open" on the
/// primary instance and exits — SCU registers for "open" but ignores it, so
/// nothing raised; and nothing tied the process to its window, so a closed
/// instance could linger and silently swallow every later launch. Both fixed
/// here: hand-off presents the window, and closing it quits the process.
@MainActor
enum WindowLifecycle {
    private static let state = WidgetSlot()

    /// Attaches to the native window backing this view (called once the
    /// window exists, so the application is registered and is the default).
    ///
    /// `.onCreate` fires on the node's first layout — NOT once per window —
    /// which matters: `.afterUpdate` re-fires on every window update and
    /// re-ran the whole hand-off wiring (including a second key controller)
    /// each time.
    static func attach<V: View>(_ view: V) -> some View {
        view.inspectWindow { window in
            // `inspectWindow` has no inspection-point parameter and runs its
            // action from `onCommit`, so it re-fires on EVERY window update.
            // Guarding on the window pointer is what makes this idempotent:
            // without it every repaint re-ran the wiring and installed
            // another key controller.
            let pointer = UnsafeMutableRawPointer(window.widgetPointer)
                .assumingMemoryBound(to: GtkWidget.self)
            guard state.store(pointer) else { return }

            window.onCloseRequest = { _ in
                if let app = g_application_get_default() {
                    g_application_quit(app)
                }
            }

            if let app = g_application_get_default() {
                connectHandOff(app: UnsafeMutableRawPointer(app))
            }
            ShortcutHub.shared.attach(to: window)
        }
    }

    private static func connectHandOff(app: UnsafeMutableRawPointer) {
        g_signal_connect_data(
            app,
            "open",
            unsafeBitCast(openHandler, to: GCallback.self),
            nil, nil, GConnectFlags(rawValue: 0)
        )
        g_signal_connect_data(
            app,
            "activate",
            unsafeBitCast(activateHandler, to: GCallback.self),
            nil, nil, GConnectFlags(rawValue: 0)
        )
    }

    /// open(GApplication, GFile**, gint, gchar* hint, gpointer) — the file
    /// list is irrelevant here, so it rides through as raw pointers (GFile is
    /// not visible through the CGTK4 modulemap's gtk.h umbrella).
    private static let openHandler: @convention(c) (
        UnsafeMutableRawPointer?,
        UnsafeMutableRawPointer?,
        Int32,
        UnsafeMutableRawPointer?,
        UnsafeMutableRawPointer?
    ) -> Void = { _, _, _, _, _ in
        WindowLifecycle.state.present()
    }

    /// activate(GApplication, gpointer)
    private static let activateHandler: @convention(c) (
        UnsafeMutableRawPointer?,
        UnsafeMutableRawPointer?
    ) -> Void = { _, _ in
        WindowLifecycle.state.present()
    }

    /// The window widget, guarded for access from plain C signal handlers
    /// and idempotent so repeated wiring cannot stack up.
    private final class WidgetSlot: @unchecked Sendable {
        private let lock = NSLock()
        private var widget: UnsafeMutablePointer<GtkWidget>?

        /// Stores the pointer; returns true the first time it is seen and
        /// false for every repeat, which is the caller's cue that the wiring
        /// has already been done for this window.
        @discardableResult
        func store(_ pointer: UnsafeMutablePointer<GtkWidget>) -> Bool {
            lock.lock()
            defer { lock.unlock() }
            if widget == pointer { return false }
            widget = pointer
            return true
        }

        func present() {
            lock.lock()
            let pointer = widget
            lock.unlock()
            guard let pointer else { return }
            // GtkWidget* reinterprets to GtkWindow* exactly like GTK's
            // GTK_WINDOW() macro.
            gtk_window_present_with_time(
                UnsafeMutableRawPointer(pointer).assumingMemoryBound(to: GtkWindow.self),
                0
            )
        }
    }
}

extension View {
    /// Presents the window on second-launch hand-off and quits on close.
    @MainActor
    func windowLifecycle() -> some View {
        WindowLifecycle.attach(self)
    }
}
