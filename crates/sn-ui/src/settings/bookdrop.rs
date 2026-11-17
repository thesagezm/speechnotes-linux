//! Split out of settings.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;

use crate::app::App;
use super::helpers::*;

// ---------------------------------------------------------------------------
// BookDrop
// ---------------------------------------------------------------------------

/// The BookDrop page: the LAN receiver's switch, its access code, and the
/// transfers that landed.
///
/// The poll timer is what makes this page live: it reads the service's
/// status and drains its history, so a transfer that lands while the page
/// is open appears in the list without the user doing anything.
pub(crate) fn bookdrop_page(
    app: &Rc<App>,
    window: &gtk::Window,
    page: &gtk::Box,
) -> Option<gtk::glib::SourceId> {
    let prefs = app.prefs.get();
    let service = app.bookdrop.clone();

    section(page, "Receive");
    let intro = gtk::Label::builder()
        .label("Receive books, notes and audiobooks sent from another device on this network. The sender finds this machine by itself — no address to type.")
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    page.append(&intro);

    let enabled_row = toggle_row(
        "Accept files sent over the local network",
        prefs.book_drop_enabled,
    );
    page.append(&enabled_row.0);

    let status = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .build();
    status.add_css_class("row-meta");
    page.append(&status);

    let port_row = row("Port");
    let port_text = service.port().to_string();
    let port_label = gtk::Label::new(Some(&port_text));
    port_row.append(&port_label);
    page.append(&port_row);

    let code_row = row("Access code");
    let code = gtk::Entry::new();
    code.set_placeholder_text(Some("none"));
    code.set_text(&prefs.book_drop_pin);
    code.set_hexpand(true);
    code_row.append(&code);
    page.append(&code_row);

    let auto_row = toggle_row(
        "Accept transfers automatically",
        prefs.book_drop_auto_accept,
    );
    page.append(&auto_row.0);

    // User-reported (2026-10-30): the send half. The worker
    // plumbing (`sn-ui::bookdrop::scan_for_peers` / `send_to_peer`) existed
    // with no UI calling it — receive worked, but there was no way to send,
    // and nothing showed an outbound transfer's progress. The same access
    // code guards both directions.
    //
    // User-reported redesign (2026-11-13): the send flow was TWO
    // stacked dialogs — pick files, then a peer chooser that owned the scan
    // — and the user was right that neither level belonged in a window:
    // "the search for peers should be within the bookdrop subsetting". The
    // devices now live ON this page, with the manual **Find devices**
    // rescan beside them (the user's explicit ask — a button they can press
    // whenever, not a one-shot sweep hidden inside a popup), and the file
    // picker is the only popup left. The typed-address box stays, small and
    // clearly secondary: a receiver on another subnet is real, but it is
    // the exception, not the flow.
    section(page, "Send");
    let send_button = gtk::Button::with_label("Send files to a nearby device…");
    page.append(&send_button);

    let devices_row = row("Nearby devices");
    let find_button = gtk::Button::with_label("Find devices");
    find_button.set_tooltip_text(Some("Search the network again for BookDrop devices"));
    devices_row.append(&find_button);
    page.append(&devices_row);

    let peer_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        // GTK4's own boxed-list cards; no new CSS needed.
        .css_classes(["boxed-list"])
        .build();
    let peer_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(84)
        .max_content_height(200)
        .propagate_natural_height(true)
        .child(&peer_list)
        .build();
    page.append(&peer_scroll);

    let peer_status = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .css_classes(["row-meta"])
        .build();
    page.append(&peer_status);

    let addr_row = row("Device not listed?");
    let address = gtk::Entry::builder()
        .placeholder_text("192.168.1.50  or  192.168.1.50:53317")
        .hexpand(true)
        .build();
    // The placeholder is not an accessible name — a driver (and a screen
    // reader) finds this box by the name, the same rule the peer dialog
    // this row replaces followed.
    address.update_property(&[gtk::accessible::Property::Label("Peer address")]);
    let connect_button = gtk::Button::with_label("Connect");
    addr_row.append(&address);
    addr_row.append(&connect_button);
    page.append(&addr_row);

    let send_status = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    send_status.add_css_class("row-meta");
    page.append(&send_status);
    let send_bar = gtk::ProgressBar::builder()
        .show_text(true)
        .text("")
        .hexpand(true)
        .build();
    page.append(&send_bar);

    // The peers behind the list's rows: the row's index is the handle.
    let peers: std::rc::Rc<std::cell::RefCell<Vec<sn_net::sender::Peer>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    // Files picked but not yet aimed at a device: the picker is the one
    // popup, so a send can start from either end — files first (the Send
    // button, then a device) or a device first (its row, then the picker).
    let pending: std::rc::Rc<std::cell::RefCell<Option<Vec<sn_net::sender::SendItem>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    // Each scan's poll timer carries the generation it was born with; a
    // newer sweep invalidates the older timer's writes instead of letting
    // two sweeps race rows into the list.
    let scan_gen: std::rc::Rc<std::cell::Cell<u64>> =
        std::rc::Rc::new(std::cell::Cell::new(0));

    // Append one peer to the list. Shared by the scan, the rescan and the
    // typed address, so all three arrive the same way.
    let add_peer: std::rc::Rc<dyn Fn(&sn_net::sender::Peer) -> gtk::ListBoxRow> = {
        let peer_list = peer_list.clone();
        let peers = peers.clone();
        std::rc::Rc::new(move |peer: &sn_net::sender::Peer| -> gtk::ListBoxRow {
            let row = gtk::ListBoxRow::new();
            let column = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(1)
                .margin_top(5)
                .margin_bottom(5)
                .margin_start(6)
                .margin_end(6)
                .build();
            column.append(
                &gtk::Label::builder()
                    .label(peer.label())
                    .halign(gtk::Align::Start)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .single_line_mode(true)
                    .build(),
            );
            column.append(
                &gtk::Label::builder()
                    .label(format!("{}:{}", peer.host, peer.port))
                    .halign(gtk::Align::Start)
                    .css_classes(["row-meta"])
                    .build(),
            );
            row.set_child(Some(&column));
            peers.borrow_mut().push(peer.clone());
            peer_list.append(&row);
            row
        })
    };

    // The selected row's peer, if the list has a selection and the list's
    // own vector agrees on the index.
    let selected_peer: std::rc::Rc<dyn Fn() -> Option<sn_net::sender::Peer>> = {
        let peer_list = peer_list.clone();
        let peers = peers.clone();
        std::rc::Rc::new(move || {
            let index = peer_list.selected_row()?.index();
            peers.borrow().get(index.max(0) as usize).cloned()
        })
    };

    // ---- the sweep, and the manual rescan --------------------------------
    let run_scan: std::rc::Rc<dyn Fn()> = {
        let app = app.clone();
        let peer_list = peer_list.clone();
        let peers = peers.clone();
        let peer_status = peer_status.clone();
        let add_peer = add_peer.clone();
        let scan_gen = scan_gen.clone();
        std::rc::Rc::new(move || {
            let peer_list = peer_list.clone();
            let peers = peers.clone();
            let peer_status = peer_status.clone();
            let add_peer = add_peer.clone();
            let scan_gen = scan_gen.clone();
            // The old answers are cleared: a peer that left the network
            // must not stay selectable, and a stale row sends into a void.
            while let Some(child) = peer_list.first_child() {
                peer_list.remove(&child);
            }
            peers.borrow_mut().clear();
            let generation = scan_gen.get() + 1;
            scan_gen.set(generation);
            peer_status.set_text("Looking for nearby devices…");
            let rx = crate::bookdrop::scan_for_peers(app.bookdrop.fingerprint());
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                if scan_gen.get() != generation {
                    return gtk::glib::ControlFlow::Break;
                }
                match rx.try_recv() {
                    Ok(found) => {
                        let mut added = 0usize;
                        for peer in &found {
                            add_peer(peer);
                            added += 1;
                        }
                        peer_status.set_text(&if added == 0 {
                            "No device answered the sweep. Start BookDrop on the other device, or type its address below."
                                .to_string()
                        } else {
                            format!("{added} device(s) found. Double-click one to send, or select it and press Send.")
                        });
                        gtk::glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        peer_status.set_text("The sweep ended without an answer.");
                        gtk::glib::ControlFlow::Break
                    }
                }
            });
        })
    };

    // ---- the typed address (the secondary way in) ------------------------
    let run_connect: std::rc::Rc<dyn Fn(String)> = {
        let peer_status = peer_status.clone();
        let add_peer = add_peer.clone();
        let peer_list = peer_list.clone();
        let app = app.clone();
        std::rc::Rc::new(move |address: String| {
            let peer_status = peer_status.clone();
            let add_peer = add_peer.clone();
            let peer_list = peer_list.clone();
            if address.trim().is_empty() {
                peer_status.set_text("Type the device's address first (192.168.1.50).");
                return;
            }
            peer_status.set_text(&format!("Looking for {address}…"));
            let rx =
                crate::bookdrop::resolve_manual_peer(address.clone(), app.bookdrop.fingerprint());
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
                match rx.try_recv() {
                    Ok(Ok(peer)) => {
                        let label = peer.label();
                        let row = add_peer(&peer);
                        peer_list.select_row(Some(&row));
                        peer_status.set_text(&format!("Added {label}."));
                        gtk::glib::ControlFlow::Break
                    }
                    Ok(Err(why)) => {
                        peer_status.set_text(&format!("Cannot use that address: {why}"));
                        gtk::glib::ControlFlow::Break
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        peer_status.set_text("The lookup ended without an answer.");
                        gtk::glib::ControlFlow::Break
                    }
                }
            });
        })
    };

    {
        let run_scan = run_scan.clone();
        find_button.connect_clicked(move |_| run_scan());
    }
    {
        let run_connect = run_connect.clone();
        let address = address.clone();
        connect_button.connect_clicked(move |_| {
            run_connect(address.text().to_string());
        });
    }
    {
        // Enter in the address box means Connect, the way a user reads it.
        let run_connect = run_connect.clone();
        address.connect_activate(move |entry| {
            run_connect(entry.text().to_string());
        });
    }

    // A sweep on open, so the page answers "who is out there?" without a
    // click. Find devices re-runs it whenever the user wants a fresh one.
    run_scan();

    // ---- aiming a send at a device ---------------------------------------
    // Double-click (or Enter) on a device row sends: the picked files when
    // they are waiting, otherwise the file picker opens for that device.
    {
        let app = app.clone();
        let pending = pending.clone();
        let send_status = send_status.clone();
        peer_list.connect_row_activated(move |_, row| {
            let index = row.index().max(0) as usize;
            let Some(peer) = peers.borrow().get(index).cloned() else {
                return;
            };
            let already = pending.borrow_mut().take();
            if let Some(items) = already {
                let pin = app.prefs.get().book_drop_pin;
                app.bookdrop.start_send(items, &peer, &pin);
                send_status.set_text(&format!("Sending to {}…", peer.label()));
                return;
            }
            crate::dialogs::pick_bookdrop_items(&app, {
                let app = app.clone();
                let send_status = send_status.clone();
                move |items| {
                    let pin = app.prefs.get().book_drop_pin;
                    app.bookdrop.start_send(items, &peer, &pin);
                    send_status.set_text(&format!("Sending to {}…", peer.label()));
                }
            });
        });
    }
    {
        let app = app.clone();
        let pending = pending.clone();
        let selected_peer = selected_peer.clone();
        let send_status = send_status.clone();
        send_button.connect_clicked(move |_| {
            let pending = pending.clone();
            let send_status = send_status.clone();
            let selected = selected_peer();
            // The picker resolves the ticked rows into files (nothing
            // outside the app's own data is offered); on OK the send aims
            // at the selected device, or waits below for one.
            crate::dialogs::pick_bookdrop_items(&app, {
                let app = app.clone();
                move |items| {
                    if let Some(peer) = selected.clone() {
                        let pin = app.prefs.get().book_drop_pin;
                        app.bookdrop.start_send(items, &peer, &pin);
                        send_status.set_text(&format!("Sending to {}…", peer.label()));
                    } else {
                        *pending.borrow_mut() = Some(items);
                        send_status.set_text(
                            "Files ready — double-click a device below to send them.",
                        );
                    }
                }
            });
        });
    }

    // ---- the switch ------------------------------------------------------
    {
        let app = app.clone();
        let service = service.clone();
        enabled_row.1.connect_toggled(move |check| {
            let value = check.is_active();
            app.prefs.update(|p| p.book_drop_enabled = value);
            if value {
                //  the original bookdrop Service::start() takes no port (the
                    // listener binds its own); the older call shape passed it.
                    if let Err(e) = service.start() {
                    tracing::warn!(error = %e, "bookdrop could not start");
                    crate::window::report_failure(
                        &app,
                        &format!("BookDrop could not start: {e}"),
                    );
                }
            } else {
                service.stop();
            }
        });
    }
    {
        let app = app.clone();
        code.connect_changed(move |entry| {
            let text = entry.text().to_string();
            app.prefs.update(|p| p.book_drop_pin = text);
        });
    }
    {
        let app = app.clone();
        auto_row.1.connect_toggled(move |check| {
            let value = check.is_active();
            app.prefs.update(|p| p.book_drop_auto_accept = value);
        });
    }

    section(page, "Recent transfers");
    let history = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    page.append(&history);
    let empty = gtk::Label::builder()
        .label("Nothing has arrived yet.")
        .halign(gtk::Align::Start)
        .build();
    empty.add_css_class("row-meta");
    history.append(&empty);

    // ---- the poll --------------------------------------------------------
    // One timer, started with the page and removed when the page is left.
    // It reads the status line, the progress bar and the history list.
    let status = status.clone();
    let port_label = port_label.clone();
    let history = history.clone();
    let empty = empty.clone();
    let service_for_tick = service.clone();
    let window = window.clone();
    // The send half's tick state ( 2026-10-30, beta).
    let send_status = send_status.clone();
    let send_bar = send_bar.clone();
    //  the original bookdrop attach_poll takes only the tick closure
    // (the app is captured here); the older call shape passed it too.
    let source = crate::bookdrop::attach_poll(move || {
        // The status line: what the receiver is doing, in words.
        let text = if service_for_tick.is_running() {
            match service_for_tick.progress() {
                Some((landed, total)) if total > 0 => {
                    format!("Receiving — {landed} of {total} file(s) landed")
                }
                _ => "Visible on this network".to_string(),
            }
        } else if let Some(error) = service_for_tick.last_error() {
            format!("Unavailable — {error}")
        } else {
            "Off".to_string()
        };
        status.set_text(&text);
        port_label.set_text(&service_for_tick.port().to_string());

        // The history: newest first, each row saying where it went.
        let records = service_for_tick.drain_events();
        if !records.is_empty() {
            if empty.parent().is_some() {
                empty.unparent();
            }
            for record in records {
                let outcome = match &record.outcome {
                    sn_net::bookdrop::TransferOutcome::Imported(where_) => {
                        format!("landed in {where_}")
                    }
                    sn_net::bookdrop::TransferOutcome::Failed(why) => format!("failed: {why}"),
                    sn_net::bookdrop::TransferOutcome::Rejected => "rejected".to_string(),
                };
                let line = gtk::Label::builder()
                    .label(format!("{} — {outcome}", record.name))
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .build();
                history.append(&line);
            }
        }

        // The send half: the progress line and bar follow the live send
        // state. The devices are on the page now, so there is no chooser
        // line to keep alive — the tick reports only the send itself.
        let progress = service_for_tick.send_progress();
        if progress.active {
            send_status.set_text(&progress.line);
            send_bar.set_fraction(progress.fraction.unwrap_or(0.0));
            send_bar.set_text(Some("Sending…"));
        } else if let Some(done) = &progress.done {
            match done {
                Ok(summary) => {
                    send_status.set_text(summary);
                    send_bar.set_fraction(1.0);
                    send_bar.set_text(Some("Done"));
                }
                Err(why) => {
                    send_status.set_text(&format!("Send failed: {why}"));
                    send_bar.set_text(Some("Failed"));
                }
            }
        } else {
            send_status.set_text("");
            send_bar.set_fraction(0.0);
            send_bar.set_text(Some(""));
        }
        let _ = &window;
    });
    Some(source)
}
