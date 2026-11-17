//! PDF layout: the scroll geometry is a prefix sum over the known page
//! boxes — nothing is measured after it renders. Split out of `pdfview.rs`
//! (Round 4, pure code-motion).

use std::rc::Rc;

use gtk4::prelude::*;

use super::pages::materialise;
use super::jump::{answer_pending_jump, page_at};
use super::{FitMode, Inner, PendingJump, State, MAX_DPI, MIN_DPI, PAGE_GAP, PAGE_MARGIN, SCROLLBAR_ALLOWANCE};

pub(crate) fn raster_px(points: f64, dpi: u32) -> f64 {
    (points * dpi as f64 / 72.0).ceil()
}

/// The pane's size in logical pixels, read from the viewport that holds the
/// canvas.
pub(crate) fn viewport_size(inner: &Rc<Inner>) -> (f64, f64) {
    // The scroll window's own allocation is the pane: the viewport between
    // it and the canvas is as wide as the canvas (it scrolls sideways), so
    // asking the viewport how wide the pane is answers the wrong question.
    (
        inner.scroll.width().max(0) as f64,
        inner.scroll.height().max(0) as f64,
    )
}

/// The pane's size changed: the fit modes answer to it, so the pages are
/// laid out again.
pub(crate) fn sync_viewport(inner: &Rc<Inner>) {
    let size = viewport_size(inner);
    let changed = {
        let state = inner.state.borrow();
        (size.0 - state.viewport.0).abs() > 0.5 || (size.1 - state.viewport.1).abs() > 0.5
    };
    if !changed {
        return;
    }
    {
        let mut state = inner.state.borrow_mut();
        state.viewport = size;
    }
    let fit = inner.state.borrow().fit;
    if fit != FitMode::Free {
        // The pane grew or shrank: fit-width and fit-page rescale every
        // page, so the scroll's old absolute y lands on a different page.
        // The reader stands where they stood — the current page is kept.
        relayout(inner, true);
    }
    materialise(inner);
}

/// Recompute the layout: the DPI, each page's slot, the prefix sums, and the
/// rows' positions.
///
/// A DPI change throws the rows away: their textures are the old zoom's, and
/// a stale texture stretched into a new slot is a blurry page.
pub(crate) fn relayout(inner: &Rc<Inner>, keep_page: bool) {
    let page = inner.state.borrow().current;
    let dsf = inner.scroll.scale_factor().max(1) as f64;
    let dpi_changed = {
        let mut state = inner.state.borrow_mut();
        state.dsf = dsf;
        state.viewport = viewport_size(inner);
        // The fit modes compute the scale from the pane; the free mode keeps
        // the scale the user chose.
        match state.fit {
            FitMode::Width => {
                if let Some(scale) = fit_width_scale(&state) {
                    state.scale = scale;
                }
            }
            FitMode::Page => {
                if let Some(scale) = fit_page_scale(&state) {
                    state.scale = scale;
                }
            }
            FitMode::Free => {}
        }
        // The DPI is the page's own size in the pane: a page 612 points
        // wide shown 447 pixels wide is 52.6 dpi, and the raster is drawn
        // at exactly that so the picture's own size and the slot it goes in
        // are the same number. A raster drawn larger would be drawn larger
        // than the slot — `GtkFixed` gives a child its natural size — and
        // the page would be cropped instead of fitted.
        let dpi = (72.0 * state.scale)
            .round()
            .clamp(MIN_DPI as f64, MAX_DPI as f64) as u32;
        let changed = dpi != state.dpi;
        state.dpi = dpi;
        state.slots.clear();
        state.prefix.clear();
        let mut y = 0.0;
        let sizes = state.sizes.clone();
        for (w, h) in &sizes {
            state.prefix.push(y);
            let slot = (raster_px(*w, dpi), raster_px(*h, dpi));
            state.slots.push(slot);
            y += slot.1 + PAGE_GAP;
        }
        if !state.sizes.is_empty() {
            // The gap after the last page is not part of the book.
            y -= PAGE_GAP;
        }
        state.total = y.max(0.0);
        changed
    };
    if dpi_changed {
        // The rows hold the old zoom's images.
        let mut state = inner.state.borrow_mut();
        state.requested.clear();
        let pages: Vec<usize> = state.rows.keys().copied().collect();
        for page in pages {
            if let Some(row) = state.rows.remove(&page) {
                inner.fixed.remove(&row.frame);
            }
        }
    }
    // The canvas is as wide as the widest page: a page wider than the pane
    // scrolls sideways (a PDF viewer does the same), and a page narrower
    // than the pane is centred inside the pane rather than in a canvas as
    // wide as the pane — which is what put every page hard against the
    // left edge.
    let (canvas_w, total) = {
        let state = inner.state.borrow();
        let widest = state
            .slots
            .iter()
            .map(|(w, _)| *w)
            .fold(0.0_f64, f64::max);
        // The canvas is the pane, or the widest page when a zoomed-in page
        // is wider than the pane — which is what makes the page scroll
        // sideways instead of being cropped.
        (widest.max(state.viewport.0), state.total)
    };
    inner
        .spacer
        .set_size_request(canvas_w.round() as i32, total.round() as i32);
    place_rows(inner);
    if keep_page {
        // A jump the reader asked for but the layout has not answered yet
        // wins over keeping the page: this layout answers the jump, and a
        // keep-page scroll queued behind it would overwrite the answer.
        let owed = inner.state.borrow().pending.is_some();
        if !owed {
            // The resize drift (2026-11-13): the keep-page is
            // registered as a pending jump, not scrolled directly. A direct
            // scroll was one `idle` behind the pane's growth — the
            // adjustment's upper only catches up at the next frame's
            // allocation — so the set-value clamped to the old, shorter
            // book and the scroll landed pages away from `page`; the next
            // settle ratified the clamp as the reader's page (60 → 90 on a
            // fullscreen, 60 → 40 on a sidebar collapse). As a pending
            // jump it survives the flight, and the adjustment's `changed`
            // (the upper catching up) answers it the rest of the way.
            inner.state.borrow_mut().pending = Some(PendingJump { page, y: None });
        }
    }
    // A jump that was asked for before this layout is answered again in it.
    answer_pending_jump(inner);
}

/// The scale at which the widest page fills the pane's width.
pub(crate) fn fit_width_scale(state: &State) -> Option<f64> {
    let widest = state.sizes.iter().map(|(w, _)| *w).fold(0.0_f64, f64::max);
    if widest <= 0.0 || state.viewport.0 <= 0.0 {
        return None;
    }
    Some((usable_width(state.viewport.0)) / widest)
}

/// The width a page may use: the pane less the margins and the vertical
/// scrollbar.
///
/// The scrollbar is the part that bites: the pane is measured *before* the
/// scrollbar appears, so a page fitted to the pane's full width is one
/// scrollbar too wide, and the reader sees the page clipped at the right
/// edge instead of fitted.
pub(crate) fn usable_width(pane: f64) -> f64 {
    (pane - 2.0 * PAGE_MARGIN - SCROLLBAR_ALLOWANCE).max(120.0)
}

/// The scale at which the tallest page fits the pane's height.
pub(crate) fn fit_page_scale(state: &State) -> Option<f64> {
    let widest = state.sizes.iter().map(|(w, _)| *w).fold(0.0_f64, f64::max);
    let tallest = state.sizes.iter().map(|(_, h)| *h).fold(0.0_f64, f64::max);
    if widest <= 0.0 || tallest <= 0.0 || state.viewport.0 <= 0.0 {
        return None;
    }
    let by_width = usable_width(state.viewport.0) / widest;
    let by_height = if state.viewport.1 > 0.0 {
        ((state.viewport.1 - 2.0 * PAGE_MARGIN).max(120.0)) / tallest
    } else {
        by_width
    };
    Some(by_width.min(by_height))
}

/// Put the materialised rows where the layout says they belong.
///
/// The canvas is a `GtkFixed`, which centres nothing: a row is placed at the
/// x it should sit at, which is the margin either side of the page.
pub(crate) fn place_rows(inner: &Rc<Inner>) {
    // The canvas is as wide as the widest page, so a page is centred in the
    // *viewport* and then placed in the canvas at the viewport's own offset
    // — the canvas scrolls horizontally when the page is wider than the
    // pane, and a page narrower than the pane sits in the middle of what
    // the reader sees, not in the middle of a canvas wider than the window.
    let (canvas_w, pages): (f64, Vec<(usize, (f64, f64), f64)>) = {
        let state = inner.state.borrow();
        let canvas_w = state
            .slots
            .iter()
            .map(|(w, _)| *w)
            .fold(0.0_f64, f64::max)
            .max(state.viewport.0);
        let pages = state
            .rows
            .keys()
            .copied()
            .filter_map(|page| {
                let slot = *state.slots.get(page)?;
                let y = *state.prefix.get(page)?;
                Some((page, slot, y))
            })
            .collect();
        (canvas_w, pages)
    };
    for (page, (slot_w, slot_h), y) in pages {
        // The page is centred in the canvas, which is the pane in
        // fit-width and the document's width when a page is zoomed in past
        // the pane. That is what every PDF viewer does: the page sits in
        // the middle of the window until it is too big for it.
        let x = ((canvas_w - slot_w) / 2.0).max(0.0);
        let row = {
            let state = inner.state.borrow();
            state.rows.get(&page).map(|row| row.frame.clone())
        };
        if let Some(frame) = row {
            frame.set_size_request(slot_w.round() as i32, slot_h.round() as i32);
            inner.fixed.move_(&frame, x, y);
        }
    }
}

/// The pages the window shows: the first whose bottom is past the top of the
/// viewport, and the last whose top is above its bottom.
pub(crate) fn visible_range(inner: &Rc<Inner>) -> (usize, usize) {
    let state = inner.state.borrow();
    if state.sizes.is_empty() {
        return (0, 0);
    }
    let adjustment = inner.scroll.vadjustment();
    let top = adjustment.value();
    let bottom = top + adjustment.page_size().max(1.0);
    let first = page_at(&state, top);
    let mut last = first;
    while last + 1 < state.sizes.len() {
        let next_top = state.prefix[last + 1];
        if next_top >= bottom {
            break;
        }
        last += 1;
    }
    (first, last)
}
