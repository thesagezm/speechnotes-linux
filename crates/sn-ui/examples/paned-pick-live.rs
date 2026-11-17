//! Probe: what does a *real* pointer hit on the notes-list rail in the
//! app's own layout?
//!
//! `paned-pick` asks the same question of a plain paned (a Box on either
//! side) and finds a 1 px handle with the button starting right after it.
//! The live app is not that shape: its list pane is a `GtkScrolledWindow`
//! (with an overlay scrollbar) and its rail is a `GtkButton` with
//! `valign(Fill)` as the *first* child of the end child, pressed against
//! the divider. The app's own accessibility tree reports that divider's
//! handle panel nine pixels wide, overlapping the rail's left edge — so
//! this probe rebuilds the live shape and asks GTK's hit test
//! (`gtk_widget_pick`) who owns each pixel across the divider.
//!
//! Run with a display: `cargo run -p sn-ui --example paned-pick-live`.

use gtk4::glib;
use gtk4::prelude::*;

fn describe(w: &gtk4::Widget) -> String {
    format!("{:?}", w.widget_name())
}

fn main() {
    gtk4::init().expect("gtk init");
    let app = gtk4::Application::builder()
        .application_id("org.speechnotes.PanedPickLive")
        .build();
    let app2 = app.clone();
    app.connect_activate(move |_| {
        // The live app's measurements: the notes list is 361 px of the
        // list/editor paned, so the divider lands at 361.
        let window = gtk4::ApplicationWindow::builder()
            .application(&app2)
            .default_width(833)
            .default_height(486)
            .build();

        // The notes list: a scroller with an overlay scrollbar, exactly as
        // the app builds it.
        let list = gtk4::ListBox::new();
        for i in 0..12 {
            let row = gtk4::ListBoxRow::new();
            let label = gtk4::Label::new(Some(&format!("note {i}")));
            row.set_child(Some(&label));
            list.append(&row);
        }
        let list_scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Automatic)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .build();
        list_scroll.set_child(Some(&list));

        // The rail: the app's shape — a flat button, `valign(Fill)`, as the
        // FIRST child of the end child, pressed against the divider.
        let rail = gtk4::Button::from_icon_name("go-next-symbolic");
        rail.set_size_request(20, -1);
        rail.set_valign(gtk4::Align::Fill);
        let editor = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let end_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        end_box.append(&rail);
        end_box.append(&editor);

        let paned = gtk4::Paned::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .position(361)
            .shrink_start_child(true)
            .shrink_end_child(true)
            .build();
        paned.set_start_child(Some(&list_scroll));
        paned.set_end_child(Some(&end_box));
        window.set_child(Some(&paned));
        window.present();

        glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
            println!("--- gtk_widget_pick across the divider ---");
            // The divider is at the paned's position (361); sweep well past
            // it so the whole handle region and the button's start show.
            for x in 345..385 {
                let picked = paned.pick(x as f64, 10.0, gtk4::PickFlags::empty());
                let what = picked.as_ref().map(describe).unwrap_or_else(|| "<none>".into());
                println!("x={x:3} -> {what}");
            }
            std::process::exit(0);
        });
    });
    app.run();
}
