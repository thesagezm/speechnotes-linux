//! The audiobook's position sliders: a drag aims, a rest commits.
//!
//! Both scopes — the chapter fill on the book page and the whole-book fill
//! in the transport bar — are `GtkScale`s now, not `GtkProgressBar`s: a
//! slider the listener can see and grab, with a handle that moves, instead
//! of a display-only line the time label floated above. The seek is still a
//! respawn (the pipe's start offset is fixed at spawn), so a drag commits
//! **one** seek: every move of the hand re-arms one timer, and the target
//! commits when the drag has rested briefly — the same one-seek-per-scrub
//! shape the press/release bars had, without fighting the scale's own drag
//! gesture for the pointer.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;

use crate::app::App;

use super::seek::{seek_in_chapter, seek_target_book, seek_whole};

/// How long a drag must rest before its target commits. A seek respawns the
/// ffmpeg pipe, so the slider waits for the hand to settle rather than
/// seeking on every tick of movement.
const COMMIT_REST_MS: u64 = 350;

/// A position slider with the audiobook's scrub wired in. The widget the
/// containers place is [`ScrubSlider::widget`]; every poll writes through
/// [`ScrubSlider::set_fraction`], which is guarded so a programmatic write
/// can never count as the hand moving.
#[derive(Clone)]
pub struct ScrubSlider {
    state: Rc<State>,
}

struct State {
    app: Rc<App>,
    /// `true` for the transport's whole-book fill, `false` for the book
    /// page's chapter fill — the two scopes the seek functions match.
    whole: bool,
    widget: gtk4::Scale,
    /// True while a programmatic write is in flight: `value_changed` fires
    /// for those too, and only the hand's moves may seek.
    updating: Cell<bool>,
    /// The fraction the poll last wrote. A hand that owns no book (the
    /// transport bar is the note player's display too) snaps back to it.
    shown: Cell<f64>,
    /// The drag's target, committed when the drag rests.
    pending: Cell<f64>,
    /// The one armed commit timer; every move of the hand replaces it.
    commit: std::cell::RefCell<Option<glib::SourceId>>,
}

impl ScrubSlider {
    /// Build the slider. `whole` says which scope it drives: the transport
    /// bar's whole-book fraction (`true`) or the book page's chapter
    /// fraction (`false`) — the same split `seek_whole` / `seek_in_chapter`
    /// serves.
    pub fn new(app: &Rc<App>, whole: bool) -> Self {
        // Three digits of adjustment over 0..1: thousandths of the scope, so
        // the far end of a 22-hour book is still one distinct handle stop.
        let adjustment = gtk4::Adjustment::new(0.0, 0.0, 1.0, 0.005, 0.05, 0.0);
        let widget = gtk4::Scale::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .adjustment(&adjustment)
            .draw_value(false)
            .digits(3)
            .round_digits(3)
            .hexpand(true)
            .valign(gtk4::Align::Center)
            .build();
        let state = Rc::new(State {
            app: app.clone(),
            whole,
            widget: widget.clone(),
            updating: Cell::new(false),
            shown: Cell::new(0.0),
            pending: Cell::new(0.0),
            commit: std::cell::RefCell::new(None),
        });
        {
            let state = state.clone();
            widget.connect_value_changed(move |_| Rc::clone(&state).on_value_changed());
        }
        Self { state }
    }

    /// The widget, for the containers that place it and the CSS classes
    /// that style it.
    pub fn widget(&self) -> &gtk4::Scale {
        &self.state.widget
    }

    /// The position write every poll uses. Guarded, so the `value_changed`
    /// it fires is known to be ours and never seeks.
    pub fn set_fraction(&self, fraction: f64) {
        self.state.write(fraction);
    }

    pub fn add_css_class(&self, class: &str) {
        self.state.widget.add_css_class(class);
    }

    pub fn remove_css_class(&self, class: &str) {
        self.state.widget.remove_css_class(class);
    }

    pub fn has_css_class(&self, class: &str) -> bool {
        self.state.widget.has_css_class(class)
    }
}

impl State {
    /// The guarded position write: the poll's fraction, painted without
    /// counting as a move of the hand.
    fn write(&self, fraction: f64) {
        let fraction = fraction.clamp(0.0, 1.0);
        self.shown.set(fraction);
        self.updating.set(true);
        self.widget.set_value(fraction);
        self.updating.set(false);
    }

    fn on_value_changed(self: &Rc<Self>) {
        if self.updating.get() {
            return;
        }
        // The transport bar is the note player's display too: a hand that
        // owns no book snaps back to the position the poll wrote, the way
        // the old press bars ignored presses they could not serve.
        if seek_target_book(&self.app).is_none() {
            self.write(self.shown.get());
            return;
        }
        self.pending.set(self.widget.value().clamp(0.0, 1.0));
        // The polls hold off while the hand aims — the same flag the press
        // bars set — and the fill brightens while it lasts.
        self.app.abook_scrub.set(true);
        self.widget.add_css_class("scrubbing");
        self.arm_commit();
    }

    /// (Re)arm the one timer that commits the drag: each move pushes it
    /// out, so a continuous drag commits once, at its end.
    fn arm_commit(self: &Rc<Self>) {
        if let Some(old) = self.commit.borrow_mut().take() {
            old.remove();
        }
        let state = Rc::downgrade(self);
        let source = glib::timeout_add_local(Duration::from_millis(COMMIT_REST_MS), move || {
            // A Weak here: the armed timer is stored in the state it
            // closes over, and the loop must not keep a dead slider's
            // state alive until it fires.
            let Some(state) = state.upgrade() else {
                return glib::ControlFlow::Break;
            };
            *state.commit.borrow_mut() = None;
            state.commit_now();
            glib::ControlFlow::Break
        });
        *self.commit.borrow_mut() = Some(source);
    }

    /// The drag's one seek: the shared tail `seek_to` runs, respawn and all,
    /// after which the polls repaint the new position.
    fn commit_now(&self) {
        self.app.abook_scrub.set(false);
        self.widget.remove_css_class("scrubbing");
        let fraction = self.pending.get();
        if self.whole {
            seek_whole(&self.app, fraction);
        } else {
            seek_in_chapter(&self.app, fraction);
        }
    }
}
