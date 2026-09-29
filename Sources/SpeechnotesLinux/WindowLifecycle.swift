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
    static func attach<V: View>(_ view: V) -> some View {
        view.inspectWindow { window in
            state.store(UnsafeMutableRawPointer(window.widgetPointer)
                .assumingMemoryBound(to: GtkWidget.self))

            window.onCloseRequest = { _ in
                if let app = g_application_get_default() {
                    g_application_quit(app)
                }
            }

            if let app = g_application_get_default() {
                connectHandOff(app: UnsafeMutableRawPointer(app))
            }
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

    /// The window widget, guarded for access from plain C signal handlers.
    private final class WidgetSlot: @unchecked Sendable {
        private let lock = NSLock()
        private var widget: UnsafeMutablePointer<GtkWidget>?

        func store(_ pointer: UnsafeMutablePointer<GtkWidget>) {
            lock.lock()
            widget = pointer
            lock.unlock()
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
