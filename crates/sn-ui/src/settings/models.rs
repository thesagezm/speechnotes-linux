//! Split out of settings.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;

use crate::app::App;
use super::helpers::*;

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// The neural engines and their model files: what is installed, how big it
/// is, and how to get or remove it.
///
/// The download runs on its own thread — it is hundreds of megabytes over a
/// network — and the page's poll timer reads the manager's progress.
pub(crate) fn models_page(app: &Rc<App>, page: &gtk::Box) {
    section(page, "Speech models");
    let intro = gtk::Label::builder()
        .label("The neural engines speak better than espeak, and RapidOCR reads the scanned pages no text layer can give the book reader. espeak is built in and always available.")
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    page.append(&intro);

    //  catalog() is the speech list; the page recogniser installs from
    // this page too (the shipped binary rows it as "RapidOCR — page
    // recognition for scanned books" beside the voice sets), so its set is
    // chained on here and its row labelled separately below.
    for set in sn_tts::manager::catalog()
        .into_iter()
        .chain(sn_tts::manager::ModelManager::set_for("ocr").cloned())
    {
        let engine = set.engine;
        let state = sn_tts::manager::ModelManager::state(engine);
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .build();
        let title = if engine == "ocr" {
            // The voice rows keep the size line; the recogniser keeps a plain label.
            "RapidOCR — page recognition for scanned books".to_string()
        } else {
            format!("{} — {:.0} MB", engine, set.approx_bytes() as f64 / 1_000_000.0)
        };
        text.append(&gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .build());
        let status = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .build();
        let installed = matches!(state, sn_tts::manager::InstallState::Installed);
        status.set_text(if installed {
            "installed"
        } else {
            "not installed"
        });
        status.add_css_class("row-meta");
        text.append(&status);
        row.append(&text);

        let button = gtk::Button::with_label(if installed { "Remove" } else { "Download" });
        {
            let app = app.clone();
            // Two clones: one is borrowed by the call, the other is moved
            // into the closure (a GTK widget is a handle, so a clone is
            // cheap — it is a refcount, not a copy).
            let button_for_call = button.clone();
            let button_in_click = button.clone();
            button_for_call.connect_clicked(move |_| {
                let app = app.clone();
                let button = button_in_click.clone();
                let engine = engine;
                // The transfer is long: the button disables itself and the
                // progress label under it moves. The work happens on a
                // worker thread — a GTK widget cannot cross into one, so
                // the thread sends the outcome over a channel and the UI
                // thread picks it up through an idle callback.
                button.set_sensitive(false);
                button.set_label(if installed { "Removing…" } else { "Downloading…" });
                let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
                let models = app.models.clone();
                std::thread::spawn(move || {
                    let mut manager = models.lock().unwrap_or_else(|e| e.into_inner());
                    let outcome = if installed {
                        manager.delete(engine).map_err(|e| e.to_string())
                    } else {
                        let mut progress = |_f: f64| {};
                        manager.install(engine, &mut progress).map_err(|e| e.to_string())
                    };
                    let _ = tx.send(outcome);
                });
                // The idle callback owns its own copies: the click
                // closure is `Fn`, so it cannot give away what it captured.
                //
                //  the original single `idle_add_local_once` +
                // `try_recv` fired on the *first* idle tick — long before a
                // multi-second transfer finished — so every install
                // reported "the worker stopped: receiving on an empty
                // channel" and re-enabled the button mid-download
                // (measured 2026-10-30: a kokoro install failed this way
                // with the worker still running). Poll on
                // a repeat timer instead: an empty channel keeps waiting,
                // the worker's message ends it, and a disconnected sender
                // (worker panicked) reports the failure.
                let button_for_callback = button.clone();
                let app_for_callback = app.clone();
                let rx = std::cell::RefCell::new(rx);
                gtk4::glib::timeout_add_local(
                    std::time::Duration::from_millis(150),
                    move || {
                        let outcome = rx.borrow_mut().try_recv();
                        match outcome {
                            Err(std::sync::mpsc::TryRecvError::Empty) => {
                                return gtk4::glib::ControlFlow::Continue;
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                let why = "the worker stopped before reporting".to_string();
                                tracing::warn!(engine, error = %why, "model install/delete failed");
                                crate::window::report_failure(
                                    &app_for_callback,
                                    &format!("The model operation failed: {why}"),
                                );
                            }
                            Ok(Err(why)) => {
                                tracing::warn!(engine, error = %why, "model install/delete failed");
                                crate::window::report_failure(
                                    &app_for_callback,
                                    &format!("The model operation failed: {why}"),
                                );
                            }
                            Ok(Ok(())) => {}
                        }
                        button_for_callback.set_sensitive(true);
                        button_for_callback
                            .set_label(if installed { "Remove" } else { "Download" });
                        gtk4::glib::ControlFlow::Break
                    },
                );
            });
        }
        row.append(&button);
        page.append(&row);
    }
}
