//! The block walker: one GTK row per markdown block, nested lists get
//! indentation. Split out of `render.rs` (Round 4, pure code-motion).

use gtk4 as gtk;
use gtk4::prelude::*;

use sn_format::markdown::{AlertKind, Block};

use super::images::image_widget;
use super::inlines::{
    escape, markup_label, markup_label_for_task, markup_label_with_marker, split_images,
};
use super::tables::table_widget;
use super::RenderCtx;

pub(crate) fn render_block(block: &Block, ctx: &RenderCtx, out: &gtk::Box) {
    match block {
        //  no arm for the DisplayMath block ever original (the original
        // write_inline fragment covers only the inline side), so this one is
        // authored to the contract its producer pinned: markdown.rs's doc
        // comment — a display formula is *centred, on its own, with space
        // above and below* — and the original inline treatment of a display
        // formula (large, italic, the Unicode form, not the LaTeX source).
        Block::DisplayMath { source } => {
            let body = gtk::Label::builder()
                .wrap(true)
                .xalign(0.5)
                .justify(gtk::Justification::Center)
                .margin_top(6)
                .margin_bottom(6)
                .selectable(true)
                .build();
            body.set_markup(&format!(
                "<span size=\"large\" style=\"italic\">{}</span>",
                escape(&sn_format::math::to_unicode(source))
            ));
            out.append(&body);
        }
        Block::Heading { level, inlines } => {
            let label = markup_label(inlines);
            label.add_css_class(match level {
                1 => "md-h1",
                2 => "md-h2",
                3 => "md-h3",
                _ => "md-h4",
            });
            out.append(&label);
        }
        Block::Paragraph { inlines } => {
            let (runs, images) = split_images(inlines);
            if !runs.is_empty() {
                // `md-p` is what carries the preview's line height and the
                // space between paragraphs. A table cell is a different
                // class on purpose: a cell has its own padding and borders,
                // and the gap that suits a paragraph would make a table look
                // broken.
                let label = markup_label(&runs);
                label.add_css_class("md-p");
                out.append(&label);
            }
            for image in images {
                out.append(&image_widget(&image, ctx));
            }
        }
        Block::ListItem {
            depth,
            ordered,
            marker,
            inlines,
        } => {
            // One label, marker and text together. A row of three widgets
            // (a box, a marker label, a text label) puts a wrapping label
            // inside a horizontal box, and GTK4 re-measures that label at
            // the width the box finally gives it — milliseconds per row, and
            // a big note is mostly list rows. See `markup_label_with_marker`.
            let (runs, images) = split_images(inlines);
            let marker_text = if *ordered { marker.as_str() } else { "•" };
            let label = markup_label_with_marker(marker_text, &runs);
            set_indent(label.upcast_ref(), *depth);
            out.append(&label);
            for image in images {
                out.append(&image_widget(&image, ctx));
            }
        }
        Block::TaskItem { depth, done, inlines } => {
            // The preview's checkbox was never interactive (it was drawn
            // insensitive, because the caret lives in the editor), so the
            // glyph in the label says the same thing for far less layout.
            let (runs, images) = split_images(inlines);
            let label = markup_label_for_task(*done, &runs);
            set_indent(label.upcast_ref(), *depth);
            out.append(&label);
            for image in images {
                out.append(&image_widget(&image, ctx));
            }
        }
        Block::Alert { kind, title, blocks } => {
            // GitHub's alert boxes: a tinted panel with a coloured edge and
            // the kind as its title, so `> [!WARNING]` reads as a warning and
            // not as a quote with brackets in it.
            let panel = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(6)
                .build();
            panel.add_css_class("md-alert");
            panel.add_css_class(match kind {
                AlertKind::Note => "md-alert-note",
                AlertKind::Tip => "md-alert-tip",
                AlertKind::Important => "md-alert-important",
                AlertKind::Warning => "md-alert-warning",
                AlertKind::Caution => "md-alert-caution",
            });
            let head = gtk::Label::builder()
                .label(title.clone().unwrap_or_else(|| kind.title().to_string()))
                .halign(gtk::Align::Start)
                .build();
            head.add_css_class("md-alert-title");
            panel.append(&head);
            for inner in blocks {
                render_block(inner, ctx, &panel);
            }
            out.append(&panel);
        }
        Block::Quote { blocks } => {
            let bar = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(8)
                .build();
            bar.add_css_class("md-quote");
            for inner in blocks {
                render_block(inner, ctx, &bar);
            }
            out.append(&bar);
        }
        Block::FootnoteDefinition { label, blocks } => {
            // A footnote reads as a small, dim, indented block led by its
            // marker — the shape readers expect at the foot of a document.
            let footnote = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .margin_start(12)
                .build();
            footnote.add_css_class("md-footnote");
            let head = gtk::Label::builder()
                .label(format!("[{label}]"))
                .halign(gtk::Align::Start)
                .build();
            head.add_css_class("md-footnote-label");
            footnote.append(&head);
            for inner in blocks {
                render_block(inner, ctx, &footnote);
            }
            out.append(&footnote);
        }
        Block::Definition { term, blocks } => {
            // Description-list entry: the term carries the weight, the
            // description hangs under it.
            let entry = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .build();
            entry.add_css_class("md-definition");
            let term_label = markup_label(term);
            term_label.add_css_class("md-def-term");
            entry.append(&term_label);
            for inner in blocks {
                render_block(inner, ctx, &entry);
            }
            entry.set_margin_start(16);
            out.append(&entry);
        }
        Block::Code { language, text } => {
            let box_ = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .build();
            if let Some(lang) = language {
                if !lang.is_empty() {
                    let tag = gtk::Label::new(Some(lang.as_str()));
                    tag.add_css_class("md-code-lang");
                    box_.append(&tag);
                }
            }
            let body = gtk::Label::builder()
                .label(text.as_str())
                .wrap(true)
                .xalign(0.0)
                .selectable(true)
                .build();
            body.add_css_class("md-code");
            box_.append(&body);
            out.append(&box_);
        }
        Block::Rule => {
            let rule = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            rule.add_css_class("md-rule");
            out.append(&rule);
        }
        Block::Table {
            header,
            rows,
            alignments,
        } => {
            out.append(&table_widget(header, rows, alignments, ctx));
        }
        Block::Raw(text) => {
            // Kept verbatim and visibly so: a construct this renderer does not
            // understand must look unrendered, never silently vanish.
            let label = gtk::Label::builder()
                .label(text.as_str())
                .wrap(true)
                .xalign(0.0)
                .selectable(true)
                .build();
            label.add_css_class("md-raw");
            out.append(&label);
        }
    }
}

/// Indent a list row by its nesting depth.
pub(crate) fn set_indent(row: &gtk::Widget, depth: usize) {
    row.set_margin_start(18 + (depth as i32) * 20);
}

