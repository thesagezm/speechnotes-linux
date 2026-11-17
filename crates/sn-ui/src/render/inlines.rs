//! Inlines: Pango markup for styled runs, one wrapping label per block.
//! Split out of `render.rs` (Round 4, pure code-motion).

use gtk4::glib;
use gtk4 as gtk;

use sn_format::markdown::Inline;

// ---------------------------------------------------------------------------
// Inlines
// ---------------------------------------------------------------------------

/// A wrapping, selectable label carrying Pango markup.
///
/// One label per block rather than one per run: a GTK4 box does not flow its
/// children, so a per-run layout either needs a flow container the toolkit does
/// not ship or a custom measure. Pango already wraps across styled runs, and
/// it wraps words between runs, which is what a reader expects.
pub(crate) fn markup_label(inlines: &[Inline]) -> gtk::Label {
    markup_label_with_markup(&pango_markup(inlines))
}

/// A wrapping, selectable label whose text starts with a dimmed marker.
///
/// The marker rides *inside* the label rather than beside it in a box, and
/// that is a performance decision, not a cosmetic one. GTK4 measures a
/// wrapping label that sits inside a horizontal box twice — once for the
/// box's natural width, then again at the width the box finally hands it —
/// and the re-wrap costs milliseconds per row. Measured on a 1130-block
/// note: the same tree drawn as 730 three-widget rows stalls the main loop
/// for 6.9 s, and drawn as one label per block for 0.49 s. A big note is
/// mostly list items, so the row shape was the whole cost of opening one.
pub(crate) fn markup_label_with_marker(marker: &str, inlines: &[Inline]) -> gtk::Label {
    markup_label_with_markup(&marked_markup(marker, false, inlines))
}

/// A task item's label: the checkbox and its text, in one label.
///
/// The preview's checkbox was never interactive — it was drawn insensitive,
/// because the caret lives in the editor and a live checkbox here would write
/// to a buffer this pane does not own. A glyph says the same thing for a
/// fraction of the layout cost, and a done item keeps the dimmed,
/// struck-through look the `md-done` class gave it.
pub(crate) fn markup_label_for_task(done: bool, inlines: &[Inline]) -> gtk::Label {
    let marker = if done { "☑" } else { "☐" };
    markup_label_with_markup(&marked_markup(marker, done, inlines))
}

/// The markup for one marker-led row: the dimmed marker, then the text.
///
/// `dim_text` is a done task item's treatment — dimmed and struck through,
/// the look the `md-done` class used to give the body label.
fn marked_markup(marker: &str, dim_text: bool, inlines: &[Inline]) -> String {
    let text = pango_markup(inlines);
    if dim_text {
        format!(
            "<span alpha=\"55%\">{}</span> <span alpha=\"55%\"><s>{}</s></span>",
            escape(marker),
            text
        )
    } else {
        format!("<span alpha=\"55%\">{}</span> {}", escape(marker), text)
    }
}

/// The body both label builders share: one wrapping, selectable label.
fn markup_label_with_markup(markup: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .yalign(0.0)
        .selectable(true)
        .build();
    // `set_markup`, not `set_text` + `set_use_markup`. In GTK 4
    // `gtk_label_set_text()` clears the use-markup flag, so the order
    // set_use_markup-then-set_text left every label with markup *off* and the
    // note's `<b>bold</b>` on screen as literal characters. `set_markup` sets
    // the text and the flag together, so the order cannot matter.
    label.set_markup(markup);

    // Links open in the user's browser, not inside the app. The scheme is
    // checked first: without it a note containing `[click](file:///…)` or a
    // handler-only scheme would make the desktop open something on click, and
    // a note is untrusted text the moment it can be imported.
    label.connect_activate_link(move |_, href| {
        let href = href.to_string();
        if !is_safe_link(&href) {
            tracing::warn!(href = %href, "refusing to open a link with an unlisted scheme");
            return glib::Propagation::Stop;
        }
        gtk::UriLauncher::new(&href).launch(
            gtk::Window::NONE,
            gtk::gio::Cancellable::NONE,
            |result| {
                if let Err(e) = result {
                    tracing::warn!(error = %e, "the desktop refused to open a link");
                }
            },
        );
        glib::Propagation::Stop
    });
    label
}

/// The link schemes a note is allowed to hand to the desktop.
///
/// `http`, `https` and `mailto` only. Everything else — `file:`, `javascript:`,
/// a custom handler — is refused, so opening a note cannot become a way to run
/// something on the machine that wrote it.
pub(crate) fn is_safe_link(href: &str) -> bool {
    let lower = href.trim().to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// Inline runs as Pango markup.
///
/// Text is XML-escaped, because a note containing `<` must not become markup.
/// Styling spans recurse into their nested runs, so `**a *b* c**` renders the
/// way a markdown reader expects.
pub(crate) fn pango_markup(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for run in inlines {
        write_inline(&mut out, run);
    }
    out
}

pub(crate) fn write_inline(out: &mut String, run: &Inline) {
    match run {
        Inline::Text(t) => out.push_str(&escape(t)),
        // A formula, shown in its readable Unicode form: `x² + √(y)`, not
        // the LaTeX source. A display formula gets its own line.
        Inline::Math { source, display } => {
            let text = sn_format::math::to_unicode(source);
            if *display {
                out.push_str("<span size=\"large\" style=\"italic\">");
                out.push_str(&escape(&text));
                out.push_str("</span>");
            } else {
                out.push_str("<span style=\"italic\">");
                out.push_str(&escape(&text));
                out.push_str("</span>");
            }
        }
        Inline::Code(t) => {
            out.push_str("<span font_family=\"monospace\">");
            out.push_str(&escape(t));
            out.push_str("</span>");
        }
        Inline::Bold(inner) => wrap_inline(out, "<b>", inner, "</b>"),
        Inline::Italic(inner) => wrap_inline(out, "<i>", inner, "</i>"),
        Inline::Strike(inner) => wrap_inline(out, "<s>", inner, "</s>"),
        // Pango has no <sup> tag on the versions this box ships; a smaller
        // font is the honest fallback and reads as a marker either way.
        Inline::Superscript(inner) => {
            wrap_inline(out, "<span size=\"70%\">", inner, "</span>")
        }
        Inline::Link { label, target } => {
            out.push_str(&format!("<a href=\"{}\">", escape(target)));
            for inner in label {
                write_inline(out, inner);
            }
            out.push_str("</a>");
        }
        // An inline image is lifted out by `split_images`; a leftover here
        // is shown as its alt text rather than dropped.
        Inline::Image { alt, .. } => out.push_str(&escape(alt)),
        Inline::SoftBreak => out.push(' '),
        Inline::HardBreak => out.push('\n'),
    }
}

pub(crate) fn wrap_inline(out: &mut String, open: &str, inner: &[Inline], close: &str) {
    out.push_str(open);
    for run in inner {
        write_inline(out, run);
    }
    out.push_str(close);
}

/// Escape text for Pango markup.
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Split a run list into the text runs and the images, so images can be laid
/// out as their own widgets instead of being dropped from the paragraph.
pub(crate) fn split_images(inlines: &[Inline]) -> (Vec<Inline>, Vec<Inline>) {
    let mut text = Vec::new();
    let mut images = Vec::new();
    for run in inlines {
        match run {
            Inline::Image { .. } => images.push(run.clone()),
            other => text.push(other.clone()),
        }
    }
    (text, images)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_marker_rides_inside_the_row_label() {
        // One label per list row is what makes a big note open in a second
        // (see `markup_label_with_marker`), so the marker and the text share
        // the markup — the marker dimmed, the text styled as usual.
        let markup = marked_markup("•", false, &[Inline::Text("buy milk".into())]);
        assert!(markup.contains("<span alpha=\"55%\">•</span> "), "{markup}");
        assert!(markup.ends_with("buy milk"), "{markup}");

        // An ordered item's number is a marker like any other.
        let numbered = marked_markup("12.", false, &[Inline::Text("twelfth".into())]);
        assert!(numbered.contains(">12.</span> "), "{numbered}");
    }

    #[test]
    fn a_marker_cannot_inject_markup() {
        // The marker comes from the note (an ordered item's number is
        // whatever the parser produced), so it is escaped like any other
        // note text.
        let markup = marked_markup("<b>1.</b>", false, &[Inline::Text("x".into())]);
        assert!(markup.contains("&lt;b&gt;1.&lt;/b&gt;"), "{markup}");
        assert!(!markup.contains("<b>1."), "{markup}");
    }

    #[test]
    fn a_done_task_is_dimmed_and_struck_through() {
        let done = marked_markup("☑", true, &[Inline::Text("file taxes".into())]);
        assert!(done.contains("<s>file taxes</s>"), "{done}");
        assert!(done.matches("<span alpha=\"55%\">").count() == 2, "{done}");

        let open = marked_markup("☐", false, &[Inline::Text("file taxes".into())]);
        assert!(!open.contains("<s>"), "{open}");
        assert!(open.matches("<span alpha=\"55%\">").count() == 1, "{open}");
    }
}

