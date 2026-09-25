import CGTK4
import Foundation

/// Minimal direct-GTK4 smoke test: opens a real window through the raw C API.
/// If this window appears, GTK4 wiring works and any failure in the main app
/// is SwiftCrossUI-specific. If it doesn't, the display/session setup is the
/// problem (e.g. this process can't map windows).
///
/// Pointer idiom: GTK typedefs (GtkWindow, GtkBox) import into Swift as types
/// distinct from GtkWidget, so each call reinterprets the same pointer —
/// identical to what GTK's GTK_WINDOW() macro does in C.
private extension UnsafeMutablePointer where Pointee == GtkWidget {
    func asGtkWindow() -> UnsafeMutablePointer<GtkWindow> {
        UnsafeMutableRawPointer(self).assumingMemoryBound(to: GtkWindow.self)
    }

    func asGtkBox() -> UnsafeMutablePointer<GtkBox> {
        UnsafeMutableRawPointer(self).assumingMemoryBound(to: GtkBox.self)
    }
}

guard gtk_init_check() != 0 else {
    FileHandle.standardError.write("gtk_init_check failed - cannot open display\n".data(using: .utf8)!)
    exit(2)
}

let windowWidget = gtk_window_new()!
gtk_window_set_title(windowWidget.asGtkWindow(), "Speechnotes GTK Smoke")
gtk_window_set_default_size(windowWidget.asGtkWindow(), 480, 240)

let box = gtk_box_new(GTK_ORIENTATION_VERTICAL, 12)!
gtk_widget_set_margin_start(box, 24)
gtk_widget_set_margin_end(box, 24)
gtk_widget_set_margin_top(box, 24)
gtk_widget_set_margin_bottom(box, 24)

gtk_box_append(box.asGtkBox(), gtk_label_new("GTK4 raw C API window")!)
gtk_box_append(box.asGtkBox(), gtk_button_new_with_label("Quit")!)

gtk_window_set_child(windowWidget.asGtkWindow(), box)
gtk_window_present(windowWidget.asGtkWindow())

FileHandle.standardError.write("GTK4 window presented; entering main loop\n".data(using: .utf8)!)
// Self-limiting so the smoke test can run unattended.
g_timeout_add(6000, { _ in
    exit(0)
    return 0
}, nil)
// GTK4 removed gtk_main(); use the GLib main loop directly.
let loop = g_main_loop_new(nil, 0)
g_main_loop_run(loop)
