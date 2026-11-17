//! Probe: does a GtkPaned's resize handle cover the first pixels of its end
//! child? A rail button placed as the end child's first child would then
//! have a dead strip on its left.
//!
//! Run with a display: `cargo run -p sn-ui --example paned-pick`.

use gtk4::glib;
use gtk4::prelude::*;

fn describe(w: &gtk4::Widget) -> String {
    let a = w.allocation();
    format!("{:?} alloc_x={} alloc_w={}", w.widget_name(), a.x(), a.width())
}

fn main() {
    gtk4::init().expect("gtk init");
    let app = gtk4::Application::builder()
        .application_id("org.speechnotes.PanedPick")
        .build();
    let app2 = app.clone();
    app.connect_activate(move |_| {
        let window = gtk4::ApplicationWindow::builder()
            .application(&app2)
            .default_width(600)
            .default_height(200)
            .build();

        // The shape the app uses: the rail button is the FIRST child of the
        // paned's end child, so it sits right against the divider.
        let rail = gtk4::Button::from_icon_name("go-next-symbolic");
        rail.set_size_request(20, -1);
        let end_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        end_box.append(&rail);
        end_box.append(&gtk4::Label::new(Some("editor")));
        let start_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        start_box.append(&gtk4::Label::new(Some("notes list")));
        let paned = gtk4::Paned::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .position(200)
            .shrink_start_child(true)
            .shrink_end_child(true)
            .build();
        paned.set_start_child(Some(&start_box));
        paned.set_end_child(Some(&end_box));
        window.set_child(Some(&paned));
        window.present();

        glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
            let paned_alloc = paned.allocation();
            let rail_alloc = rail.allocation();
            println!("paned alloc: {}x{}", paned_alloc.width(), paned_alloc.height());
            println!(
                "rail alloc: x={} w={} (relative to the paned)",
                rail_alloc.x(),
                rail_alloc.width()
            );
            let end_alloc = end_box.allocation();
            println!(
                "end_box alloc: x={} w={} (relative to the paned)",
                end_alloc.x(),
                end_alloc.width()
            );
            println!("--- gtk_widget_pick across the divider ---");
            for x in 194..226 {
                let picked = paned.pick(x as f64, 10.0, gtk4::PickFlags::empty());
                let what = picked.as_ref().map(describe).unwrap_or_else(|| "<none>".into());
                println!("x={x:3} -> {what}");
            }
            std::process::exit(0);
        });
    });
    app.run();
}
