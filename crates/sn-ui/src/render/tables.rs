//! Tables: a bordered grid with a bold header row, wrapping cells, and
//! columns that share the width. Split out of `render.rs` (Round 4, pure
//! code-motion).

use gtk4 as gtk;
use gtk4::prelude::*;

use sn_format::markdown::{Align, Inline};

use super::images::image_widget;
use super::inlines::{markup_label, split_images};
use super::RenderCtx;

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// A bordered grid: a real table, not a stack of labels.
///
/// The grid is a single-pixel grid because spacing between cells is zero and
/// every cell draws only its right and bottom edge; row 0 adds a top edge and
/// column 0 a left edge. That yields one line between neighbours instead of the
/// doubled line that per-cell borders on both sides would give.
pub(crate) fn table_widget(
    header: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    alignments: &[Align],
    ctx: &RenderCtx,
) -> gtk::ScrolledWindow {
    let column_count = header
        .len()
        .max(rows.iter().map(|r| r.len()).max().unwrap_or(0))
        .max(1);

    let grid = gtk::Grid::builder()
        .column_spacing(0)
        .row_spacing(0)
        .hexpand(true)
        .build();
    grid.add_css_class("md-table");

    // A sensible minimum per column from its longest single word, so a long
    // word is never clipped and a short table does not stretch to full width.
    let mins = column_minimums(header, rows, column_count);

    // One alignment per column, from the delimiter row. A ragged table pads
    // missing columns with the source's default (None → left).
    let align_of = |x: usize| -> Align {
        alignments.get(x).copied().unwrap_or(Align::None)
    };

    let mut y = 0i32;
    if !header.is_empty() {
        for x in 0..column_count as i32 {
            let cell = table_cell(
                &header.get(x as usize).cloned().unwrap_or_default(),
                mins[x as usize],
                align_of(x as usize),
                true,
                ctx,
            );
            grid.attach(&cell, x, y, 1, 1);
        }
        y += 1;
    }
    for row in rows {
        for x in 0..column_count as i32 {
            let cell = table_cell(
                &row.get(x as usize).cloned().unwrap_or_default(),
                mins[x as usize],
                align_of(x as usize),
                false,
                ctx,
            );
            grid.attach(&cell, x, y, 1, 1);
        }
        y += 1;
    }

    // Horizontal scrolling only when the table genuinely cannot fit; below that
    // the grid takes the width it is given and the cells wrap.
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .child(&grid)
        .build()
}

/// One cell: wrapping, selectable, and edge-aware so the grid is single-pixel.
///
/// A cell that holds only an image becomes a small vertical box — the image
/// under its alt text — because dropping it would lose a word of the note's
/// content and showing only the alt text would hide the picture a reader can
/// see in the source. Either way the grid position is unchanged.
pub(crate) fn table_cell(
    inlines: &[Inline],
    min_chars: usize,
    align: Align,
    is_header: bool,
    ctx: &RenderCtx,
) -> gtk::Widget {
    let (runs, images) = split_images(inlines);
    let runs = if runs.is_empty() {
        vec![Inline::Text(" ".to_string())]
    } else {
        runs
    };
    let label = markup_label(&runs);
    label.set_width_chars(min_chars as i32);
    // The delimiter row's colons decide alignment: `:---:` centres, `---:`
    // right-aligns. The old renderer parsed these and threw them away.
    label.set_xalign(match align {
        Align::Center => 0.5,
        Align::Right => 1.0,
        Align::None | Align::Left => 0.0,
    });
    label.add_css_class("md-cell");
    if is_header {
        label.add_css_class("md-cell-head");
    }
    if images.is_empty() {
        return label.upcast();
    }

    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    box_.append(&label);
    for image in &images {
        box_.append(&image_widget(image, ctx));
    }
    box_.upcast()
}

/// Per-column wrap width in characters.
///
/// **The width must be measured from the longest *cell*, not the longest
/// *word*, and the reason is a GTK one that costs a hundred blank pixels if you
/// get it wrong.** `set_width_chars` on a wrapping label does two things: it sets
/// the minimum width *and* it fixes the width the label wraps at, so the
/// label's natural height is computed at that width. The table's scroller
/// propagates natural height, so it reserves exactly that.
///
/// Sizing a column by its longest word made "small models, needs a download"
/// reserve four wrapped lines while the column — which is wider, because the
/// grid expands — rendered it on one. The reserved four lines became a tall
/// empty gap under the table. Sizing by the longest cell keeps the reserved
/// height equal to the height actually used: when the table fits, every column
/// is at least this wide, so the wrap width is the real one; when the table is
/// too wide and scrolls, each column is *exactly* this wide, so it is again the
/// real one.
///
/// The cap stops one long sentence from stretching the table sideways, which
/// is the layout the iOS build had and the reason this one is not that.
pub(crate) fn column_minimums(
    header: &[Vec<Inline>],
    rows: &[Vec<Vec<Inline>>],
    column_count: usize,
) -> Vec<usize> {
    (0..column_count)
        .map(|c| {
            let mut longest = 1usize;
            let mut measure = |inlines: &[Inline]| {
                let text: String = inlines.iter().map(|i| i.plain()).collect();
                // Longest line, so a cell that already wraps is not counted
                // twice over.
                for line in text.split('\n') {
                    longest = longest.max(line.chars().count());
                }
            };
            if let Some(h) = header.get(c) {
                measure(h);
            }
            for row in rows {
                if let Some(v) = row.get(c) {
                    measure(v);
                }
            }
            longest.clamp(8, 36)
        })
        .collect()
}

