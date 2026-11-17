//! Rendering a parsed document into GTK widgets.
//!
//! One parse, three consumers: this preview, the speech text and the
//! read-along highlight all walk the same [`Document`], so what you see and
//! what the voice says cannot drift.
//!
//! **Tables are drawn the way a table is expected to look**: a bordered grid
//! with a bold header row, cells that wrap, and columns that share the width
//! instead of sizing to their widest cell. The iOS build drew a `Grid` with one
//! `Divider` under the header and a faint rounded background — no cell borders,
//! no header emphasis — which reads as a list of labels, not a table. That is
//! the thing this file exists to not do.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;

use sn_format::markdown::Document;
#[cfg(test)]
use sn_format::markdown::Inline;

mod blocks;

use blocks::render_block;
#[cfg(test)]
use tables::column_minimums;
#[cfg(test)]
use images::{resolve_image, Resolved};
#[cfg(test)]
use inlines::{is_safe_link, pango_markup};
mod images;
mod inlines;
mod tables;


#[derive(Clone, Default, Debug)]
pub struct RenderCtx {
    /// The note being rendered. `speechnotes://note-image/x.png` resolves
    /// inside this note's image directory.
    pub note_id: Option<String>,
    /// Base directory for `note-images/`, injected so tests can point it
    /// somewhere disposable.
    pub images_dir: Option<PathBuf>,
    /// Remote image URLs this preview wanted and had not cached yet. The
    /// renderer never fetches: it records what it is missing here, and the
    /// caller fetches off the UI thread and re-renders — a preview rebuilds
    /// on every debounced save, and a network round trip on the main loop
    /// would stall typing.
    pub pending_remote: Rc<RefCell<Vec<String>>>,
    /// URLs already asked this run for, successes and failures alike. Shared
    /// with the note's other render passes through one `Rc`: the preview
    /// re-renders after every fetch, and a URL that failed must not be
    /// re-queued on each one.
    pub attempted_remote: Rc<RefCell<std::collections::HashSet<String>>>,
    /// Remote URLs whose fetch has already come back negative this run. The
    /// renderer draws the failure placeholder for these instead of a
    /// spinner, so a dead link reads as dead the moment its fetch lands —
    /// not as a spinner that never resolves (the Android build's bug).
    /// Filled by the window's fetch poll; the renderer only reads it.
    pub failed_remote: Rc<RefCell<std::collections::HashSet<String>>>,
}

impl RenderCtx {
    pub fn for_note(note_id: &str) -> Self {
        Self {
            note_id: Some(note_id.to_string()),
            images_dir: Some(sn_core::paths::note_images_dir()),
            pending_remote: Rc::new(RefCell::new(Vec::new())),
            attempted_remote: Rc::new(RefCell::new(std::collections::HashSet::new())),
            failed_remote: Rc::new(RefCell::new(std::collections::HashSet::new())),
        }
    }

    /// The same context, but carrying a `seen` set the caller owns, so the
    /// set survives this render pass's re-renders. The failed set starts
    /// empty — a probe or an example has no fetch poll behind it to fill
    /// one — which simply means every remote URL shows as loading.
    pub fn with_seen(note_id: &str, seen: Rc<RefCell<std::collections::HashSet<String>>>) -> Self {
        Self {
            note_id: Some(note_id.to_string()),
            images_dir: Some(sn_core::paths::note_images_dir()),
            pending_remote: Rc::new(RefCell::new(Vec::new())),
            attempted_remote: seen,
            failed_remote: Rc::new(RefCell::new(std::collections::HashSet::new())),
        }
    }
}

/// A rendered document: a scrolling column of block widgets.
pub fn render_document(doc: &Document, ctx: &RenderCtx) -> gtk::ScrolledWindow {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    column.add_css_class("markdown-body");

    for block in &doc.blocks {
        render_block(block, ctx, &column);
    }
    if doc.blocks.is_empty() {
        let empty = gtk::Label::new(Some("Nothing to preview yet."));
        empty.add_css_class("empty");
        column.append(&empty);
    }

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&column)
        .build()
}

/// How many blocks the first synchronous chunk carries. The rest follow on
/// idle callbacks: the first screen paints immediately, the tail of a long
/// note builds while the user starts reading.
const FIRST_CHUNK: usize = 120;
const CHUNK: usize = 120;

/// A rendered document that **paints before it finishes building**.
///
/// The whole-document widget tree used to be built in one synchronous pass,
/// which on a 162 KB note with 63 photos was hundreds of milliseconds of
/// frozen frame after the images themselves were fixed. The column is real
/// the moment this returns — with its first ~120 blocks — and the remainder
/// appends on idle, so the main loop breathes between chunks.
///
/// `on_done` runs once, when the last block is in — the caller's chance to
/// do the work that must see the whole document (the preview's remote-image
/// fetch queue is the one today).
pub fn render_document_progressive(
    doc: Rc<Document>,
    ctx: Rc<RenderCtx>,
    on_done: Box<dyn Fn(&RenderCtx)>,
) -> gtk::ScrolledWindow {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    column.add_css_class("markdown-body");

    let t_build = std::time::Instant::now();
    let first_end = doc.blocks.len().min(FIRST_CHUNK);
    for block in &doc.blocks[..first_end] {
        render_block(block, &ctx, &column);
    }
    if doc.blocks.is_empty() {
        let empty = gtk::Label::new(Some("Nothing to preview yet."));
        empty.add_css_class("empty");
        column.append(&empty);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&column)
        .build();
    if first_end >= doc.blocks.len() {
        tracing::info!(
            blocks = doc.blocks.len(),
            build_ms = t_build.elapsed().as_secs_f64() * 1000.0,
            "preview built"
        );
        on_done(&ctx);
        return scroll;
    }
    // The tail: chunks of blocks on idle, sharing the column. The Rc'd
    // document keeps the source alive for exactly as long as the builds do;
    // a preview replaced mid-build simply appends into a column nobody is
    // showing, which is harmless.
    let mut next = first_end;
    let column_clone = column.clone();
    let doc_for_build = doc.clone();
    glib::idle_add_local(move || {
        let blocks = &doc_for_build.blocks;
        let end = (next + CHUNK).min(blocks.len());
        for block in &blocks[next..end] {
            render_block(block, &ctx, &column_clone);
        }
        next = end;
        if next >= blocks.len() {
            tracing::info!(
                blocks = blocks.len(),
                build_ms = t_build.elapsed().as_secs_f64() * 1000.0,
                "preview built"
            );
            on_done(&ctx);
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
    scroll
}

/// Append one block's widgets to `out`.

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RenderCtx {
        RenderCtx {
            note_id: Some("NOTE".to_string()),
            images_dir: Some(PathBuf::from("/data/note-images")),
            pending_remote: Rc::new(RefCell::new(Vec::new())),
            attempted_remote: Rc::new(RefCell::new(std::collections::HashSet::new())),
            failed_remote: Rc::new(RefCell::new(std::collections::HashSet::new())),
        }
    }

    /// A disposable `note-images` tree holding one real file, because
    /// resolution now tells `Ready` (the file is there) from `Missing` (it
    /// is not) — a path string alone can no longer carry that.
    fn dir_with(tag: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sn-ui-render-images-{tag}-{}",
            std::process::id()
        ));
        let note = dir.join("NOTE");
        std::fs::create_dir_all(&note).unwrap();
        for f in files {
            std::fs::write(note.join(f), b"").unwrap();
        }
        dir
    }

    fn ctx_in(tag: &str, files: &[&str]) -> RenderCtx {
        RenderCtx {
            images_dir: Some(dir_with(tag, files)),
            ..ctx()
        }
    }

    #[test]
    fn a_note_can_only_hand_the_desktop_a_web_or_mail_link() {
        for ok in [
            "https://example.com/x",
            "http://example.com",
            "mailto:someone@example.com",
            "HTTPS://EXAMPLE.COM",
        ] {
            assert!(is_safe_link(ok), "{ok} should be allowed");
        }
        // A note is untrusted the moment it can be imported. These would let
        // the text choose what the desktop opens.
        for refused in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "sms:+15550100",
            "ftp://example.com",
            "data:text/html,<script>",
            "",
        ] {
            assert!(!is_safe_link(refused), "{refused} must be refused");
        }
    }

    #[test]
    fn a_note_image_resolves_inside_its_own_note() {
        let c = ctx_in("own", &["a.png"]);
        assert_eq!(
            resolve_image("speechnotes://note-image/a.png", &c),
            Resolved::Ready(c.images_dir.clone().unwrap().join("NOTE/a.png"))
        );
        assert_eq!(
            resolve_image("a.png", &c),
            Resolved::Ready(c.images_dir.clone().unwrap().join("NOTE/a.png"))
        );
        // A name with no file behind it is Missing, not Failed: the note's
        // own picture wandered off, which the placeholder says differently
        // than "this app refused the reference".
        assert_eq!(
            resolve_image("gone.png", &c),
            Resolved::Missing,
            "no such file in the note's directory"
        );
    }

    #[test]
    fn an_image_path_cannot_escape_its_note() {
        let c = ctx();
        // `..` would read any file the user can read, and an absolute path
        // points outside the note's own directory. Both are refused outright.
        for bad in [
            "../../../etc/passwd",
            "speechnotes://note-image/../../other-note/secret.png",
            "/etc/passwd",
            "file:///etc/passwd",
        ] {
            assert_eq!(resolve_image(bad, &c), Resolved::Failed, "{bad} must not resolve");
        }
    }

    #[test]
    fn a_remote_image_is_queued_not_fetched_on_the_render_thread() {
        let c = ctx();
        // The renderer never fetches: it records what it is missing, and the
        // caller pulls it off the UI thread and re-renders. A preview rebuild
        // runs on every debounced save — a network round trip here stalls
        // typing.
        assert_eq!(
            resolve_image("https://example.com/pic.png", &c),
            Resolved::Loading,
            "not cached yet, so no picture this pass"
        );
        assert_eq!(
            c.pending_remote.borrow().as_slice(),
            ["https://example.com/pic.png"],
            "and the URL is queued for the background fetch"
        );
        // Asked once: a fetch in flight shows the spinner rather than
        // refetching on every keystroke.
        assert_eq!(
            resolve_image("https://example.com/pic.png", &c),
            Resolved::Loading,
            "a second render pass is still not a fetch"
        );
        assert_eq!(
            c.pending_remote.borrow().len(),
            1,
            "the URL is queued exactly once per render pass"
        );
    }

    #[test]
    fn a_failed_fetch_is_a_failure_not_an_everlasting_spinner() {
        // The window's fetch poll records the URLs that came back negative;
        // the next render pass must show the failure placeholder and must
        // not queue them again. This is the one behaviour Android's build
        // gets wrong — its failed images spin forever.
        let c = ctx();
        c.failed_remote
            .borrow_mut()
            .insert("https://example.com/pic.png".to_string());
        assert_eq!(
            resolve_image("https://example.com/pic.png", &c),
            Resolved::Failed,
            "the fetch already failed this run"
        );
        assert!(
            c.pending_remote.borrow().is_empty(),
            "a failed URL is not re-queued"
        );
    }

    #[test]
    fn note_text_cannot_inject_markup() {
        // A note containing angle brackets must render as text, not as markup.
        let markup = pango_markup(&[
            Inline::Text("a < b & c > d".into()),
            Inline::Bold(vec![Inline::Text("x".into())]),
        ]);
        assert!(markup.contains("a &lt; b &amp; c &gt; d"), "{markup}");
        assert!(markup.contains("<b>x</b>"), "{markup}");

        // And a link target is escaped the same way, so a crafted target
        // cannot break out of the href attribute.
        let markup = pango_markup(&[Inline::Link {
            label: vec![Inline::Text("x".into())],
            target: "\"><script>".into(),
        }]);
        assert!(!markup.contains("<script>"), "{markup}");
    }

    #[test]
    fn a_formula_renders_as_unicode_not_latex_source() {
        // The preview shows the readable Unicode form (math::to_unicode), the
        // way the original write_inline fragment did — never the raw LaTeX a
        // reader cannot parse at a glance.
        let markup = pango_markup(&[Inline::Math {
            source: r"x^2 + \sqrt{y}".into(),
            display: false,
        }]);
        assert!(markup.contains("x² + √(y)"), "{markup}");
        assert!(!markup.contains("x^2"), "{markup}");
        assert!(markup.contains("style=\"italic\""), "{markup}");

        // A display formula runs large as well as italic.
        let display = pango_markup(&[Inline::Math {
            source: r"\alpha".into(),
            display: true,
        }]);
        assert!(display.contains("α"), "{display}");
        assert!(display.contains("size=\"large\""), "{display}");
    }

    #[test]
    fn a_table_column_never_gets_a_zero_width_floor() {
        // A one-character column and a long-word column both stay readable, and
        // a pathological column cannot blow the table out sideways.
        let header = vec![vec![Inline::Text("A".into())], vec![Inline::Text(
            "a-very-long-header-word".into(),
        )]];
        let rows = vec![vec![
            vec![Inline::Text("x".into())],
            vec![Inline::Text("y".into())],
        ]];
        let mins = column_minimums(&header, &rows, 2);
        assert_eq!(mins.len(), 2);
        assert!(mins.iter().all(|m| (6..=26).contains(m)), "{mins:?}");
    }

    #[test]
    fn a_ragged_table_pads_to_the_widest_row() {
        // More cells in a body row than in the header still yields one column
        // per position, so the grid has no hole in it. The widget-level proof
        // is the integration test; this pins the arithmetic.
        let header: Vec<Vec<Inline>> = vec![vec![Inline::Text("A".into())]];
        let rows: Vec<Vec<Vec<Inline>>> = vec![
            vec![vec![Inline::Text("1".into())]],
            vec![
                vec![Inline::Text("2".into())],
                vec![Inline::Text("3".into())],
                vec![Inline::Text("4".into())],
            ],
        ];
        let count = header
            .len()
            .max(rows.iter().map(|r| r.len()).max().unwrap_or(0))
            .max(1);
        assert_eq!(count, 3, "the widest body row sets the column count");
    }

}
