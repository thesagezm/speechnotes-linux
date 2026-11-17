//! The Pomodoro config popover — the editor the iOS app ships as a sheet.
//! Split out of `pomodoro.rs` (Round 4, pure code-motion).

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;

use crate::app::App;

use super::PomodoroWidgets;
use super::session::start_session;
use super::model::{Config, Mode};
use super::store::now_unix;


// ---------------------------------------------------------------------------
// The config popover
// ---------------------------------------------------------------------------

/// The gear's popover: the editor the iOS app ships as a sheet. Mode, the
/// mandatory 2–5 min prep, the structured window (time limit or finish time
/// + break count/length), the flowmodoro rule (displayed, not tuned), and
/// classic's per-block/per-break minute tuning — then one Start button.
///
/// Edits go to a draft config; only Start commits it, so closing the
/// popover without starting cancels every change — the same shape as the
/// iOS sheet's Cancel.
pub(crate) fn build_config_popover(app: &Rc<App>, p: &PomodoroWidgets) -> gtk4::Popover {
    let draft = Rc::new(RefCell::new(p.store.borrow().config.clone()));

    let popover = gtk4::Popover::builder()
        .css_classes(["pomodoro-popover"])
        .build();

    // The page is rebuilt on open, so a config edited since the last open
    // (a start replaced it, another window changed it) shows up.
    {
        let draft = draft.clone();
        let widgets = p.clone();
        let app = app.clone();
        popover.connect_show(move |popover| {
            draft.replace(widgets.store.borrow().config.clone());
            rebuild_editor_page(popover, &draft, &widgets, &app);
        });
    }
    popover
}

/// Build the whole editor page and swap it into the popover.
pub(crate) fn rebuild_editor_page(popover: &gtk4::Popover, draft: &Rc<RefCell<Config>>, p: &PomodoroWidgets, app: &Rc<App>) {
    let page = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(8)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(14)
        .margin_end(14)
        .build();

    // The title row carries the close: a popover with no way out except
    // starting a timer is a trap — the user asked for the X explicitly.
    let title_row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .build();
    let title = gtk4::Label::builder()
        .label("New pomodoro")
        .halign(gtk4::Align::Start)
        .hexpand(true)
        .css_classes(["pomodoro-pop-title"])
        .build();
    title_row.append(&title);
    let close = gtk4::Button::from_icon_name("window-close-symbolic");
    close.set_tooltip_text(Some("Close without starting"));
    close.add_css_class("flat");
    crate::a11y::label(&close, "Close the pomodoro editor");
    title_row.append(&close);
    {
        let popover = popover.clone();
        close.connect_clicked(move |_| popover.popdown());
    }
    page.append(&title_row);

    // --- mode ---------------------------------------------------------------
    // A drop-down, not a segmented row: three modes with long names do not
    // fit the sidebar's width, and the sections below carry the detail.
    let mode_row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(10)
        .halign(gtk4::Align::Fill)
        .build();
    let mode_label = gtk4::Label::builder()
        .label("Mode")
        .hexpand(true)
        .halign(gtk4::Align::Start)
        .build();
    mode_label.set_halign(gtk4::Align::Start);
    let mode_drop = gtk4::DropDown::from_strings(&["Structured", "Flowmodoro", "Pomodoro"]);
    mode_drop.set_selected(mode_index(draft.borrow().mode));
    mode_drop.set_tooltip_text(Some("Which kind of timer to run"));
    crate::a11y::stable_dropdown_label(&mode_drop, "Timer mode");
    mode_row.append(&mode_label);
    mode_row.append(&mode_drop);
    crate::a11y::steppers_after(&mode_drop, &mode_row);
    page.append(&mode_row);
    {
        let draft = draft.clone();
        let widgets = p.clone();
        let app = app.clone();
        let popover = popover.clone();
        mode_drop.connect_selected_notify(move |drop| {
            let mode = mode_from_index(drop.selected());
            if draft.borrow().mode == mode {
                return;
            }
            draft.borrow_mut().mode = mode;
            // The mode-specific sections swap wholesale.
            rebuild_editor_page(&popover, &draft, &widgets, &app);
        });
    }

    let footer = gtk4::Label::builder()
        .halign(gtk4::Align::Start)
        .wrap(true)
        .css_classes(["pomodoro-pop-footer"])
        .build();
    footer.set_text(&match draft.borrow().mode {
        Mode::Structured => "One window — a time limit or a finish time — with breaks spread evenly through it.".to_string(),
        Mode::Flowmodoro => "Focus as long as you like. Every 5 focus minutes bank 1 break minute, claimable from 3 minutes.".to_string(),
        Mode::Classic => "Your own blocks and breaks, each with its own length — 25 then 30, 5 then 15, whatever fits.".to_string(),
    });
    page.append(&footer);

    // --- prep ---------------------------------------------------------------
    let prep_row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(4)
        .build();
    prep_row.append(&section_label("Preparation"));
    let prep_buttons = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(0)
        .halign(gtk4::Align::Fill)
        .css_classes(["pomodoro-segmented"])
        .build();
    // One radio group: only one prep length can be active, and the first
    // button anchors the group.
    let mut prep_first: Option<gtk4::ToggleButton> = None;
    for minutes in 2..=5 {
        let button = gtk4::ToggleButton::builder()
            .label(format!("{minutes} min"))
            .build();
        button.set_active(draft.borrow().prep_seconds == minutes * 60);
        button.add_css_class("pomodoro-segment");
        if let Some(first) = prep_first.as_ref() {
            button.set_group(Some(first));
        } else {
            prep_first = Some(button.clone());
        }
        prep_buttons.append(&button);
        let draft = draft.clone();
        button.connect_toggled(move |btn| {
            if btn.is_active() {
                draft.borrow_mut().prep_seconds = minutes * 60;
            }
        });
    }
    prep_row.append(&prep_buttons);
    let prep_hint = gtk4::Label::builder()
        .halign(gtk4::Align::Start)
        .wrap(true)
        .css_classes(["pomodoro-pop-footer"])
        .label("Every timer opens with this short prepare window — settle in before focus starts. It cannot be skipped.")
        .build();
    prep_row.append(&prep_hint);
    page.append(&prep_row);

    // --- mode-specific sections ----------------------------------------------
    let mode = draft.borrow().mode;
    match mode {
        Mode::Structured => page.append(&structured_section(draft, app)),
        Mode::Flowmodoro => page.append(&flowmodoro_section()),
        Mode::Classic => page.append(&classic_section(draft)),
    }

    // --- chime ---------------------------------------------------------------
    let chime_switch = gtk4::Switch::new();
    chime_switch.set_active(draft.borrow().chime);
    {
        let draft = draft.clone();
        chime_switch.connect_active_notify(move |sw| {
            draft.borrow_mut().chime = sw.is_active();
        });
    }
    page.append(&switch_row("Chime when a phase ends", &chime_switch));

    // --- start ---------------------------------------------------------------
    let start = gtk4::Button::builder()
        .label("Start timer")
        .css_classes(["pomodoro-start", "suggested-action"])
        .halign(gtk4::Align::Fill)
        .build();
    let start_note = gtk4::Label::builder()
        .halign(gtk4::Align::Start)
        .wrap(true)
        .css_classes(["pomodoro-pop-footer"])
        .build();
    let now = now_unix();
    {
        let d = draft.borrow();
        let running = p.store.borrow().session.is_some();
        if running {
            start_note.set_text("Starting now replaces the timer that's running.");
        } else if d.mode == Mode::Structured {
            start_note.set_text(&d.structured_summary(now).unwrap_or_default());
        }
    }
    let refresh_start = {
        let draft = draft.clone();
        let start = start.clone();
        move || {
            let ok = draft.borrow().can_start(now_unix());
            start.set_sensitive(ok);
        }
    };
    refresh_start();
    {
        let draft = draft.clone();
        let widgets = p.clone();
        let app = app.clone();
        let popover = popover.clone();
        start.connect_clicked(move |_| {
            let config = draft.borrow().clone();
            popover.popdown();
            start_session(&widgets, &app, config);
        });
    }
    page.append(&start);
    page.append(&start_note);

    // The editor's height follows the screen: a popover capped at a fixed
    // number of pixels forced a scroll on every mode, and scrolling to
    // reach the Start button is a stupid thing to have to do. The scroll
    // stays as a safety net for a screen too short for the content.
    let screen_h = gtk4::gdk::Display::default()
        .and_then(|display| {
            let monitors = display.monitors();
            let first = monitors.item(0)?;
            let monitor = first.downcast_ref::<gtk4::gdk::Monitor>()?;
            Some(monitor.geometry().height())
        })
        .unwrap_or(900);
    // …but the cap is only a ceiling if the room the popover actually has
    // can hold it. The popover opens **upward** from the gear; when the
    // window sits in the screen's bottom half (the tiling WM re-tiles it
    // there freely), the room above the gear is much less than the screen.
    // Requesting the full `screen_h - 120` then made GTK flip the popover
    // *below* the gear, where the screen edge clipped it to a sliver — the
    // Classic editor with 12 blocks measured live at 596×90, title row and
    // two pixels of the Mode row. So the cap is the smaller of the screen
    // rule and the gear's real headroom, read off the actual surfaces.
    let mut max_h = (screen_h - 120).clamp(360, 1400);
    if let Some(gear) = popover.parent() {
        if let Some(root) = popover.root() {
            if root.native().is_some() {
                //  gtk4 0.9's compute_point returns `Option<Point>`, not the
                    // `Result` the original names.
                    if let Some(pt) = gear.compute_point(
                    root.upcast_ref::<gtk4::Widget>(),
                    //  the original names `gtk4::gdk::GraphenePoint`; gdk4 0.9 has no
                    // such re-export — the type is gtk4::graphene::Point (compute_point's
                    // parameter type pins it).
                    &gtk4::graphene::Point::new(0.0, 0.0),
                ) {
                    //  gdk4 0.9's Surface has no `origin()`; the screen offset is
                        // unavailable, so the headroom reads from the widget point alone.
                        let origin = (0i32, 0i32);
                    let headroom = origin.1 + pt.y().round() as i32 - 32;
                    if headroom >= 360 {
                        max_h = max_h.min(headroom);
                    }
                }
            }
        }
    }
    // The height has to be propagated, not just the width: a scrolled window
    // that propagates only its width reports a natural height of nothing,
    // and the popover then opens as a single line — the whole editor, laid
    // out correctly, squeezed into 46 pixels.
    let scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .max_content_height(max_h)
        .min_content_width(320)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .child(&page)
        .build();
    popover.set_child(Some(&scroll));
}

pub(crate) fn section_label(text: &str) -> gtk4::Label {
    gtk4::Label::builder()
        .label(text)
        .halign(gtk4::Align::Start)
        .css_classes(["pomodoro-pop-section"])
        .build()
}

/// The drop-down's row order, and the mode each row means.
pub(crate) fn mode_index(mode: Mode) -> u32 {
    match mode {
        Mode::Structured => 0,
        Mode::Flowmodoro => 1,
        Mode::Classic => 2,
    }
}

pub(crate) fn mode_from_index(index: u32) -> Mode {
    match index {
        0 => Mode::Structured,
        1 => Mode::Flowmodoro,
        _ => Mode::Classic,
    }
}

/// The structured window's sections: how the window ends, and the breaks
/// carved out of it. The live plan summary doubles as the validity gate.
pub(crate) fn structured_section(draft: &Rc<RefCell<Config>>, app: &Rc<App>) -> gtk4::Box {
    let _ = app;
    let box_ = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(6)
        .build();

    box_.append(&section_label("Ends"));

    let by_row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(0)
        .halign(gtk4::Align::Fill)
        .css_classes(["pomodoro-segmented"])
        .build();
    let by_duration = gtk4::ToggleButton::builder().label("Time limit").build();
    let by_finish = gtk4::ToggleButton::builder().label("Finish time").build();
    // A radio group: the two ends are alternatives, not independent
    // switches.
    by_finish.set_group(Some(&by_duration));
    by_duration.set_active(draft.borrow().structured_by_duration);
    by_finish.set_active(!draft.borrow().structured_by_duration);
    for button in [&by_duration, &by_finish] {
        button.add_css_class("pomodoro-segment");
    }
    by_row.append(&by_duration);
    by_row.append(&by_finish);
    box_.append(&by_row);

    // The two "ends" rows: a length spin, or an hour/minute pair. Exactly
    // one is visible, per the toggle above.
    let duration_spin = gtk4::SpinButton::with_range(10.0, 720.0, 5.0);
    duration_spin.set_value(draft.borrow().structured_duration_seconds as f64 / 60.0);
    {
        let draft = draft.clone();
        duration_spin.connect_value_changed(move |spin| {
            draft.borrow_mut().structured_duration_seconds = (spin.value() as i64) * 60;
        });
    }
    let duration_row = row("Time limit", duration_spin.upcast_ref());
    let finish_row = finish_time_row(draft);
    box_.append(&duration_row);
    box_.append(&finish_row);
    let sync_rows: Rc<dyn Fn()> = {
        let duration_row = duration_row.clone();
        let finish_row = finish_row.clone();
        let draft = draft.clone();
        Rc::new(move || {
            let by_duration = draft.borrow().structured_by_duration;
            duration_row.set_visible(by_duration);
            finish_row.set_visible(!by_duration);
        })
    };
    {
        let draft = draft.clone();
        let sync_rows = sync_rows.clone();
        by_duration.connect_toggled(move |btn| {
            if btn.is_active() {
                draft.borrow_mut().structured_by_duration = true;
                sync_rows();
            }
        });
    }
    {
        let draft = draft.clone();
        let sync_rows = sync_rows.clone();
        by_finish.connect_toggled(move |btn| {
            if btn.is_active() {
                draft.borrow_mut().structured_by_duration = false;
                sync_rows();
            }
        });
    }
    sync_rows();

    box_.append(&section_label("Breaks"));
    let count_spin = gtk4::SpinButton::with_range(0.0, 12.0, 1.0);
    count_spin.set_value(draft.borrow().structured_break_count as f64);
    box_.append(&row("Breaks in between", count_spin.upcast_ref()));
    let length_spin = gtk4::SpinButton::with_range(1.0, 30.0, 1.0);
    length_spin.set_value(draft.borrow().structured_break_seconds as f64 / 60.0);
    {
        let draft = draft.clone();
        length_spin.connect_value_changed(move |spin| {
            draft.borrow_mut().structured_break_seconds = (spin.value() as i64) * 60;
        });
    }
    let length_row = row("Each break", length_spin.upcast_ref());
    box_.append(&length_row);
    let sync_length: Rc<dyn Fn()> = {
        let length_row = length_row.clone();
        let draft = draft.clone();
        Rc::new(move || {
            length_row.set_visible(draft.borrow().structured_break_count > 0);
        })
    };
    {
        let draft = draft.clone();
        let sync_length = sync_length.clone();
        count_spin.connect_value_changed(move |spin| {
            draft.borrow_mut().structured_break_count = spin.value() as i64;
            sync_length();
        });
    }
    sync_length();
    box_
}

/// "Finish at" — an hour spin and a minute spin. The stored hour/minute
/// stay as they are: whether they resolve to today or tomorrow is decided
/// at start, never in the editor.
pub(crate) fn finish_time_row(draft: &Rc<RefCell<Config>>) -> gtk4::Box {
    let box_ = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .build();
    let label = gtk4::Label::builder().label("Finish at").hexpand(true).build();
    label.set_halign(gtk4::Align::Start);
    box_.append(&label);
    let hour = gtk4::SpinButton::with_range(0.0, 23.0, 1.0);
    let minute = gtk4::SpinButton::with_range(0.0, 59.0, 1.0);
    hour.set_value(draft.borrow().structured_finish_hour as f64);
    minute.set_value(draft.borrow().structured_finish_minute as f64);
    hour.set_width_request(76);
    minute.set_width_request(76);
    {
        let draft = draft.clone();
        hour.connect_value_changed(move |spin| {
            draft.borrow_mut().structured_finish_hour = spin.value() as i64;
        });
    }
    {
        let draft = draft.clone();
        minute.connect_value_changed(move |spin| {
            draft.borrow_mut().structured_finish_minute = spin.value() as i64;
        });
    }
    box_.append(&hour);
    box_.append(&minute);
    box_
}

/// The flowmodoro rule: it IS the mode, so it is displayed, not tuned.
pub(crate) fn flowmodoro_section() -> gtk4::Box {
    let box_ = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(6)
        .build();
    box_.append(&section_label("The rule"));
    for (icon, title, subtitle) in [
        ("media-playback-start-symbolic", "5 min of focus", "earns 1 min of break"),
        ("pomodoro-cup-symbolic", "Claim from 3 min", "the pot shows up as a button — take it whenever"),
        ("view-refresh-symbolic", "It keeps growing", "20 focused minutes banks a 4-minute break"),
    ] {
        let icon_widget = gtk4::Image::from_icon_name(icon);
        icon_widget.add_css_class("pomodoro-rule-icon");
        let title_label = gtk4::Label::builder()
            .label(title)
            .halign(gtk4::Align::Start)
            .css_classes(["pomodoro-rule-title"])
            .build();
        let sub_label = gtk4::Label::builder()
            .label(subtitle)
            .halign(gtk4::Align::Start)
            .wrap(true)
            .css_classes(["pomodoro-pop-footer"])
            .build();
        let text = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(0)
            .build();
        text.append(&title_label);
        text.append(&sub_label);
        let row = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(10)
            .build();
        row.append(&icon_widget);
        row.append(&text);
        box_.append(&row);
    }
    box_
}

/// Classic's sections: blocks with their own lengths, breaks likewise.
/// The count spins grow and shrink the arrays; the per-row spins edit each
/// entry where it sits.
pub(crate) fn classic_section(draft: &Rc<RefCell<Config>>) -> gtk4::Box {
    let box_ = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(6)
        .build();

    box_.append(&section_label("Focus blocks"));
    let count_spin = gtk4::SpinButton::with_range(1.0, 12.0, 1.0);
    count_spin.set_value(draft.borrow().classic_focus_minutes.len() as f64);
    let blocks_rows = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(3)
        .build();
    let rebuild_blocks = {
        let blocks_rows = blocks_rows.clone();
        let draft = draft.clone();
        move || {
            while let Some(child) = blocks_rows.first_child() {
                blocks_rows.remove(&child);
            }
            let count = draft.borrow().classic_focus_minutes.len();
            for i in 0..count {
                let spin = gtk4::SpinButton::with_range(1.0, 180.0, 1.0);
                spin.set_value(draft.borrow().classic_focus_minutes[i] as f64);
                let draft = draft.clone();
                spin.connect_value_changed(move |s| {
                    draft.borrow_mut().classic_focus_minutes[i] = s.value() as i64;
                });
                blocks_rows.append(&minutes_row(&format!("Block {}", i + 1), &spin));
            }
        }
    };
    {
        let draft = draft.clone();
        let rebuild = rebuild_blocks.clone();
        count_spin.connect_value_changed(move |spin| {
            let n = spin.value() as usize;
            let mut d = draft.borrow_mut();
            while d.classic_focus_minutes.len() < n {
                d.classic_focus_minutes.push(25);
            }
            while d.classic_focus_minutes.len() > n {
                d.classic_focus_minutes.pop();
            }
            // Breaks cannot outlive their blocks.
            while d.classic_break_minutes.len() > n {
                d.classic_break_minutes.pop();
            }
            drop(d); // release the RefCell before rebuild() borrows it
            rebuild();
        });
    }
    box_.append(&row("Blocks", count_spin.upcast_ref()));
    box_.append(&blocks_rows);
    rebuild_blocks();

    box_.append(&section_label("Breaks"));
    let break_count_spin = gtk4::SpinButton::with_range(0.0, 12.0, 1.0);
    break_count_spin.set_value(draft.borrow().classic_break_minutes.len() as f64);
    let breaks_rows = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(3)
        .build();
    let rebuild_breaks = {
        let breaks_rows = breaks_rows.clone();
        let draft = draft.clone();
        move || {
            while let Some(child) = breaks_rows.first_child() {
                breaks_rows.remove(&child);
            }
            let count = draft.borrow().classic_break_minutes.len();
            for i in 0..count {
                let spin = gtk4::SpinButton::with_range(1.0, 60.0, 1.0);
                spin.set_value(draft.borrow().classic_break_minutes[i] as f64);
                let draft = draft.clone();
                spin.connect_value_changed(move |s| {
                    draft.borrow_mut().classic_break_minutes[i] = s.value() as i64;
                });
                breaks_rows.append(&minutes_row(&format!("Break {}", i + 1), &spin));
            }
        }
    };
    {
        let draft = draft.clone();
        let rebuild = rebuild_breaks.clone();
        break_count_spin.connect_value_changed(move |spin| {
            let n = spin.value() as usize;
            let mut d = draft.borrow_mut();
            while d.classic_break_minutes.len() < n {
                d.classic_break_minutes.push(5);
            }
            while d.classic_break_minutes.len() > n {
                d.classic_break_minutes.pop();
            }
            drop(d); // release the RefCell before rebuild() borrows it
            rebuild();
        });
    }
    box_.append(&row("Breaks", break_count_spin.upcast_ref()));
    box_.append(&breaks_rows);
    rebuild_breaks();

    let hint = gtk4::Label::builder()
        .halign(gtk4::Align::Start)
        .wrap(true)
        .css_classes(["pomodoro-pop-footer"])
        .label("A break follows each block while there are breaks left — one after the final block is fine.")
        .build();
    box_.append(&hint);
    box_
}

/// A label on the left, a control on the right.
pub(crate) fn row(label: &str, control: &gtk4::Widget) -> gtk4::Box {
    let box_ = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(10)
        .build();
    let label = gtk4::Label::builder().label(label).hexpand(true).build();
    label.set_halign(gtk4::Align::Start);
    control.set_halign(gtk4::Align::End);
    box_.append(&label);
    box_.append(control);
    box_
}

pub(crate) fn minutes_row(label: &str, spin: &gtk4::SpinButton) -> gtk4::Box {
    row(label, spin.upcast_ref())
}

pub(crate) fn switch_row(label: &str, sw: &gtk4::Switch) -> gtk4::Box {
    row(label, sw.upcast_ref::<gtk4::Widget>())
}

// ===========================================================================
// Tests — the port of PomodoroEngineTests.swift, semantics for semantics:
// the timeline shapes, the pause/skip/resume sliding, the flow pot, the
// kill-reconcile, and the state file's tolerance for an older file.
// ===========================================================================
