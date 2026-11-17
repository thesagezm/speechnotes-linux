//! The show: the presentation machinery the deck's Present button runs.
//!
//! PowerPoint's two-screen show is the shape the reader asked for: the
//! **audience** screen gets the slide — fullscreen, black around it; the
//! **speaker's** screen gets the console — the current slide small and
//! *rendered*, the next slide beside it, also rendered, the speaker text
//! below, a timer and the stepping controls. With one screen the console
//! floats over the deck instead of owning a screen. **Mirror** is the show
//! without the console: the slide fullscreen on both screens.
//!
//! The show's extra views are real [`SlideView`]s spawned on the deck's own
//! engine (the one scheme handler, the one deck parse), so a preview is the
//! slide — the same absolutely-positioned document the audience screen
//! shows — never a re-layout of parsed text. The reader's own deck view
//! stays the sync hub: a step anywhere (console button, audience click,
//! arrow key) scrolls the deck, the deck reports, and the follow pushes the
//! new index into the audience view and both previews.

use std::cell::Cell;
use std::rc::Rc;

use gtk4::glib::Propagation;
use gtk4::prelude::*;

use crate::slideview::SlideView;

// ---------------------------------------------------------------------------
// The show's parts
// ---------------------------------------------------------------------------

/// The audience window: the fullscreen slide on the other screen.
pub(crate) struct Audience {
    window: gtk4::Window,
    #[allow(dead_code)]
    view: SlideView,
}

/// The speaker's console: the current slide rendered, the next slide
/// rendered, the speaker text, the timer and the controls.
pub(crate) struct Console {
    window: gtk4::Window,
    counter: gtk4::Label,
    timer: gtk4::Label,
    notes: gtk4::Label,
    notes_scroll: gtk4::ScrolledWindow,
    /// The next-slide preview's frame: hidden once the show reaches the
    /// last slide — there is no next, so nothing stands where it would be.
    next_frame: gtk4::Frame,
    view_current: SlideView,
    view_next: SlideView,
    tick: Cell<Option<gtk4::glib::SourceId>>,
}

/// The running show: the windows it opened (which ones is the kind's
/// answer at start time — Mirror opens only the audience screen, Speaker
/// opens both, Floating only the console).
pub(crate) struct Show {
    audience: Option<Audience>,
    console: Option<Console>,
}

/// Window titles the show carries. On X they are also how the drive finds
/// the windows for a screenshot; keep them distinct from the main window's.
pub(crate) const AUDIENCE_TITLE: &str = "Speechnotes — Show";
pub(crate) const CONSOLE_TITLE: &str = "Speechnotes — Speaker view";

// ---------------------------------------------------------------------------
// The show on the SlideView
// ---------------------------------------------------------------------------

impl SlideView {
    /// Whether a show is running (any kind — Mirror included).
    pub fn show_running(&self) -> bool {
        self.inner.show.borrow().is_some()
    }

    /// Whether the speaker's console is part of the running show.
    pub fn console_open(&self) -> bool {
        self.inner
            .show
            .borrow()
            .as_ref()
            .is_some_and(|s| s.console.is_some())
    }

    /// Start a show. `audience_monitor` is the screen the slide fullscreens
    /// on — Mirror and Speaker pass one (a projector's screen, the monitor
    /// the main window is not on), Floating passes none. `console_monitor`
    /// is the screen the console fullscreens on — Speaker passes the main
    /// window's own. With an audience screen and no console monitor —
    /// **Mirror** — the show runs WITHOUT the console: the slide owns both
    /// screens and the reader's floating transport is the speaker's control
    /// there (`reader::sync_show_controls` pins it). With no audience at all
    /// (one screen) the console floats over the deck. No-op while a show is
    /// already running.
    pub(crate) fn start_show(
        &self,
        audience_monitor: Option<&gtk4::gdk::Monitor>,
        console_monitor: Option<&gtk4::gdk::Monitor>,
    ) {
        if self.show_running() {
            return;
        }
        let click_advance = self.inner.click_advance.get();
        let index = self.inner.current.get();

        // The audience screen: a real deck view on the deck's engine, in
        // present mode, opened on the reader's current slide. Its reports
        // share the main view's follow, so a click or a key on it steers
        // the whole show.
        let audience = audience_monitor.map(|monitor| {
            let view = self.spawn_show_view(true);
            view.set_fit("page");
            view.set_present(true, click_advance);
            view.show_slide(index);
            view.load_deck();
            let window = gtk4::Window::new();
            window.set_title(Some(AUDIENCE_TITLE));
            window.set_child(Some(view.widget()));
            // The show's keys on the audience screen, whatever widget holds
            // the focus there: the arrows step (a projector clicker's
            // keyboard twin), Esc ends the show. Capture phase — the
            // window sees the key before the page can.
            {
                let inner = self.inner.clone();
                let controller = gtk4::EventControllerKey::new();
                controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
                controller.connect_key_pressed(move |_, key, _, _| show_key(&inner, key));
                window.add_controller(controller);
            }
            window.present();
            // Projectors: fullscreen-on-monitor is only a *hint* to the
            // compositor (xdg-shell carries no forced output), and the show
            // lives or dies on the slide landing on the audience's screen.
            // Measured on this box (COSMIC, 2026-11-12): the hint is IGNORED
            // for a window that is not mapped yet — both the X11 and the
            // Wayland backends fullmapped it in place instead. So the
            // window maps first (wherever the compositor puts it), and the
            // fullscreen lands after the map, when set_fullscreen carries
            // an output for a window the compositor already knows — which
            // it honours. The landing is then VERIFIED, and a wrong-screen
            // landing retries the request once, each attempt logged: a
            // compositor that ignores the hint is a logged fact, not a
            // mystery the speaker discovers mid-talk.
            {
                let want = monitor.clone();
                let window = window.clone();
                gtk4::glib::timeout_add_local(std::time::Duration::from_millis(260), move || {
                    window.fullscreen_on_monitor(&want);
                    gtk4::glib::ControlFlow::Break
                });
            }
            {
                let want = monitor.clone();
                let window = window.clone();
                gtk4::glib::timeout_add_local(std::time::Duration::from_millis(1100), move || {
                    let on_wanted = audience_landed_on(&window, &want);
                    match on_wanted {
                        Some(true) => {
                            tracing::info!(
                                audience = %want.model().unwrap_or_default(),
                                "the show's slide window landed on the audience screen"
                            );
                        }
                        Some(false) => {
                            tracing::warn!(
                                requested = %want.model().unwrap_or_default(),
                                "the show's slide window missed the audience screen; asking again"
                            );
                            window.fullscreen_on_monitor(&want);
                            // The re-ask's own check, a second later.
                            let want = want.clone();
                            let window = window.clone();
                            gtk4::glib::timeout_add_local(
                                std::time::Duration::from_millis(1100),
                                move || {
                                    match audience_landed_on(&window, &want) {
                                        Some(true) => tracing::info!(
                                            audience = %want.model().unwrap_or_default(),
                                            "the show's slide window landed on the retry"
                                        ),
                                        Some(false) => tracing::error!(
                                            requested = %want.model().unwrap_or_default(),
                                            "the show's slide window is not on the audience \
                                             screen after the retry — the compositor ignored \
                                             the output; the show continues where it landed"
                                        ),
                                        None => tracing::warn!(
                                            "the show's slide window has no surface to check"
                                        ),
                                    }
                                    gtk4::glib::ControlFlow::Break
                                },
                            );
                        }
                        None => {
                            tracing::warn!("the show's slide window has no surface to check")
                        }
                    }
                    gtk4::glib::ControlFlow::Break
                });
            }
            Audience { window, view }
        });

        // The speaker's console: its two previews are non-interactive deck
        // views (no click-stepping, no navigation reports — the reader's
        // follow drives them), fitted to their own boxes so a slide shows
        // whole at preview size. **Mirror** (an audience screen, no console
        // monitor) builds none: the slide owns both screens and the pinned
        // floating transport is the speaker's control. One screen (no
        // audience) floats the console over the deck.
        let console = if audience_monitor.is_none() || console_monitor.is_some() {
            let slides_row = match console_monitor {
                Some(monitor) => (monitor.geometry().height() as f32 * 0.44) as i32,
                None => 320,
            };
            let view_current = self.spawn_show_view(false);
            let view_next = self.spawn_show_view(false);
            let console = build_console(
                self.inner.clone(),
                view_current,
                view_next,
                slides_row,
                console_monitor.is_some(),
            );
            console.view_current.set_fit("page");
            console.view_next.set_fit("page");
            console.view_current.show_slide(index);
            console.view_next.show_slide(index + 1);
            console.view_current.load_deck();
            console.view_next.load_deck();
            match console_monitor {
                Some(monitor) => {
                    // Map first, fullscreen after — the audience window's
                    // lesson (fullscreen-on-monitor before the map is
                    // ignored by the compositor; after the map it is kept).
                    console.window.present();
                    let want = monitor.clone();
                    let window = console.window.clone();
                    gtk4::glib::timeout_add_local(
                        std::time::Duration::from_millis(260),
                        move || {
                            window.fullscreen_on_monitor(&want);
                            gtk4::glib::ControlFlow::Break
                        },
                    );
                }
                None => {
                    console.window.set_default_size(1040, 700);
                    console.window.present();
                }
            }
            Some(console)
        } else {
            None
        };

        *self.inner.show.borrow_mut() = Some(Show {
            audience,
            console,
        });
    }

    /// End the show: the audience window and the console go, wherever they
    /// were opened from (the Present toggle, Esc, the mode leaving, a new
    /// deck replacing the one the show describes).
    pub fn end_show(&self) {
        let show = self.inner.show.borrow_mut().take();
        if let Some(show) = show {
            if let Some(console) = show.console {
                if let Some(tick) = console.tick.take() {
                    tick.remove();
                }
                console.window.destroy();
            }
            if let Some(audience) = show.audience {
                audience.window.destroy();
            }
        }
    }

    /// Fill the console's readouts: the slide counter and the current
    /// slide's speaker text. A no-op while the console is closed — the
    /// reader calls this on every slide change regardless. (The next slide
    /// needs no text readout: the console shows the next slide *rendered*.)
    pub fn presenter_update(&self, counter: &str, now: &str) {
        let show = self.inner.show.borrow();
        if let Some(console) = show.as_ref().and_then(|s| s.console.as_ref()) {
            console.update(counter, now);
        }
    }

    /// Push the show to slide `index`: the audience screen scrolls, the
    /// console's previews follow. The reader calls this on every slide
    /// change while a show runs — every path that moves the deck's place
    /// (the deck's own report, the toolbar's step, a console button).
    pub(crate) fn present_follow(&self, index: usize) {
        let show = self.inner.show.borrow();
        let Some(show) = show.as_ref() else {
            return;
        };
        if let Some(audience) = &show.audience {
            audience.view.show_slide(index);
        }
        if let Some(console) = &show.console {
            let count = self
                .inner
                .engine
                .state
                .deck
                .borrow()
                .as_ref()
                .map(|d| d.slides.len())
                .unwrap_or(0);
            console.view_current.show_slide(index);
            console.next_frame.set_visible(index + 1 < count);
            if index + 1 < count {
                console.view_next.show_slide(index + 1);
            }
        }
    }
}


/// Where a window actually landed: `Some(true)` on the wanted monitor,
/// `Some(false)` elsewhere, `None` while it has no surface yet. Identity is
/// model + geometry — the bindings carry no monitor equality, and two
/// monitors never share both on a sane desktop.
fn audience_landed_on(
    window: &gtk4::Window,
    want: &gtk4::gdk::Monitor,
) -> Option<bool> {
    let display = gtk4::gdk::Display::default()?;
    let surface = window.surface()?;
    let monitor = display.monitor_at_surface(&surface)?;
    let g = want.geometry();
    let on_wanted = monitor.model() == want.model()
        && monitor.geometry()
            == gtk4::gdk::Rectangle::new(g.x(), g.y(), g.width(), g.height());
    Some(on_wanted)
}

/// The show's key map, shared by the audience window and the console:
/// arrows and the paging keys step, Esc ends the show. Runs in capture
/// phase, so the page under it never sees a key the show already acted on.
fn show_key(inner: &Rc<super::Inner>, key: gtk4::gdk::Key) -> Propagation {
    use gtk4::gdk::Key;
    let step = match key {
        Key::Right | Key::Down | Key::Page_Down | Key::space | Key::Return | Key::KP_Enter => {
            Some(1i64)
        }
        Key::Left | Key::Up | Key::Page_Up | Key::BackSpace => Some(-1i64),
        Key::Home => Some(i64::MIN / 2),
        Key::End => Some(i64::MAX / 2),
        Key::Escape => None,
        _ => return Propagation::Proceed,
    };
    match step {
        Some(delta) => {
            if let Some(callback) = inner.on_console_step.borrow().clone() {
                callback(delta);
            }
        }
        None => {
            if let Some(callback) = inner.engine.on_exit_present.borrow().clone() {
                callback();
            }
        }
    }
    Propagation::Stop
}

// ---------------------------------------------------------------------------
// The console
// ---------------------------------------------------------------------------

/// Build the console window and its widgets around the two preview views.
///
/// The layout is PowerPoint's presenter view: the counter and the timer on
/// the header row, the current slide's preview on the left and the next
/// slide's on the right, the stepping controls beneath them, and the
/// speaker text filling the rest. Dark surface — the speaker reads it in a
/// darkened room (`.presenter-window`).
fn build_console(
    inner: Rc<super::Inner>,
    view_current: SlideView,
    view_next: SlideView,
    slides_row_height: i32,
    fullscreen: bool,
) -> Console {
    let window = gtk4::Window::new();
    window.set_title(Some(CONSOLE_TITLE));
    window.add_css_class("presenter-window");
    // Transient for the main window: it stays with the deck, and a tiling
    // compositor may keep it above the deck the console describes.
    if let Some(root) = view_current.widget().root() {
        if let Some(main) = root.downcast_ref::<gtk4::Window>() {
            window.set_transient_for(Some(main));
        }
    }

    let timer = gtk4::Label::builder()
        .css_classes(["presenter-timer", "numeric"])
        .label("0:00")
        .halign(gtk4::Align::End)
        .valign(gtk4::Align::Start)
        .build();
    let counter = gtk4::Label::builder()
        .css_classes(["presenter-counter"])
        .halign(gtk4::Align::Start)
        .valign(gtk4::Align::Start)
        .build();

    // The previews. The frames give the webviews a stage of their own — the
    // deck's own page-fit letterboxes inside — with a thin edge so the
    // slide's white reads against the dark console. The current preview
    // takes the wider share, the next the remainder, PowerPoint's split.
    let current_frame = gtk4::Frame::builder()
        .css_classes(["presenter-slide"])
        .hexpand(true)
        .halign(gtk4::Align::Fill)
        .child(view_current.widget())
        .build();
    let next_frame = gtk4::Frame::builder()
        .css_classes(["presenter-slide"])
        .hexpand(true)
        .halign(gtk4::Align::Fill)
        .child(view_next.widget())
        .build();

    let slides_row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .height_request(slides_row_height)
        .build();
    slides_row.append(&current_frame);
    slides_row.append(&next_frame);

    // The controls. Named for the drive (`press 'Next slide'`), tooltip'd
    // for the speaker.
    let prev = gtk4::Button::builder()
        .label("◀  Previous")
        .css_classes(["presenter-btn"])
        .tooltip_text("Previous slide (Left)")
        .build();
    let next = gtk4::Button::builder()
        .label("Next  ▶")
        .css_classes(["presenter-btn"])
        .tooltip_text("Next slide (Right, Space)")
        .build();
    let end = gtk4::Button::builder()
        .label("End show")
        .css_classes(["presenter-btn", "presenter-btn-end"])
        .tooltip_text("End the show (Esc)")
        .build();
    let controls = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk4::Align::Center)
        .build();
    controls.append(&prev);
    controls.append(&next);
    controls.append(&end);

    let notes_heading = gtk4::Label::builder()
        .css_classes(["presenter-heading"])
        .label("NOTES")
        .halign(gtk4::Align::Start)
        .build();
    let notes = gtk4::Label::builder()
        .css_classes(["presenter-now"])
        .wrap(true)
        .wrap_mode(gtk4::pango::WrapMode::Word)
        .xalign(0.0)
        .valign(gtk4::Align::Start)
        .build();
    let notes_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&notes)
        .build();

    let margins = if fullscreen { 16 } else { 14 };
    let col = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(10)
        .margin_top(margins)
        .margin_bottom(margins)
        .margin_start(margins + 4)
        .margin_end(margins + 4)
        .build();
    let header = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .build();
    header.append(&counter);
    // The timer rides the header's right end.
    header.append(&{
        let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        spacer
    });
    header.append(&timer);
    col.append(&header);
    col.append(&slides_row);
    col.append(&controls);
    col.append(&notes_heading);
    col.append(&notes_scroll);
    window.set_child(Some(&col));

    // The controls' clicks are the reader's navigation: the same step the
    // arrows take, wired by the reader (`on_console_step`), and "End show"
    // is Esc's own exit.
    {
        let inner = inner.clone();
        prev.connect_clicked(move |_| {
            if let Some(callback) = inner.on_console_step.borrow().clone() {
                callback(-1);
            }
        });
    }
    {
        let inner = inner.clone();
        next.connect_clicked(move |_| {
            if let Some(callback) = inner.on_console_step.borrow().clone() {
                callback(1);
            }
        });
    }
    {
        let inner = inner.clone();
        end.connect_clicked(move |_| {
            if let Some(callback) = inner.engine.on_exit_present.borrow().clone() {
                callback();
            }
        });
    }
    // The console's own keys: whatever widget inside it holds the focus,
    // the arrows step and Esc ends the show.
    {
        let controller = gtk4::EventControllerKey::new();
        controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        controller.connect_key_pressed({
            let inner = inner.clone();
            move |_, key, _, _| show_key(&inner, key)
        });
        window.add_controller(controller);
    }

    let started = std::time::Instant::now();
    let console = Console {
        window: window.clone(),
        counter,
        timer,
        notes,
        notes_scroll,
        next_frame,
        view_current,
        view_next,
        tick: Cell::new(None),
    };

    // The elapsed timer, one tick a second for as long as the console is
    // open; cancelled on close.
    {
        let timer = console.timer.clone();
        let tick = gtk4::glib::timeout_add_local(std::time::Duration::from_millis(1000), move || {
            let secs = started.elapsed().as_secs();
            let readout = if secs >= 3600 {
                format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
            } else {
                format!("{}:{:02}", secs / 60, secs % 60)
            };
            timer.set_text(&readout);
            gtk4::glib::ControlFlow::Continue
        });
        console.tick.set(Some(tick));
    }

    // Closing the console window (its WM close, the floating case) takes
    // the console out of the running show and stops its timer. The show
    // itself — the audience screen — stays; Esc or the Present toggle ends
    // it. The window closes itself: returning Proceed lets the default
    // handler destroy it (destroying it from inside the handler would
    // re-enter).
    {
        let inner = inner.clone();
        window.connect_close_request(move |_| {
            if let Some(show) = inner.show.borrow_mut().as_mut() {
                if let Some(console) = show.console.take() {
                    if let Some(tick) = console.tick.take() {
                        tick.remove();
                    }
                }
            }
            Propagation::Proceed
        });
    }

    console
}

impl Console {
    /// Fill the readouts: the slide counter and the current slide's speaker
    /// text. The notes pane restarts at its top — a new slide's notes start
    /// where the speaker starts reading.
    fn update(&self, counter: &str, now: &str) {
        self.counter.set_text(counter);
        self.notes.set_text(now);
        self.notes_scroll.vadjustment().set_value(0.0);
    }
}
