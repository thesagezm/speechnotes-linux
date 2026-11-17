//! The per-column rails (collapse/expand the sidebar and the notes list)
//! and fill-the-window mode. Split out of `window.rs` (Round 4, pure
//! code-motion).

use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4 as gtk;

use crate::app::App;

use super::Widgets;
use super::editor::refresh_editor;

/// One rail button: a slim flat chevron strip at a column's right edge that
/// collapses and re-opens the column beside it. The glyph and the wording
/// flip with the state — `sync_rails` keeps them honest.
///
/// Fixed 2026-11-02: the strip is **24 px** wide, not the
/// 20 px the icon and its padding ask for. The rail is a small target
/// pressed against a divider, and a target that narrow is one missed
/// click away from the divider's own drag gesture — the user's report
/// that this rail "needs several clicks". The width is a minimum, so the
/// glyph stays centred in it.
pub(crate) fn rail_button(initial_tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name("sn-pane-hide-symbolic")
        .css_classes(["pane-rail"])
        .valign(gtk::Align::Fill)
        .build();
    button.set_size_request(24, -1);
    button.set_tooltip_text(Some(initial_tooltip));
    button.update_property(&[gtk::accessible::Property::Label(initial_tooltip)]);
    button
}

/// At or under this width a divider counts as collapsed. Not zero: a user
/// dragging an extreme can leave three pixels of divider, and "three pixels"
/// is a lost column as surely as zero is. (The Paned hides nothing itself —
/// a drag only narrows — so the width test is what catches a crushed column;
/// the visibility test catches what the rail and focus mode hid.)
const RAIL_COLLAPSED_AT: i32 = 12;

/// Whether a divider's column is effectively gone: squeezed to the threshold
/// by a drag, or hidden outright by its rail or by focus mode.
pub(crate) fn rail_collapsed(pane: &gtk::Paned) -> bool {
    if pane.position() <= RAIL_COLLAPSED_AT {
        return true;
    }
    pane.start_child().is_some_and(|child| !child.is_visible())
}

/// The width a rail expands its column to: the width the user last chose for
/// it (recorded when the rail collapsed it), or the launch default when that
/// record is no good — a divider never rail-collapsed, or remembered at zero
/// by focus mode. Without the floor, a column crushed to a sliver would
/// expand back into a sliver, and the rail would be no way out at all.
pub(crate) fn rail_expand_target(saved: i32, default: i32) -> i32 {
    if saved > RAIL_COLLAPSED_AT {
        saved
    } else {
        default
    }
}

/// Collapse or re-open one column by its rail: 0 the sidebar, 1 the notes
/// list. The state read is the divider's own — not a flag the button
/// maintains — so a column the user crushed by dragging recovers through the
/// same click that would undo a rail collapse.
///
/// **The rail toggle settles in one frame, not a slide.** The animated
/// version left a ~220 ms window in which every further click was dropped
/// (the guard below), so a user who clicked twice in quick succession — the
/// natural "it didn't take" response — had the second click eaten; that is
/// the user's phase-3 report ("needs several clicks") and, still, their
/// round-4 report after the guard landed (measured live 2026-10-31: a
/// click 29 ms after the first did nothing). The rails now toggle
/// instantly via [`crate::motion::Slide::collapse_now`] /
/// [`crate::motion::Slide::expand_now`] — no flight, no dead window, every
/// click registers. Fill-the-window keeps its animated slide.
// Semantics: the guard must not reverse mid-flight and must not swallow
// mid-flight clicks (both were real regressions here once each).
pub(crate) fn toggle_column(app: &Rc<App>, index: usize) {
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let (pane, slide, saved, default) = match index {
        0 => (&w.panes.0, &w.slides.0, app.rail_saved.get().0, 235),
        _ => (&w.panes.1, &w.slides.1, app.rail_saved.get().1, 400),
    };
    // The only flight left is fill-the-window's (its own slide, chrome
    // fading out from under the pointer): a click during it has no honest
    // target to settle, so it waits.
    if slide.running() {
        return;
    }
    let collapsing = !rail_collapsed(pane);
    if !collapsing {
        slide.expand_now(rail_expand_target(saved, default));
    } else {
        // The divider is settled (no flight), so its position is the
        // column's chosen width; remember it before collapsing.
        match index {
            0 => app.rail_saved.set((pane.position(), app.rail_saved.get().1)),
            _ => app.rail_saved.set((app.rail_saved.get().0, pane.position())),
        }
        slide.remember();
        slide.collapse_now();
    }
    sync_rails(&w, None);
}

/// Point the rails' glyphs and wording at the dividers' real state, so the
/// affordance never lies about what a click will do.
///
/// `only` narrows the work to one rail (the divider that just moved); both
/// are updated when it is None.
///
/// While a slide is in flight the glyph follows the slide's *heading*, not
/// the divider's position: the position is mid-flight for the whole 220 ms,
/// and a glyph that waited for it would flip a hundred milliseconds after
/// the click — or, on a reversed slide, point the wrong way entirely. The
/// heading is what the click promised; the position test resumes the moment
/// the slide settles (and is what catches a column crushed by dragging).
pub(crate) fn sync_rails(w: &Widgets, only: Option<&gtk::Button>) {
    let (root, list_editor) = &w.panes;
    for (rail, pane, slide, hide_word, show_word) in [
        (
            &w.sidebar_rail,
            root,
            &w.slides.0,
            "Hide the sidebar",
            "Show the sidebar",
        ),
        (
            &w.list_rail,
            list_editor,
            &w.slides.1,
            "Hide the notes list",
            "Show the notes list",
        ),
    ] {
        if let Some(one) = only {
            if one != rail {
                continue;
            }
        }
        let gone = if slide.running() && slide.heading().is_some() {
            slide.heading() == Some(true)
        } else {
            rail_collapsed(pane)
        };
        let word = if gone { show_word } else { hide_word };
        rail.set_icon_name(if gone {
            "sn-pane-show-symbolic"
        } else {
            "sn-pane-hide-symbolic"
        });
        rail.set_tooltip_text(Some(word));
        rail.update_property(&[gtk::accessible::Property::Label(word)]);
    }
}

/// Fill the whole window with the open page, or put the columns back.
///
/// Focus mode is *only the page*: the side columns, the toolbar row, the
/// note title entry, the format bar, the status line, the header's buttons
/// and its search, and the transport at the bottom all collapse. What
/// remains is the window controls — the titlebar keeps drawing them — and
/// one leave button beside them; F11 is the keyboard route. One function
/// and one `Cell`, so the toolbar toggle, the leave button and F11 cannot
/// drift: every route calls this, and the `replace` guard makes the
/// button-sync at the end idempotent rather than re-entrant.
///
/// **The motion.** The two dividers slide to zero over ~220 ms while the
/// chrome fades, and slide back when the mode leaves. The slide is the
/// divider's own position, not a CSS transition — GTK's CSS cannot animate
/// a position, and the whole point of the mode is that the page *moves*
/// into the space the columns left.
///
/// **Why the chrome fades rather than vanishes.** A row that disappears
/// instantly leaves a hole; one that fades out leaves the page already
/// there. The fade and the slide run together, so the reader's eye follows
/// the page rather than the hole.
pub fn apply_focus_mode(app: &Rc<App>, on: bool) {
    if app.focus_mode.replace(on) == on {
        return;
    }
    let Some(w) = app.widgets.borrow().clone() else {
        return;
    };
    let (root, list_editor) = &w.panes;
    if on {
        app.focus_saved.set((root.position(), list_editor.position()));
        // Each slide remembers its own divider, so the mode comes back to
        // the layout the user had rather than to a default.
        w.slides.0.remember();
        w.slides.1.remember();
    }
    // The chrome collapses as one block. The title entry and the format bar
    // are rows of their own, so they hide here directly; the toolbar's
    // buttons go with their row. Coming back, those two rows belong to the
    // open page — `refresh_editor` decides, now that it may show them.
    //
    // The rows fade out and are hidden when the fade lands; the button
    // widgets inside the header bar are hidden outright, because a
    // half-faded button is a button the reader might press.
    let mut chrome: Vec<gtk::Widget> = vec![
        w.toolbar.upcast_ref::<gtk::Widget>().clone(),
        w.editor_title.upcast_ref::<gtk::Widget>().clone(),
        w.format_bar.upcast_ref::<gtk::Widget>().clone(),
        w.editor_status.upcast_ref::<gtk::Widget>().clone(),
        // The transport's *scroller*, not the row inside it: hiding the row
        // alone would leave the scroller's empty strip behind.
        w.player_scroll.upcast_ref::<gtk::Widget>().clone(),
        // The rails are chrome too — fill-the-window is only the page — and
        // they come back with it: whichever column is collapsed, its rail is
        // the affordance that brings it back.
        w.sidebar_rail.upcast_ref::<gtk::Widget>().clone(),
        w.list_rail.upcast_ref::<gtk::Widget>().clone(),
    ];
    // The reader's contents sidebar is page furniture in the same sense: a
    // column beside the page, not the page. It fades and hides with the
    // chrome and comes back with it; its own reveal state is untouched by
    // the mode, so the reader's toggle still says the right thing.
    chrome.push(
        w.reader
            .toc_revealer
            .upcast_ref::<gtk::Widget>()
            .clone(),
    );
    if on {
        // The chrome fades out and goes with the slide. The rows are hidden
        // when the fade ends — a row at zero opacity still occupies its
        // height, and the page would not move up until it did.
        crate::motion::fade_group(&chrome, &w.focus_anim, 0.0, crate::motion::FOCUS_MS);
        // The hide is a timer, not the fade's own last frame, and the mode
        // can leave before it fires: a reader who taps F11 twice — the
        // natural "did it take?" response — exits inside the fade window.
        // The timer is stored and CANCELLED on the way out; left running it
        // hid the toolbar, the transport bar and both rails out of the
        // restored layout, and the reader's Contents button went dead with
        // them (its toggle flips reveal, never visibility).
        if let Some(old) = app.focus_hide_source.borrow_mut().take() {
            old.remove();
        }
        let chrome = chrome.clone();
        let timer_app = app.clone();
        let source = glib::timeout_add_local(
            std::time::Duration::from_millis(crate::motion::FOCUS_MS as u64),
            move || {
                *timer_app.focus_hide_source.borrow_mut() = None;
                for widget in &chrome {
                    widget.set_visible(false);
                    // The opacity goes back to 1 so the next fade starts
                    // from a known place.
                    widget.set_opacity(1.0);
                }
                glib::ControlFlow::Break
            },
        );
        *app.focus_hide_source.borrow_mut() = Some(source);
        for widget in [
            w.search.upcast_ref::<gtk::Widget>(),
            w.sort_button.upcast_ref(),
            w.new_note_button.upcast_ref(),
            w.new_nb_button.upcast_ref(),
            w.import_book_button.upcast_ref(),
            w.settings_button.upcast_ref(),
        ] {
            widget.set_visible(false);
        }
        // The dividers slide to zero; the start children are hidden when
        // the slide lands, so the page expands exactly as the divider
        // reaches the edge.
        w.slides.0.collapse();
        w.slides.1.collapse();
    } else {
        // Coming back: the chrome is shown first and fades *in*, so the
        // rows are there before they are legible — a row that appeared at
        // full opacity would flash.
        //
        // The hide timer the entry armed goes first: an exit inside the
        // fade window must not leave a timer aimed at chrome that is coming
        // back — it would hide it out of the restored layout.
        if let Some(old) = app.focus_hide_source.borrow_mut().take() {
            old.remove();
        }
        for widget in &chrome {
            widget.set_visible(true);
        }
        crate::motion::fade_group(&chrome, &w.focus_anim, 1.0, crate::motion::FOCUS_MS);
        for widget in [
            w.search.upcast_ref::<gtk::Widget>(),
            w.sort_button.upcast_ref(),
            w.new_note_button.upcast_ref(),
            w.new_nb_button.upcast_ref(),
            w.import_book_button.upcast_ref(),
            w.settings_button.upcast_ref(),
        ] {
            widget.set_visible(true);
        }
        // The dividers slide back to where they were, and the start
        // children come back as the slide begins — the divider needs
        // somewhere to slide to.
        w.slides.0.expand();
        w.slides.1.expand();
        // The saved positions are the authority here: a slide that was
        // interrupted half-way may have left a divider short of where it
        // started, and the mode must land where the user left it.
        let (sidebar_at, list_at) = app.focus_saved.get();
        root.set_position(sidebar_at);
        list_editor.set_position(list_at);
        // A column collapsed before the mode stays collapsed. The restore
        // put each divider back where the mode found it; one at or under the
        // threshold is a collapsed column whose start child must be hidden —
        // the mode's own expand showed it again, and a visible child at zero
        // width is the sliver-plus-tiny-handle state the rails exist to end.
        for pane in [&root, &list_editor] {
            if pane.position() <= RAIL_COLLAPSED_AT {
                if let Some(child) = pane.start_child() {
                    child.set_visible(false);
                }
            }
        }
        sync_rails(&w, None);
    }
    // The book pane's frame: the slide grows and shrinks it, and a quick
    // toggle's collapse-then-expand lands at the allocation it left — the
    // two resizes coalesce, and on this WebKit the pane keeps whatever frame
    // was last committed: white, when the boot's first frame raced the
    // toggle. The nudge (two one-pixel margin flips) forces a real
    // allocation once the slide has settled, enter and exit alike; it is
    // the same medicine the engine's own 'ready' applies.
    {
        let book = w.reader.book.clone();
        glib::timeout_add_local(
            std::time::Duration::from_millis(crate::motion::FOCUS_MS as u64 + 60),
            move || {
                book.nudge_allocation();
                glib::ControlFlow::Break
            },
        );
    }
    // Fixed 2026-11-02: the reader page takes the keyboard
    // focus as the mode comes and goes. The mode hides the rails and the
    // toolbar row — and hiding the widget that holds the focus is one of
    // the ways the focus is lost — so the page claims it again here. With
    // no focused widget GTK4 dispatches no key event at all, which is why
    // the arrows died in fill-the-window until the reader clicked an
    // on-screen button. (The grab itself checks that the reader page is
    // the visible one: on the note page the editor's caret owns the
    // keyboard.)
    super::focus::grab_reader_focus(app);
    w.exit_focus.set_visible(on);
    // The floating transport is fill-the-window's own affordance: it shows
    // once when the mode comes on over the reader page (so the reader
    // learns it exists), follows the mouse while the mode is on, and goes
    // with the mode when it goes off. Outside the mode it never shows,
    // because the toolbar row it mirrors is there.
    if on {
        crate::reader::float_poke(app);
    } else {
        crate::reader::float_hide(app);
    }
    // For a deck, fill-the-window *is* presentation mode: one slide per
    // screen, click and the arrows stepping. The sync is idempotent and a
    // no-op for every other format.
    crate::reader::sync_slide_present(app);
    if !on {
        refresh_editor(app);
    }
    // Sync the toggle: the one that was clicked already holds the state.
    // Setting active fires `toggled`, whose handler re-enters with a state
    // that now matches — the guard at the top returns — so no loop.
    w.focus_toggle.set_active(on);
    w.focus_toggle.set_icon_name(if on {
        "view-restore-symbolic"
    } else {
        "view-fullscreen-symbolic"
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_expand_target_floors_the_crushed_widths() {
        // A real remembered width comes back exactly.
        assert_eq!(rail_expand_target(235, 235), 235);
        assert_eq!(rail_expand_target(300, 235), 300);
        // A divider remembered collapsed (focus mode on an already-collapsed
        // column) expands to the default, not to zero.
        assert_eq!(rail_expand_target(0, 235), 235);
        // A crushed-by-drag width is not a chosen width.
        assert_eq!(rail_expand_target(5, 400), 400);
        assert_eq!(rail_expand_target(RAIL_COLLAPSED_AT, 400), 400);
        // One pixel above the threshold is already a choice.
        assert_eq!(
            rail_expand_target(RAIL_COLLAPSED_AT + 1, 400),
            RAIL_COLLAPSED_AT + 1
        );
    }
}
