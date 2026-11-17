//! The transport bar's audiobook mode and idle state: who is speaking, the
//! speed display, the play/pause glyph. Split out of `player.rs` (Round 4,
//! pure code-motion).

use std::rc::Rc;

use gtk4::prelude::*;

use crate::app::App;

use super::highlight::{clear_highlight, update_widgets};

pub fn describe_audiobook(
    app: &Rc<App>,
    title: &str,
    chapter: &str,
    elapsed: &str,
    length: &str,
    fraction: f64,
) {
    let Some(widgets) = app.widgets.borrow().clone() else {
        return;
    };
    // The bar's visibility is focus mode's to call: the mode collapsed it,
    // and a tick describing the book must not bring it back on its own.
    widgets
        .player
        .bar
        .set_visible(!app.focus_mode.get());
    widgets.player.status.set_text(&format!("{title} — {chapter}"));
    widgets
        .player
        .position
        .set_text(&format!("{elapsed} / {length}"));
    // A scrub gesture holds the fill: the drag paints its own target, and
    // the tick's position — the audio's, not the drag's — would fight it.
    if !app.abook_scrub.get() {
        widgets.player.progress.set_fraction(fraction.clamp(0.0, 1.0));
    }
}

/// The speed spin shows the live session's rate, which for an audiobook is
/// its own pref rather than the note player's.
///
/// The last value shown is remembered, so the `value_changed` handler this
/// triggers can tell it apart from the listener turning the dial.
pub fn show_audiobook_speed(app: &Rc<App>) {
    let rate = app.prefs.get().audio_rate;
    let Some(widgets) = app.widgets.borrow().clone() else {
        return;
    };
    widgets.player.speed.set_value(rate);
    app.shown_speed.set(rate);
}

/// The rate the spin is currently displaying.
pub(crate) fn shown_rate(app: &Rc<App>) -> f64 {
    app.shown_speed.get()
}

/// The play button's icon follows the audiobook's paused state.
pub fn set_audiobook_play_icon(app: &Rc<App>, paused: bool) {
    set_play_icon(app, !paused);
}

/// Redraw the transport bar from the note player's own state, for when an
/// audiobook stops and the bar goes back to describing notes.
pub fn refresh_transport(app: &Rc<App>) {
    let Some(session) = app.tts.borrow().clone() else {
        if let Some(widgets) = app.widgets.borrow().clone() {
            widgets.player.status.set_text("");
            widgets.player.position.set_text("");
            widgets.player.progress.set_fraction(0.0);
        }
        return;
    };
    update_widgets(app, &session.progress());
}

pub(crate) fn show_bar(app: &Rc<App>) {
    if let Some(widgets) = app.widgets.borrow().clone() {
        // Focus mode collapsed the bar; a session starting under it stays
        // hidden until the mode leaves.
        widgets.player.bar.set_visible(!app.focus_mode.get());
    }
}

pub(crate) fn set_play_icon(app: &Rc<App>, playing: bool) {
    if let Some(widgets) = app.widgets.borrow().clone() {
        if playing {
            widgets.player.play.set_pause();
        } else {
            widgets.player.play.set_play();
        }
    }
}

pub(crate) fn set_status(app: &Rc<App>, text: &str) {
    if let Some(widgets) = app.widgets.borrow().clone() {
        widgets.player.status.set_text(text);
    }
}

/// Disconnect the tick and drop the session. The bar stays up with its final
/// status; the next play rebuilds everything.
pub(crate) fn detach(app: &Rc<App>) {
    if let Some(source) = app.tts_tick.borrow_mut().take() {
        source.remove();
    }
    clear_highlight(app);
    set_play_icon(app, false);
    // The voice no longer owns the reader's page: the reader's own scrolling
    // moves the manifest's position again.
    crate::reader::set_speaking(app, false);
    *app.tts.borrow_mut() = None;
}
