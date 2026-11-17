//! Motion: the small, cheap animations the app spends its life with.
//!
//! **Why a module.** Three places want the same two things — a value that
//! eases from where it is to where it is going, and a pair of widgets that
//! trade places without a frame of flicker. Each was going to be written
//! once and copied; this is the one copy.
//!
//! **Why not `PropertyAnimation`.** GTK4 0.9 (the version this app builds
//! against) has no `gtk::PropertyAnimation` — that arrived in GTK 4.16, and
//! this box's `gtk4` crate predates it by seven minor versions. The
//! alternatives are all worse here:
//!
//! * a CSS transition cannot animate a widget's *position* (only its
//!   colour, opacity, border and size), so the fill-the-window slide cannot
//!   be CSS;
//! * `gtk::Animation` is deprecated and does the same job with more
//!   ceremony;
//! * driving the value by hand is two dozen lines, has no dependency, and
//!   runs on the main context's idle — which is exactly where a 220 ms
//!   slide belongs.
//!
//! So the easing is hand-rolled, and it is deliberately *cheap*: one
//! `timeout_add_local` at roughly 60 Hz for the duration of the animation,
//! nothing at all once it ends. No widget is redrawn more often than the
//! window is actually visible, and every animation here stops the instant it
//! is superseded.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::glib;
use gtk4::prelude::*;

/// How long a fill-the-window slide takes, in milliseconds.
///
/// Long enough to read as motion rather than a jump, short enough that the
/// reader is not waiting on it. The plan calls for ~220 ms.
pub const FOCUS_MS: u32 = 220;

/// How long a page crossfade takes, in milliefseconds.
pub const FADE_MS: u32 = 240;

/// How long the play/pause glyph takes to change, in milliseconds. Shorter
/// than the others on purpose: a button that is being pressed should answer
/// immediately, and a slow glyph makes the press feel unacknowledged.
pub const GLYPH_MS: u32 = 150;

/// The easing curve: `ease-out-cubic`.
///
/// Motion that starts fast and settles slowly reads as *arriving*; motion
/// that starts slow and ends fast reads as being yanked. A UI that is
/// getting out of the way should feel like the first kind.
fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// The easing curve for a value that has to *leave* the screen: `ease-in-out`.
///
/// A slide to zero and a slide back are mirror images, and a reader who
/// toggles the mode twice in a second should not see the page lurch. This
/// curve starts and ends slowly, which reads as a deliberate movement
/// rather than a snap.
fn ease_in_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}

/// The ticks per second an animation runs at. 60 Hz is the display's rate;
/// 30 Hz is plenty for a 220 ms slide and halves the work.
const TICK_HZ: u32 = 60;

/// A running animation, so a new one can replace the old.
///
/// One per kind of thing that animates — the focus slide, the play glyph —
/// so that starting a new animation cancels the one in flight rather than
/// letting two drive the same property.
///
/// The timer handle lives behind an `Rc` because `glib::SourceId` is not
/// `Clone` and the animation's callback needs to clear the handle when it
/// finishes.
#[derive(Clone)]
pub struct Anim {
    source: Rc<RefCell<Option<glib::SourceId>>>,
}

impl Anim {
    /// A new, idle animation.
    pub fn new() -> Self {
        Self {
            source: Rc::new(RefCell::new(None)),
        }
    }

    /// Stop whatever is running, and leave the value where it is.
    ///
    /// Called when an animation is superseded: the new one starts from the
    /// current value, so the handover is seamless rather than a jump back to
    /// the beginning.
    pub fn stop(&self) {
        if let Some(source) = self.source.borrow_mut().take() {
            source.remove();
        }
    }

    /// True while an animation is in flight.
    pub fn running(&self) -> bool {
        self.source.borrow().is_some()
    }

    /// Run `step(t)` for `duration`, from `t = 0` to `t = 1`.
    ///
    /// `step` is called on the main context, so it may touch widgets. The
    /// final call is at exactly `t = 1`, so the end state is always reached
    /// even if the clock is coarse — no animation ends a frame short.
    pub fn run<F>(&self, duration_ms: u32, step: F)
    where
        F: FnMut(f64) + 'static,
    {
        self.run_eased(duration_ms, ease_out_cubic, step);
    }

    /// Run `step(t)` for `duration`, with the easing curve of the caller's
    /// choosing.
    ///
    /// The raw `t` is passed to `ease` first, so `step` always receives a
    /// value in `0.0..=1.0`.
    pub fn run_eased<F, E>(&self, duration_ms: u32, ease: E, mut step: F)
    where
        F: FnMut(f64) + 'static,
        E: Fn(f64) -> f64 + 'static,
    {
        self.stop();
        let start = Instant::now();
        let duration = Duration::from_millis(duration_ms as u64);
        let period = Duration::from_millis((1000 / TICK_HZ) as u64);
        let source_handle = Rc::clone(&self.source);
        let source = glib::timeout_add_local(period, move || {
            let elapsed = start.elapsed();
            let t = if elapsed >= duration {
                1.0
            } else {
                elapsed.as_secs_f64() / duration.as_secs_f64()
            };
            step(ease(t));
            if t >= 1.0 {
                *source_handle.borrow_mut() = None;
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        *self.source.borrow_mut() = Some(source);
    }
}

impl Default for Anim {
    fn default() -> Self {
        Self::new()
    }
}

/// A button whose glyph changes between two icons, crossfading.
///
/// **Why.** A button that swaps its icon in one frame *pops* — the old glyph
/// vanishes and the new one appears, and at 150 ms that is the difference
/// between a control that answers you and one that blinks. Two stacked
/// images, the outgoing one fading out over the incoming one, reads as the
/// glyph turning into its other shape.
///
/// **Why not a `Stack`.** A `gtk::Stack` with a crossfade transition animates
/// *between pages* and costs a full re-parent and re-layout of the stack's
/// child; a `Stack` with `None` costs a hard cut, which is what this
/// replaces. Two `gtk::Image`s in a fixed overlay are cheaper than either:
/// the images never move, only their opacity changes, and the button's
/// layout never changes at all.
///
/// The widget is a `gtk::Overlay` holding two `gtk::Image`s. The *button*
/// is the parent, and the overlay is the button's child — so the button
/// keeps its own click handling, tooltip and accessible name, and the
/// glyphs are simply painted inside it.
#[derive(Clone)]
pub struct Glyph {
    /// The button the glyph lives in.
    pub button: gtk4::Button,
    /// The outgoing glyph.
    play: gtk4::Image,
    /// The incoming glyph.
    pause: gtk4::Image,
    /// Which glyph is currently showing.
    showing_pause: Rc<RefCell<bool>>,
    /// The crossfade in flight, so a second press does not start a second
    /// one over the first.
    anim: Anim,
}

impl Glyph {
    /// A button showing the play glyph.
    pub fn new() -> Self {
        let play = gtk4::Image::from_icon_name("media-playback-start-symbolic");
        let pause = gtk4::Image::from_icon_name("media-playback-pause-symbolic");
        // The pause glyph starts hidden: the button is a play button until
        // something is playing.
        pause.set_opacity(0.0);
        let overlay = gtk4::Overlay::new();
        overlay.add_overlay(&pause);
        overlay.set_child(Some(&play));
        let button = gtk4::Button::new();
        button.set_child(Some(&overlay));
        Self {
            button,
            play,
            pause,
            showing_pause: Rc::new(RefCell::new(false)),
            anim: Anim::new(),
        }
    }

    /// Show the play glyph.
    pub fn set_play(&self) {
        self.set(false);
    }

    /// Show the pause glyph.
    pub fn set_pause(&self) {
        self.set(true);
    }

    /// Show the glyph that matches the session's state: the pause glyph while
    /// something is playing, the play glyph otherwise.
    pub fn set_playing(&self, playing: bool) {
        self.set(playing);
    }

    /// Show one glyph or the other, crossfading if it is a change.
    fn set(&self, pause: bool) {
        if *self.showing_pause.borrow() == pause {
            return;
        }
        *self.showing_pause.borrow_mut() = pause;
        let (from, to) = if pause {
            (self.play.clone(), self.pause.clone())
        } else {
            (self.pause.clone(), self.play.clone())
        };
        // The incoming glyph goes to full opacity immediately and the
        // outgoing one fades off under it: the two overlap for the length of
        // the fade, and the button never shows a gap.
        to.set_opacity(1.0);
        self.anim.run(GLYPH_MS, move |t| {
            from.set_opacity(1.0 - t);
        });
    }

    /// The button's icon name, for the callers that still read it.
    pub fn icon_name(&self) -> &'static str {
        if *self.showing_pause.borrow() {
            "media-playback-pause-symbolic"
        } else {
            "media-playback-start-symbolic"
        }
    }
}

impl Default for Glyph {
    fn default() -> Self {
        Self::new()
    }
}

/// Fade a group of widgets' opacity to a value, with one animation.
///
/// **Why a group and not one fade per widget.** Each fade needs its own
/// timer, and a timer per widget means a widget that is faded twice — or a
/// second fade that starts while the first is running — cancels the first
/// and leaves that widget at the wrong opacity. One animation for the whole
/// group is one timer, and every widget in the group moves together, which
/// is also what the eye expects: the chrome leaves as a block.
pub fn fade_group(widgets: &[gtk4::Widget], anim: &Anim, to: f64, duration_ms: u32) {
    let from = widgets.first().map(|w| w.opacity()).unwrap_or(1.0);
    for widget in widgets {
        widget.set_opacity(from);
    }
    let widgets = widgets.to_vec();
    anim.run(duration_ms, move |t| {
        let opacity = from + (to - from) * t;
        for widget in &widgets {
            widget.set_opacity(opacity);
        }
    });
}

/// A pair of panes whose divider slides out of the way and back.
///
/// **What it animates.** A `gtk::Paned` has one position: the width of its
/// start child. Sliding that to zero is what makes the end child — the
/// page — take the whole window. So the slide is the divider's own
/// position, driven a frame at a time.
///
/// **Why the start child is hidden *after* the slide.** A `Paned` gives its
/// whole allocation to its one visible child, so the page cannot expand
/// until the sidebar is actually gone. But hiding it *first* would make the
/// page jump to full width on the first frame, and the slide would have
/// nothing to slide. So the divider moves first, and the child is hidden
/// when the slide lands — by which time the divider is already at zero and
/// the page is already at full width. The two agree exactly.
///
/// **Why the position is restored from the saved value.** The divider's
/// position is remembered when the mode turns on, so the slide back goes to
/// where the user had it rather than to a default.
pub struct Slide {
    /// The paned whose divider moves.
    pub pane: gtk4::Paned,
    /// Where the divider was before the slide.
    saved: Rc<std::cell::Cell<i32>>,
    /// The animation in flight, so a second toggle replaces the first.
    anim: Anim,
    /// The direction a slide in flight is heading: `Some(true)` collapsing,
    /// `Some(false)` opening, `None` settled. The divider's *position* is
    /// mid-flight for ~220 ms after every click, so a caller that decides
    /// from the position cannot tell what a click will do — the heading can.
    heading: Rc<std::cell::Cell<Option<bool>>>,
}

impl Clone for Slide {
    fn clone(&self) -> Self {
        Self {
            pane: self.pane.clone(),
            saved: Rc::clone(&self.saved),
            anim: self.anim.clone(),
            heading: Rc::clone(&self.heading),
        }
    }
}

impl Slide {
    /// A slide for one pane, remembering where its divider is.
    pub fn new(pane: &gtk4::Paned) -> Self {
        Self {
            pane: pane.clone(),
            saved: Rc::new(std::cell::Cell::new(pane.position())),
            anim: Anim::new(),
            heading: Rc::new(std::cell::Cell::new(None)),
        }
    }

    /// The direction the slide in flight is heading: `Some(true)` collapsing,
    /// `Some(false)` opening, `None` when nothing is moving.
    pub fn heading(&self) -> Option<bool> {
        self.heading.get()
    }

    /// True while a slide is in flight.
    pub fn running(&self) -> bool {
        self.anim.running()
    }

    /// Slide the divider to zero, hiding the start child when it lands.
    pub fn collapse(&self) {
        let start = self.pane.start_child();
        // The divider is already at zero — there is nothing to animate, and
        // hiding the child now is all that is left to do.
        if self.pane.position() <= 0 {
            self.heading.set(None);
            if let Some(child) = start {
                child.set_visible(false);
            }
            return;
        }
        let from = self.pane.position();
        let pane = self.pane.clone();
        // The child is hidden on the *last* frame of the slide, by which
        // time the divider is already at zero. Hiding it earlier would let
        // the page jump to full width while the divider was still
        // travelling, and the slide would have nothing to slide.
        let child = start.clone();
        let heading = Rc::clone(&self.heading);
        heading.set(Some(true));
        self.anim.run_eased(FOCUS_MS, ease_in_out, move |t| {
            pane.set_position((from as f64 * (1.0 - t)).round() as i32);
            if t >= 1.0 {
                heading.set(None);
                if let Some(child) = &child {
                    child.set_visible(false);
                }
            }
        });
    }

    /// Slide the divider back to where it was, showing the start child as it
    /// goes.
    pub fn expand(&self) {
        self.expand_to(self.saved.get());
    }

    /// Show the start child and slide the divider to an explicit position.
    ///
    /// `expand` goes to the remembered position; the per-column rails need
    /// to go to the remembered position *when it is a real one* and to the
    /// launch default when it is not — a divider remembered at zero by focus
    /// mode, or never remembered at all because the column was crushed by a
    /// drag rather than collapsed by its rail. Same motion, chosen target.
    pub fn expand_to(&self, to: i32) {
        // The child comes back first: the divider needs somewhere to slide
        // *to*, and a paned with a hidden start child has no width to give.
        if let Some(child) = self.pane.start_child() {
            child.set_visible(true);
        }
        self.heading.set(Some(false));
        let from = self.pane.position();
        if from >= to {
            // Not a slide after all — but the old animation, if one is
            // running, would keep dragging the divider against this direct
            // set. Stop it and own the settled heading.
            self.anim.stop();
            self.heading.set(None);
            self.pane.set_position(to);
            return;
        }
        let pane = self.pane.clone();
        let heading = Rc::clone(&self.heading);
        self.anim.run_eased(FOCUS_MS, ease_in_out, move |t| {
            pane.set_position((from as f64 + (to - from) as f64 * t).round() as i32);
            if t >= 1.0 {
                heading.set(None);
            }
        });
    }

    /// Remember where the divider is now, so `expand` can come back to it.
    pub fn remember(&self) {
        self.saved.set(self.pane.position());
    }

    /// Settle the divider to zero in one frame — the column rails' collapse.
    ///
    ///  (alpha, 2026-10-31, the user's round-4 report "the notes-list
    /// rail still needs several clicks"): the animated rail toggle left a
    /// 220 ms window in which every further click was dropped, so a user
    /// who clicks twice in quick succession — the natural "it didn't take"
    /// response — had the second click eaten (measured live: a click 29 ms
    /// after the first did nothing). The rails are secondary chrome; the
    /// motion earns nothing there that an instant settle loses, and an
    /// instant toggle has no in-flight window at all: every click
    /// registers. Fill-the-window keeps its animated slide — there the
    /// page genuinely grows into the space, and the chrome is fading out
    /// from under the pointer anyway.
    pub fn collapse_now(&self) {
        self.anim.stop();
        self.heading.set(None);
        self.pane.set_position(0);
        if let Some(child) = self.pane.start_child() {
            child.set_visible(false);
        }
    }

    /// Settle the divider at `to` in one frame — the rails' expand. See
    /// [`Slide::collapse_now`] for why the rails do not animate.
    pub fn expand_now(&self, to: i32) {
        self.anim.stop();
        self.heading.set(None);
        if let Some(child) = self.pane.start_child() {
            child.set_visible(true);
        }
        self.pane.set_position(to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_out_cubic_is_a_clamped_ramp() {
        assert_eq!(ease_out_cubic(-1.0), 0.0);
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert!(ease_out_cubic(2.0) <= 1.0);
    }

    #[test]
    fn ease_out_cubic_starts_fast_and_settles() {
        // Halfway through, an ease-out is already past halfway — that is
        // what makes it read as *arriving* rather than being yanked.
        assert!(ease_out_cubic(0.5) > 0.5);
        // And it is monotonic, so the value never travels backwards.
        let mut previous = 0.0;
        for step in 0..=100 {
            let t = step as f64 / 100.0;
            let value = ease_out_cubic(t);
            assert!(value >= previous, "not monotonic at {t}");
            previous = value;
        }
    }

    #[test]
    fn anim_starts_idle_and_stops_cleanly() {
        let anim = Anim::new();
        assert!(!anim.running());
        anim.stop();
        assert!(!anim.running());
    }
}
