//! Read-along highlighting: the spoken word lights up in the note buffer or
//! on the book page, and the transport widgets mirror the progress. Split
//! out of `player.rs` (Round 4, pure code-motion).

use std::rc::Rc;

use gtk4::prelude::*;

use crate::app::App;

use super::transport::set_play_icon;
use super::{TAG_READ_ALONG, HIGHLIGHT};

pub(crate) fn update_read_along(app: &Rc<App>, progress: &sn_tts::Progress) {
    let note_is_open = app.tts_note.borrow().as_deref() == app.open_note.borrow().as_deref();
    // A book's read-along: the voice's text must be the chapter's text, or
    // the highlight would land on words that are not on screen. The hash is
    // the same comparison the note path uses — the text is compared, not the
    // identity, because a chapter's text is re-extracted when the chapter
    // changes.
    let book_is_open = app.tts_book.get()
        && crate::reader::read_along_target(app).is_some()
        && {
            let state = app.reader.borrow();
            let spoken = app.tts_text.borrow();
            !spoken.is_empty() && sn_core::util::stable_hash(&spoken) == sn_core::util::stable_hash(&state.text)
        };
    let foreign = progress.state != sn_tts::PlayState::Playing
        || !app.prefs.get().read_along_enabled
        || (!note_is_open && !book_is_open)
        || app.preview.get()
        || (note_is_open
            && app.open_note().map(|n| n.speech_hash()) != Some(app.tts_hash.get()));
    if foreign {
        clear_highlight(app);
        return;
    }
    // A resume re-plans from the frontier; the epoch says when to re-fetch.
    // The epoch read is atomic — copying the plan per tick would be the
    // per-tick O(plan) this UI avoids everywhere else.
    let cached_epoch = app.tts_plan.borrow().0;
    let session_epoch = app.tts.borrow().as_ref().map(|s| s.plan_epoch()).unwrap_or(0);
    if session_epoch != cached_epoch {
        if let Some(s) = app.tts.borrow().as_ref() {
            let (epoch, plan) = s.plan();
            *app.tts_plan.borrow_mut() = (epoch, plan.chunks);
        }
    }

    let plan = app.tts_plan.borrow();
    let chunks = &plan.1;
    // The first chunk whose end is past the frontier owns it — the same
    // answer as sn_tts's own `chunk_at`, reached by binary search: a linear
    // scan here would run ten times a second over every chunk of the note.
    let owner = chunks.partition_point(|c| c.start <= progress.frontier);
    let chunk = match owner.checked_sub(1).and_then(|i| chunks.get(i)) {
        Some(c) if progress.frontier < c.end() => c,
        _ => {
            clear_highlight(app);
            return;
        }
    };
    if app.tts_highlight.get() == (chunk.start as u32, chunk.end() as u32) {
        return; // same sentence: nothing to move
    }
    let text = app.tts_text.borrow();
    // GTK text iterators count CHARS; the plan counts BYTES (the standing
    // sn-ui rule). Done here, once per sentence change, never per tick.
    let start_char = text[..chunk.start].chars().count() as u32;
    let end_char = text[..chunk.end()].chars().count() as u32;
    drop(text);
    apply_highlight(app, start_char, end_char);
}

/// Where a highlight is painted: the editor's buffer or a reader's text view.
pub(crate) enum HighlightTarget {
    Editor,
    Reader(gtk4::TextView),
}

/// The buffer the highlight goes into, and the view it belongs to.
///
/// The reader's text view is whichever one is open — a PDF's extracted text
/// or an EPUB's read-along view. The scroll needs the view it scrolls, so
/// the two travel together.
pub(crate) fn highlight_buffer(app: &Rc<App>) -> Option<(gtk4::TextBuffer, HighlightTarget)> {
    if let Some(view) = crate::reader::read_along_target(app) {
        return Some((view.buffer(), HighlightTarget::Reader(view)));
    }
    let widgets = app.widgets.borrow().clone()?;
    Some((
        widgets.editor_text.buffer(),
        HighlightTarget::Editor,
    ))
}

pub(crate) fn apply_highlight(app: &Rc<App>, start_char: u32, end_char: u32) {
    let Some(widgets) = app.widgets.borrow().clone() else {
        return;
    };
    clear_highlight(app);
    let Some((buffer, target)) = highlight_buffer(app) else {
        return;
    };
    if buffer.tag_table().lookup(TAG_READ_ALONG).is_none() {
        let rgba = gtk4::gdk::RGBA::new(HIGHLIGHT.0, HIGHLIGHT.1, HIGHLIGHT.2, HIGHLIGHT.3);
        buffer.create_tag(Some(TAG_READ_ALONG), &[("background-rgba", &rgba)]);
    }
    let mut start = buffer.iter_at_offset(start_char as i32);
    let end = buffer.iter_at_offset(end_char as i32);
    buffer.apply_tag_by_name(TAG_READ_ALONG, &start, &end);
    app.tts_highlight.set((start_char, end_char));
    // Follow the voice; this runs on sentence changes only, so a long
    // sentence is not re-scrolling per tick.
    match target {
        HighlightTarget::Editor => {
            widgets.editor_text.scroll_to_iter(&mut start, 0.1, false, 0.0, 0.0);
        }
        HighlightTarget::Reader(view) => {
            view.scroll_to_iter(&mut start, 0.1, false, 0.0, 0.0);
        }
    }
}

pub(crate) fn clear_highlight(app: &Rc<App>) {
    let (start, end) = app.tts_highlight.get();
    if start == 0 && end == 0 {
        return;
    }
    app.tts_highlight.set((0, 0));
    let Some(widgets) = app.widgets.borrow().clone() else {
        return;
    };
    // Remove over the whole buffer, not the recorded range: GTK tags move
    // with edits, so after a mid-playback keystroke the tag can sit anywhere
    // and a range remove would leave painted characters behind.
    //
    // Both buffers are cleared: the note's editor and either reader text
    // view (a PDF's extracted text, an EPUB's read-along) may have carried
    // the tag, and a stale highlight on the page the listener is no longer
    // reading is a paint left behind.
    for buffer in [
        widgets.editor_text.buffer(),
        widgets.reader.pdf_text.buffer(),
        widgets.reader.epub_text.buffer(),
    ] {
        if buffer.tag_table().lookup(TAG_READ_ALONG).is_some() {
            buffer.remove_tag_by_name(TAG_READ_ALONG, &buffer.start_iter(), &buffer.end_iter());
        }
    }
    let _ = (start, end);
}

pub(crate) fn update_widgets(app: &Rc<App>, progress: &sn_tts::Progress) {
    let Some(widgets) = app.widgets.borrow().clone() else {
        return;
    };
    let player = &widgets.player;
    match progress.state {
        sn_tts::PlayState::Playing => {
            set_play_icon(app, true);
            // The bar is *live*: the fill carries the accent glow while the
            // voice moves it. The class is added and removed by the ticks
            // that already drive the fraction — no new timer, no per-frame
            // work of our own.
            if !player.progress.has_css_class("live") {
                player.progress.add_css_class("live");
            }
            let total = app.tts_plan.borrow().1.len().max(1);
            player.position.set_text(&format!(
                "{} / {} · {:.1}s",
                progress.chunks_done.min(total),
                total,
                progress.audio_total.as_secs_f64()
            ));
            // The transport bar's progress. A note speaks its own sentences;
            // a book chapter is placed in the whole book — the chapters
            // before it plus how far the frontier is through this one — so
            // chapter forty of seventy-three reads as a bit over half, not
            // as a percent of itself.
            let fraction = if app.tts_book.get() {
                let (chapter, text_len) = {
                    let state = app.reader.borrow();
                    (state.chapter, state.text.len().max(1))
                };
                let chapters = crate::reader::current_book(app)
                    .ok()
                    .flatten()
                    .map(|b| b.chapters())
                    .unwrap_or(0);
                if chapters == 0 {
                    0.0
                } else {
                    ((chapter as f64 + progress.frontier as f64 / text_len as f64)
                        / chapters as f64)
                        .clamp(0.0, 1.0)
                }
            } else {
                progress.chunks_done as f64 / total as f64
            };
            player.progress.set_fraction(fraction);
            if progress.errors > 0 {
                player
                    .status
                    .set_text(&format!("{} sentence(s) skipped", progress.errors));
            } else {
                player.status.set_text("Speaking");
            }
        }
        sn_tts::PlayState::Paused => {
            set_play_icon(app, false);
            player.progress.remove_css_class("live");
            player.status.set_text("Paused");
        }
        sn_tts::PlayState::Finished => {
            set_play_icon(app, false);
            player.progress.remove_css_class("live");
            player.status.set_text("Finished");
            player.progress.set_fraction(1.0);
            player
                .position
                .set_text(&format!("{:.1}s", progress.audio_total.as_secs_f64()));
        }
        sn_tts::PlayState::Stopped => {
            set_play_icon(app, false);
            player.progress.remove_css_class("live");
            player.status.set_text("Stopped");
        }
        sn_tts::PlayState::Failed => {
            set_play_icon(app, false);
            player.progress.remove_css_class("live");
            player.status.set_text(&format!(
                "Failed: {}",
                progress.error.as_deref().unwrap_or("unknown")
            ));
        }
    }
}
