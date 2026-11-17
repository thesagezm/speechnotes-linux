//! Jumping and settling: a jump is "done" only when the reader stands on
//! the page it asked for. Split out of `pdfview.rs` (Round 4, pure
//! code-motion).

use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;

use super::{Inner, PendingJump, SETTLE_MS, State};
use super::pages::{materialise, schedule_materialise};

pub(crate) fn page_at(state: &State, y: f64) -> usize {
    if state.sizes.is_empty() {
        return 0;
    }
    match state.prefix.iter().rposition(|top| *top <= y) {
        Some(page) => page.min(state.sizes.len() - 1),
        None => 0,
    }
}

/// The scroll settled: the reader's page is the one at the top, and the
/// pages around it are wanted.
pub(crate) fn settle(inner: &Rc<Inner>) {
    let adjustment = inner.scroll.vadjustment();
    let top = adjustment.value();
    // The furthest down the layout can scroll right now: a jump whose y is
    // past it has been CLAMPED by an upper that lags the layout a frame
    // behind (the pane just grew) — that is not the reader scrolling away.
    let max_y = adjustment.upper() - adjustment.page_size();
    let page = {
        let state = inner.state.borrow();
        page_at(&state, top)
    };
    let changed = {
        let mut state = inner.state.borrow_mut();
        // A jump is done when the reader stands on the page it asked for,
        // and abandoned when the reader scrolls away from it. A jump the
        // scroll is still on its way to is kept: the layout may yet move
        // under it and the next relayout will answer it again.
        state.pending = settle_pending(state.pending, page, top, max_y);
        // While a jump is in flight the scroll's position is the journey,
        // not the destination: the pages it passes are not ratified as the
        // reader's page — a clamp landing deep in the book for one frame
        // used to become `current` (and the saved position) right here.
        let owed = state.pending.as_ref().is_some_and(|p| p.page != page);
        let changed = !owed && state.current != page;
        if !owed {
            state.current = page;
        }
        changed
    };
    if changed {
        let callback = inner.on_page.borrow().clone();
        if let Some(callback) = callback {
            callback(page);
        }
    }
    materialise(inner);
}

/// Arm the settle timer: the reader's page changes when the scroll stops,
/// not while it moves.
pub(crate) fn arm_settle(inner: &Rc<Inner>) {
    {
        let mut pending = inner.settle.borrow_mut();
        if let Some(source) = pending.take() {
            source.remove();
        }
    }
    schedule_materialise(inner);
    let timer_inner = inner.clone();
    let source = glib::timeout_add_local(Duration::from_millis(SETTLE_MS), move || {
        *timer_inner.settle.borrow_mut() = None;
        settle(&timer_inner);
        glib::ControlFlow::Break
    });
    *inner.settle.borrow_mut() = Some(source);
}

/// What is left of a jump once the scroll has settled.
///
/// A jump is done when the reader stands on the page it asked for. It is
/// abandoned when the reader has scrolled away from where the jump aimed —
/// the reader's own scroll wins over a jump that was asked for before the
/// pane was measured. And it is kept when the scroll is still on its way
/// there: the layout may yet move under it, and the next relayout answers
/// it again.
///
/// The resize drift (2026-11-13): "on its way" now also covers the
/// clamp. The adjustment's upper lags the layout one frame behind, so a
/// jump aimed lower than the old upper was set, clamped, and the settle
/// read the clamp position as the reader scrolling away — the keep-page
/// jump of every pane growth (fullscreen, sidebar collapse) was abandoned
/// mid-flight and the clamp page ratified as the reader's. A jump whose y
/// sits past what the layout can scroll to right now is kept, not
/// abandoned; the adjustment's `changed` re-answers it once the upper
/// catches up.
pub(crate) fn settle_pending(
    pending: Option<PendingJump>,
    page: usize,
    top: f64,
    max_y: f64,
) -> Option<PendingJump> {
    let pending = pending?;
    if pending.page == page {
        return None;
    }
    if pending
        .y
        .is_some_and(|y| y <= max_y + 1.0 && (y - top).abs() > 1.0)
    {
        return None;
    }
    Some(pending)
}

/// Scroll the viewport to a page's top.
///
/// The y comes from the layout that exists *now*. If that layout is about
/// to change — the pane has not been measured yet, so fit-width has not
/// found its scale — the jump is owed, not done: [`PendingJump`] keeps the
/// target and the next layout answers it again.
pub(crate) fn scroll_to(inner: &Rc<Inner>, page: usize) {
    let y = match inner.state.borrow().prefix.get(page) {
        Some(y) => *y,
        None => return,
    };
    // The adjustment's upper bound is only known after the new layout has
    // been measured, so the scroll waits for the next idle turn — a scroll
    // to a page the scroller does not yet believe in is clamped to the old
    // end of the book.
    let scroll = inner.scroll.clone();
    glib::idle_add_local_once(move || {
        scroll.vadjustment().set_value(y);
    });
}

/// Perform a jump and remember where it aimed.
///
/// The y it used is recorded, so the settle can tell a landing on the
/// wrong page (the layout moved under the jump) apart from the reader
/// scrolling away (the jump is abandoned).
pub(crate) fn perform_jump(inner: &Rc<Inner>, page: usize) {
    let y = inner.state.borrow().prefix.get(page).copied();
    if y.is_none() {
        // The layout does not know this page yet: the jump stays owed.
        let mut state = inner.state.borrow_mut();
        state.pending = Some(PendingJump { page, y: None });
        return;
    }
    {
        let mut state = inner.state.borrow_mut();
        state.pending = Some(PendingJump { page, y });
    }
    scroll_to(inner, page);
}

/// Answer a jump that is still owed, in the layout that is real now.
///
/// Called after every relayout AND from the adjustment's `changed` — the
/// upper catching up a frame behind the layout is the moment a clamped
/// scroll becomes completable, and nothing else re-fires here.
pub(crate) fn answer_pending_jump(inner: &Rc<Inner>) {
    let pending = {
        let state = inner.state.borrow();
        state.pending
    };
    let Some(pending) = pending else {
        return;
    };
    let y = match inner.state.borrow().prefix.get(pending.page) {
        Some(y) => *y,
        None => return,
    };
    // The jump's y is still the answer AND the scroll is parked on it:
    // nothing to do. The same y with the scroll clamped short of it (the
    // adjustment lagged when the jump was set) is re-set — that is the
    // completion the `changed` hook exists for.
    let adjustment = inner.scroll.vadjustment();
    if pending.y == Some(y) && (adjustment.value() - y).abs() <= 1.0 {
        return;
    }
    perform_jump(inner, pending.page);
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    use super::super::{FitMode, PAGE_GAP};
    use super::*;


    /// A jump is done when the reader stands on the page it asked for.
    #[test]
    fn a_jump_is_done_on_its_page() {
        let pending = PendingJump { page: 35, y: Some(100.0) };
        assert!(settle_pending(Some(pending), 35, 100.0, 10_000.0).is_none());
    }

    /// A jump the reader has scrolled away from is abandoned: the reader's
    /// own scroll wins.
    #[test]
    fn a_jump_is_abandoned_when_the_reader_scrolls_away() {
        let pending = PendingJump { page: 35, y: Some(100.0) };
        assert!(settle_pending(Some(pending), 36, 5000.0, 10_000.0).is_none());
    }

    /// A jump the scroll is still on its way to is kept: the layout may
    /// yet move under it, and the next relayout answers it again.
    #[test]
    fn a_jump_on_its_way_is_kept() {
        let pending = PendingJump { page: 35, y: Some(100.0) };
        assert!(settle_pending(Some(pending), 34, 100.0, 10_000.0).is_some());
    }

    /// A jump that was never performed — the layout did not know the page
    /// yet — is kept until it can be.
    #[test]
    fn a_jump_with_no_y_is_kept() {
        let pending = PendingJump { page: 35, y: None };
        assert!(settle_pending(Some(pending), 0, 0.0, 10_000.0).is_some());
    }

    /// The resize drift (2026-11-13): a jump the adjustment could
    /// not reach yet — the upper lags the layout one frame behind a pane
    /// growth — is CLAMPED, not abandoned: the settle must not read the
    /// clamp as the reader scrolling away, or the keep-page of every
    /// fullscreen/sidebar change lands pages off and is ratified there.
    #[test]
    fn a_clamped_jump_is_kept() {
        let pending = PendingJump { page: 35, y: Some(5_000.0) };
        assert!(settle_pending(Some(pending), 90, 4_000.0, 4_200.0).is_some());
    }

    /// The page at a y in the document: the prefix sum is the truth.
    #[test]
    fn a_page_is_found_by_its_prefix() {
        let mut state = State {
            book_id: String::new(),
            pdf: PathBuf::new(),
            sizes: vec![(612.0, 792.0); 10],
            slots: vec![(100.0, 200.0); 10],
            prefix: Vec::new(),
            total: 0.0,
            scale: 1.0,
            fit: FitMode::Width,
            dsf: 1.0,
            dpi: 96,
            viewport: (0.0, 0.0),
            rows: HashMap::new(),
            requested: HashSet::new(),
            epoch: 0,
            current: 0,
            pending: None,
            links: Vec::new(),
        };
        let mut y = 0.0;
        for _ in 0..10 {
            state.prefix.push(y);
            y += 200.0 + PAGE_GAP;
        }
        assert_eq!(page_at(&state, 0.0), 0);
        // A page owns the space from its own top to the next page's top.
        assert_eq!(page_at(&state, 201.0), 0);
        assert_eq!(page_at(&state, 212.0), 1);
        assert_eq!(page_at(&state, 213.0), 1);
        assert_eq!(page_at(&state, 424.0), 2);
    }
}
