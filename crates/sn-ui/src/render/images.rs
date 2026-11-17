//! Images: the picture when it can be read, a quiet placeholder while the
//! decode is in flight, a labelled one when it cannot. Split out of
//! `render.rs` (Round 4, pure code-motion).

use std::path::{Path, PathBuf};

use gtk4 as gtk;
use gtk4::prelude::*;

use sn_format::markdown::Inline;

use super::RenderCtx;

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

/// A note image, as the phone apps show one: the picture itself when it can
/// be read, a quiet placeholder while the decode is in flight, and a labelled
/// placeholder when it cannot.
///
/// The picture comes from [`crate::imgcache`] — decoded off the UI thread,
/// downscaled, and cached, so a photo-heavy note neither freezes on open nor
/// pays again on the next one. The widget is the cache's own box: full
/// width, shimmering placeholder when loading, the picture fading in when
/// the bytes land.
///
/// A remote `http(s)` link is *not* fetched here: this runs on the UI thread
/// on every debounced save. Cached pictures resolve like local files; an
/// uncached one shows the spinner and is queued on the render context for
/// the caller to fetch off the main loop and re-render; a fetch that already
/// came back negative shows the failure placeholder instead of a spinner
/// that never resolves.
pub(crate) fn image_widget(image: &Inline, ctx: &RenderCtx) -> gtk::Widget {
    let Inline::Image { alt, src } = image else {
        return gtk::Label::new(None).upcast();
    };
    match resolve_image(src, ctx) {
        Resolved::Ready(path) => ready_image(&path, alt, src),
        Resolved::Loading => loading_image(alt, src),
        Resolved::Failed => failed_image(alt, src),
        Resolved::Missing => missing_image(alt, src),
    }
}

/// The picture, full width, clickable into the zoom viewer.
pub(crate) fn ready_image(path: &Path, alt: &str, _src: &str) -> gtk::Widget {
    let picture = crate::imgcache::picture(path, crate::imgcache::NOTE_MAX_EDGE);
    if !alt.is_empty() {
        // iOS reads the alt as the accessibility label and never as a
        // caption; the tooltip is this build's way of surfacing it on hover.
        picture.set_tooltip_text(Some(alt));
        picture.update_property(&[gtk::accessible::Property::Label(alt)]);
    }
    // A click opens the full-screen zoom viewer, the way both phone builds
    // do. The parent is found from the widget's own root at click time —
    // the image may sit in the preview, a table cell or an alert box. The
    // viewer reads the *original file*: the preview's downscaled decode is
    // for the page, not for zooming.
    let click = gtk::GestureClick::new();
    let path = path.to_path_buf();
    let alt = alt.to_string();
    click.connect_pressed(move |gesture, _, _, _| {
        let parent = gesture
            .widget()
            .and_then(|w| w.root())
            .and_downcast::<gtk::Window>();
        crate::imageview::open(parent.as_ref(), &path, &alt);
    });
    picture.add_controller(click);
    picture.upcast()
}

/// The spinner box shown while a remote image is downloading.
///
/// Android reserves a full-width 96dp box with a centred spinner; iOS shows
/// a centred `ProgressView`. The box is full width so the note's shape does
/// not jump when the picture lands and takes the width anyway.
pub(crate) fn loading_image(alt: &str, src: &str) -> gtk::Widget {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.add_css_class("md-image-loading");
    let inner = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    inner.set_halign(gtk::Align::Center);
    inner.set_valign(gtk::Align::Center);
    inner.set_vexpand(true);
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    inner.append(&spinner);
    inner.append(&gtk::Label::new(Some("Loading image…")));
    outer.append(&inner);
    outer.set_tooltip_text(Some(if alt.is_empty() { src } else { alt }));
    outer.upcast()
}

/// The placeholder for an image this app cannot show: a refused reference or
/// a download that failed.
///
/// Android falls back to the alt text as body text; iOS shows a grey glyph.
/// The alt text is the note's own words and carries more than a glyph, so it
/// leads, with the failure called out beside it.
pub(crate) fn failed_image(alt: &str, src: &str) -> gtk::Widget {
    let text = if alt.is_empty() {
        "[image] — could not load".to_string()
    } else {
        format!("{alt} — image could not load")
    };
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .build();
    label.add_css_class("md-image-missing");
    label.set_tooltip_text(Some(src));
    label.upcast()
}

/// The placeholder for a local `speechnotes://note-image/<name>` whose file
/// is gone — deleted out from under the note, or a note imported without its
/// pictures. The file name goes in the tooltip so the user can tell which
/// attachment wandered off.
pub(crate) fn missing_image(alt: &str, src: &str) -> gtk::Widget {
    let text = if alt.is_empty() {
        format!("🖼 Missing image: {}", file_of(src))
    } else {
        format!("🖼 {alt}")
    };
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .build();
    label.add_css_class("md-image-missing");
    label.set_tooltip_text(Some(&file_of(src)));
    label.upcast()
}

/// What a `src` resolves to this pass, never a network call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    /// The picture is on disk at this path.
    Ready(PathBuf),
    /// A remote URL queued for the background fetch, or already under it —
    /// not on disk yet, but there is reason to believe it will be.
    Loading,
    /// Nothing this app can be responsible for showing: a refused reference
    /// (`file:`, absolute, traversal) or a fetch that already failed.
    Failed,
    /// A local `speechnotes://note-image/<name>` whose file is gone.
    Missing,
}

/// Resolve where a `src` points on disk.
///
/// `speechnotes://note-image/<name>` is this app's own scheme and resolves
/// inside the note's image directory. A bare relative name resolves the same
/// way. An `http(s)` URL resolves only if the note's copy has already been
/// fetched (see [`sn_net::image_cache_path`]); a URL not yet on disk is
/// *queued* on the render context for the caller to fetch off the UI thread,
/// and this pass shows the spinner. A `file:` URL, an absolute path and a
/// parent traversal are refused outright: none of them is a picture the app
/// can be responsible for.
pub(crate) fn resolve_image(src: &str, ctx: &RenderCtx) -> Resolved {
    let dir = ctx
        .images_dir
        .clone()
        .unwrap_or_else(sn_core::paths::note_images_dir);
    let Some(note_id) = ctx.note_id.as_deref() else {
        return Resolved::Failed;
    };

    if let Some(rest) = src.strip_prefix("https://").or_else(|| src.strip_prefix("http://")) {
        let url = if src.starts_with("http://") {
            format!("http://{rest}")
        } else {
            format!("https://{rest}")
        };
        let cache_dir = dir.join(note_id).join("remote");
        let cached = sn_net::image_cache_path(&url, &cache_dir);
        if cached.is_file() {
            return Resolved::Ready(cached);
        }
        if ctx.failed_remote.borrow().contains(&url) {
            return Resolved::Failed;
        }
        let mut attempted = ctx.attempted_remote.borrow_mut();
        if attempted.insert(url.clone()) {
            ctx.pending_remote.borrow_mut().push(url);
        }
        return Resolved::Loading;
    }

    let name = src
        .strip_prefix("speechnotes://note-image/")
        .or_else(|| src.strip_prefix("note-image/"))
        .unwrap_or(src);
    if name.contains("://") || name.starts_with('/') || name.contains("..") {
        return Resolved::Failed;
    }
    let path = dir.join(note_id).join(name);
    if path.is_file() {
        Resolved::Ready(path)
    } else {
        Resolved::Missing
    }
}

pub(crate) fn file_of(src: &str) -> String {
    src.rsplit('/').next().unwrap_or(src).to_string()
}

