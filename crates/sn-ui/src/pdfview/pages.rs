//! Row materialisation: the rows in the window exist as widgets, the rest
//! is the spacer's height. Split out of `pdfview.rs` (Round 4, pure
//! code-motion).

use std::rc::Rc;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;

use super::layout::{place_rows, visible_range};
use super::worker::cache_path;
use super::worker::Job;
use super::{Inner, PageRow, MATERIALISE_MS};

pub(crate) fn materialise(inner: &Rc<Inner>) {
    let (first, last) = visible_range(inner);
    let count = inner.state.borrow().sizes.len();
    if count == 0 {
        return;
    }
    let keep_from = first.saturating_sub(4);
    let keep_to = (last + 4).min(count - 1);
    {
        let mut state = inner.state.borrow_mut();
        let stale: Vec<usize> = state
            .rows
            .keys()
            .copied()
            .filter(|p| *p < keep_from || *p > keep_to)
            .collect();
        for page in stale {
            if let Some(row) = state.rows.remove(&page) {
                inner.fixed.remove(&row.frame);
            }
            // The row is gone, and the row carried the page's picture — so
            // the flag that says "this page has been asked for" must go with
            // it. It did not, and that is the bug this fixes: the page
            // scrolled out of range, its row was dropped, and the flag
            // stayed set. Scrolling back rebuilt an **empty** row, and
            // `request_pages` saw the flag and never asked for the render
            // again — so the page came back blank for good. The voice was
            // unaffected (it reads the OCR cache and the text layer, not
            // the raster), which is exactly how it was reported: the page
            // would not draw, but the TTS read it.
            let dpi = state.dpi;
            state.requested.remove(&(page, dpi));
        }
    }
    let from = first.saturating_sub(2);
    let to = (last + 2).min(count - 1);
    for page in from..=to {
        ensure_row(inner, page);
    }
    place_rows(inner);
    request_pages(inner, from, to);
}

/// A row for a page, if it does not have one yet.
pub(crate) fn ensure_row(inner: &Rc<Inner>, page: usize) {
    let (slot, y) = {
        let state = inner.state.borrow();
        let slot = match state.slots.get(page) {
            Some(s) => *s,
            None => return,
        };
        let y = state.prefix[page];
        (slot, y)
    };
    if inner.state.borrow().rows.contains_key(&page) {
        return;
    }
    let frame = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .css_classes(["pdf-page"])
        .build();
    frame.set_size_request(slot.0.round() as i32, slot.1.round() as i32);
    let picture = gtk4::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk4::ContentFit::Contain)
        .hexpand(true)
        .vexpand(true)
        .build();
    // The picture asks for its slot: the raster is drawn at the page's own
    // size in the pane, so the picture's natural size and the slot are the
    // same number and the page is drawn at the size it was laid out for.
    picture.set_size_request(slot.0.round() as i32, slot.1.round() as i32);
    frame.append(&picture);
    let canvas_w = {
        let state = inner.state.borrow();
        state
            .slots
            .iter()
            .map(|(w, _)| *w)
            .fold(0.0_f64, f64::max)
            .max(state.viewport.0)
    };
    let x = ((canvas_w - slot.0) / 2.0).max(0.0);
    inner.fixed.put(&frame, x, y);
    inner
        .state
        .borrow_mut()
        .rows
        .insert(page, PageRow { frame, picture });
}

/// Ask the worker for the pages' images.
pub(crate) fn request_pages(inner: &Rc<Inner>, from: usize, to: usize) {
    let jobs: Vec<Job> = {
        let state = inner.state.borrow();
        if state.book_id.is_empty() || state.pdf.as_os_str().is_empty() {
            return;
        }
        let mut jobs = Vec::new();
        for page in from..=to {
            let key = (page, state.dpi);
            if state.requested.contains(&key) {
                continue;
            }
            jobs.push(Job {
                pdf: state.pdf.clone(),
                page: page + 1,
                dpi: state.dpi,
                out: cache_path(&state.book_id, state.dpi, page + 1),
                epoch: state.epoch,
            });
        }
        jobs
    };
    for job in jobs {
        {
            let mut state = inner.state.borrow_mut();
            let key = (job.page - 1, job.dpi);
            if !state.requested.insert(key) {
                continue;
            }
        }
        if inner.jobs.send(job).is_err() {
            break;
        }
    }
}

/// The worker's answers: the page's image goes into its row.
pub(crate) fn collect_done(inner: &Rc<Inner>) {
    loop {
        let message = match inner.done.borrow_mut().try_recv() {
            Ok(message) => message,
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
        };
        let page = message.page.saturating_sub(1);
        // An answer from an older zoom or another book is for a page nobody
        // is looking at: it is dropped, not drawn.
        if message.epoch != inner.state.borrow().epoch {
            continue;
        }
        let texture = message
            .path
            .as_ref()
            .and_then(|path| gtk4::gdk::Texture::from_filename(path).ok());
        let row = {
            let state = inner.state.borrow();
            state.rows.get(&page).map(|row| row.picture.clone())
        };
        let Some(picture) = row else {
            continue;
        };
        match texture {
            Some(texture) => picture.set_paintable(Some(&texture)),
            None => {
                if let Some(error) = message.error {
                    tracing::warn!(page, error, "a page could not be drawn");
                }
            }
        }
    }
}

/// The scroll moved: the pages around the new position are wanted soon.
pub(crate) fn schedule_materialise(inner: &Rc<Inner>) {
    let inner = inner.clone();
    glib::timeout_add_local(Duration::from_millis(MATERIALISE_MS), move || {
        materialise(&inner);
        glib::ControlFlow::Break
    });
}
