//! Split out of slideview.rs 2026-10-31 (pure code-motion).

use super::DeckState;

// ---------------------------------------------------------------------------
// Serving
// ---------------------------------------------------------------------------

/// Answer one request from the deck.
///
/// `/deck.html` (the document itself, kept in [`DeckState`]) and
/// `/media-{index}/…` are served: the index into the deck's own media
/// list (never a name — pptx and odp name their media differently, the index
/// is the one address both agree on). The rest of the path is decoration.
pub(crate) fn serve_request(state: &DeckState, req: &webkit6::URISchemeRequest) {
    let Some(path) = req.path() else {
        return finish_missing(req, "no path");
    };
    if path == "/" || path == "/deck.html" || path.starts_with("/deck.html#") {
        let html = state.html.borrow();
        if html.is_empty() {
            return finish_missing(req, "no deck is open");
        }
        let stream = gtk4::gio::MemoryInputStream::from_bytes(&gtk4::glib::Bytes::from_owned(
            html.as_bytes().to_vec(),
        ));
        return req.finish(&stream, -1, Some("text/html"));
    }
    let Some(rest) = path.strip_prefix("/media-") else {
        return finish_missing(req, "not a media path");
    };
    let Ok(index) = rest.split('/').next().unwrap_or("").parse::<usize>() else {
        return finish_missing(req, "bad media index");
    };
    let deck = state.deck.borrow();
    let Some(deck) = deck.as_ref() else {
        return finish_missing(req, "no deck is open");
    };
    let Some(media) = deck.media.get(index) else {
        return finish_missing(req, "no such media");
    };
    let stream = gtk4::gio::MemoryInputStream::from_bytes(&gtk4::glib::Bytes::from_owned(
        media.data.to_vec(),
    ));
    req.finish(&stream, -1, Some(media.content_type.as_str()));
}

/// Refuse a request with a 404.
fn finish_missing(req: &webkit6::URISchemeRequest, why: &str) {
    let mut error = gtk4::glib::Error::new(
        gtk4::gio::IOErrorEnum::NotFound,
        &format!("not found: {why}"),
    );
    req.finish_error(&mut error);
}
