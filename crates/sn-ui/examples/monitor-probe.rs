//! Probe: what monitors does GTK see, under the backend the environment
//! picks? Standalone — prints the geometry and exits. Used to verify the
//! two-screen presentation work (the presenter's "other screen" logic).

use gtk4::prelude::*;

fn main() {
    let app = gtk4::Application::builder().application_id("org.sn.monitorprobe").build();
    app.connect_activate(|_app| {
        let d = gtk4::gdk::Display::default().expect("no display");
        let monitors = d.monitors();
        // The GLib type name says which backend landed ("GdkWaylandDisplay" /
        // "GdkX11Display") without needing the crate's backend features.
        let backend = d.type_().name().to_string();
        println!("backend={} monitors={}", backend, monitors.n_items());
        for i in 0..monitors.n_items() {
            let m = monitors.item(i).and_downcast::<gtk4::gdk::Monitor>().unwrap();
            let g = m.geometry();
            println!(
                "  monitor {i}: model={} {}x{}+{}+{} scale={}",
                m.model().unwrap_or_default(),
                g.width(),
                g.height(),
                g.x(),
                g.y(),
                m.scale_factor(),
            );
        }
        _app.quit();
    });
    app.run();
}
