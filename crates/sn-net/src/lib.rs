//! Networking for the download work: one HTTP client, used for the model
//! manager (Batch 6) and the interchange batches later.
//!
//! The rules this module exists to hold, all learned the hard way on the
//! iOS/Android ports:
//!
//! * **Redirects only to hosts the catalog pinned.** A model URL that 302s to
//!   an arbitrary host is how a supply-chain attack gets in. The final URL's
//!   host must be the requested one *or* a pinned CDN that the model hosts
//!   themselves are known to use — huggingface.co serves its large files from
//!   its own `*.hf.co` CDN (verified against every URL in the Batch 6
//!   catalog), and that CDN is the only off-host exception. Anything else is
//!   refused, naming the target.
//! * **A download is only atomic if it lands whole.** Callers stream into a
//!   `.part` file and rename into place; a partial file that survived would
//!   load as a corrupt model (the Kokoro story: a bank one row short is a
//!   voice from another speaker).
//! * **Progress is a callback, not a channel.** The UI wants a status line,
//!   not a thread.
//! * **Bytes are capped before they are read.** `Content-Length` is a hint
//!   the server chooses; a response beyond `max_bytes` is aborted mid-stream
//!   rather than filling the disk.

pub mod bookdrop;
//  sender.rs (original 20261025T004451, postdates this lib.rs original)
// is declared here — sn-ui's bookdrop module calls sn_net::sender::*.
pub mod sender;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A redirect off-host, an over-large body, or a failed request.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("http: {0}")]
    Http(String),
    #[error("{url} resolved to {location}, a host neither the request nor the pinned CDNs own")]
    CrossHost { url: String, location: String },
    #[error("{url} is too large: {size} bytes over the {limit}-byte limit")]
    TooLarge { url: String, size: u64, limit: u64 },
    #[error("{url} sent no content length; refusing to stream an unbounded body")]
    NoLength { url: String },
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// Where a finished download landed, and how big it was.
pub struct Download {
    /// The `.part` path. A caller that renames it into place owns the move;
    /// one that abandons it leaves a file the next attempt overwrites.
    pub part: PathBuf,
    /// How many bytes were written.
    pub bytes: u64,
}

/// Stream `url` to `dir/<file_name>.part`, reporting progress as it goes.
///
/// `max_bytes` caps the response: `Content-Length` is checked up front and the
/// body is aborted mid-read if it overruns — a server that under-reports its
/// length must not fill the disk before it is noticed.
///
/// `on_progress` runs on the calling thread with the running total; it may
/// block (it is usually a UI status line) and may abort the download by
/// returning `Err`.
pub fn download(
    url: &str,
    dir: &Path,
    max_bytes: u64,
    on_progress: &mut dyn FnMut(u64) -> Result<(), NetError>,
) -> Result<Download, NetError> {
    let response = ureq::get(url)
        // The download-failure regression (2026-10-29): ask for the identity
        // encoding explicitly. ureq's `gzip` feature otherwise advertises
        // `Accept-Encoding: gzip`, and the model hosts answer a small file
        // (under the LFS threshold — every pinned JSON in the catalog) with a
        // gzip'd, chunked body that carries **no Content-Length**. The check
        // below then refuses it before a byte lands, and no small file can
        // ever install: kokoro's `tokenizer.json` and
        // supertonic's `tts.json`/`unicode_indexer.json`/voice styles all
        // failed with `NetError::NoLength` while every large LFS file (served
        // from the CDN with a length) succeeded — reproduced 2026-10-29 by
        // calling this function on the catalog's own URLs. `identity` keeps
        // the response the length-delimited raw body every check here is
        // written against (cap up front, floor and sha after); ureq skips its
        // own Accept-Encoding header when the caller sets one, and a server
        // that still streams without a length is refused by the existing
        // NoLength arm rather than trusted.
        .set("Accept-Encoding", "identity")
        .timeout(Duration::from_secs(600))
        .call()
        .map_err(|e| NetError::Http(format!("{url}: {e}")))?;

    let status = response.status();
    if status != 200 {
        return Err(NetError::Http(format!("{url}: HTTP {status}")));
    }
    // The redirect check: ureq followed any 3xx transparently, so the final
    // URL is the one that actually served the bytes. Same host, or a host
    // the pinned-CDN rule allows (see `host_allowed`).
    let final_url = response.get_url().to_string();
    if !host_allowed(url, &final_url) {
        return Err(NetError::CrossHost {
            url: url.to_string(),
            location: final_url,
        });
    }

    let length = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| NetError::NoLength {
            url: url.to_string(),
        })?;
    if length > max_bytes {
        return Err(NetError::TooLarge {
            url: url.to_string(),
            size: length,
            limit: max_bytes,
        });
    }

    std::fs::create_dir_all(dir)?;
    let part = dir.join(format!("{}.part", file_name_from(url)));
    let file = std::fs::File::create(&part)?;
    let mut sink = std::io::BufWriter::new(file);
    let mut written: u64 = 0;
    let mut reader = response.into_reader();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        written += n as u64;
        // Defense-in-depth: ureq's `LimitedRead` stops at the declared
        // `Content-Length`, and the up-front check already refused a length
        // over the cap, so this branch is unreachable through ureq today —
        // it guards the cap for any future reader that can overrun its own
        // length (a decompressing wrapper, a transport bug), where "abort
        // and delete the .part" is the only safe answer.
        if written > max_bytes {
            drop(sink);
            let _ = std::fs::remove_file(&part);
            return Err(NetError::TooLarge {
                url: url.to_string(),
                size: written,
                limit: max_bytes,
            });
        }
        sink.write_all(&buf[..n])?;
        on_progress(written)?;
    }
    sink.flush()?;
    let file = sink.into_inner().map_err(|e| NetError::Io(e.into_error()))?;
    file.sync_all()?;

    tracing::debug!(url = %url, bytes = written, "download complete");
    Ok(Download { part, bytes: written })
}

/// Total length of a URL's body without downloading it. Unused for now —
/// the catalog ships the published sizes it quotes — and kept because the
/// interchange batches (Batch 10) need to size remote imports before they
/// commit to one. The cross-host rule does not apply to a HEAD: no body is
/// read, and the point of the call is only the number.
pub fn head_len(url: &str) -> Result<Option<u64>, NetError> {
    let response = ureq::request("HEAD", url)
        .timeout(Duration::from_secs(60))
        .call()
        .map_err(|e| NetError::Http(format!("{url}: {e}")))?;
    if response.status() != 200 {
        return Err(NetError::Http(format!(
            "{url}: HTTP {}",
            response.status()
        )));
    }
    Ok(response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok()))
}

/// A cap on a note's remote image: a photo a phone pasted into a note is a
/// few megabytes at most, and the cap is what stops a note becoming a
/// disk-filling fetcher.
pub const MAX_IMAGE_BYTES: u64 = 12 * 1024 * 1024;

/// Where a note's copy of `url` lives (or will live once fetched).
///
/// The name is a hash of the URL, not the URL's last segment: whoever wrote
/// the note's markdown does not get to choose a path in this app's cache, and
/// the check "is this image already here?" has to be answerable *before* the
/// fetch — the preview rebuilds on every save, and a fetch on the UI thread
/// would stall it.
pub fn image_cache_path(url: &str, dir: &Path) -> PathBuf {
    dir.join(format!("img-{:016x}.img", sn_core::util::stable_hash(url)))
}

/// Fetch an image into `dir` and return the file it landed in.
///
/// This is the note-image counterpart to `download`: a note that carries a
/// remote `![](https://…)` link should show the picture, and the editor has
/// to fetch it rather than refuse it — but on this app's rules, not the URL's.
/// `max_bytes` caps the body (an image a server claims is 900 MB is not one),
/// the host pinning in `host_allowed` still applies — which means a same-host
/// redirect and nothing else — and the bytes are sniffed before they are
/// trusted as an image: a server that answers an image request with HTML
/// gets nothing on disk.
///
/// The file lands **exactly at [`image_cache_path`]`(url, dir)`** — the name
/// is keyed on the URL, not on the bytes. That is the one contract the
/// renderer relies on: `resolve_image` answers "is this image already here?"
/// from that path *before* any fetch, so a fetch that named its file
/// differently (a content hash, say) would download fine and then never be
/// found — 40 fetched images sitting on disk while every note still spun
/// forever is precisely that bug ( 2026-11-07). The regression test
/// below pins fetch and lookup to the same path.
pub fn fetch_image(url: &str, dir: &Path, max_bytes: u64) -> Result<PathBuf, NetError> {
    let response = ureq::get(url)
        // Security review (2026-10-30): same exposure `download`
        // had before its round-2 fix — with ureq's `gzip` feature advertising
        // `Accept-Encoding: gzip`, a server that answers a small image with a
        // gzip'd, chunked body carries no Content-Length and is refused by
        // the NoLength arm below before a byte lands. Ask for identity so the
        // length-delimited raw body the checks here are written against is
        // what arrives; a server that still streams without a length is
        // refused, not trusted.
        .set("Accept-Encoding", "identity")
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| NetError::Http(format!("{url}: {e}")))?;
    if response.status() != 200 {
        return Err(NetError::Http(format!("{url}: HTTP {}", response.status())));
    }
    let final_url = response.get_url().to_string();
    if !host_allowed(url, &final_url) {
        return Err(NetError::CrossHost {
            url: url.to_string(),
            location: final_url,
        });
    }
    let length = response
        .header("Content-Length")
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| NetError::NoLength {
            url: url.to_string(),
        })?;
    if length > max_bytes {
        return Err(NetError::TooLarge {
            url: url.to_string(),
            size: length,
            limit: max_bytes,
        });
    }

    let mut bytes = Vec::with_capacity(length.min(1024 * 1024) as usize);
    let mut reader = response.into_reader();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if bytes.len() as u64 + n as u64 > max_bytes {
            return Err(NetError::TooLarge {
                url: url.to_string(),
                size: bytes.len() as u64 + n as u64,
                limit: max_bytes,
            });
        }
        bytes.extend_from_slice(&buf[..n]);
    }

    // Sniff before trusting: a picture is bytes a pixbuf loader accepts, and a
    // server answering an image link with HTML must not land on disk as one.
    // `sn_core::util::is_supported_image` is the app's existing check for the
    // same question — GTK accepts exactly what it accepts.
    if !sn_core::util::is_supported_image(&bytes) {
        return Err(NetError::Http(format!(
            "{url}: the body is not an image this app draws"
        )));
    }

    std::fs::create_dir_all(dir)?;
    // The URL-hash name `image_cache_path` computes: the "is it already
    // here?" check the renderer answers before any fetch has to match where
    // the fetch lands, byte for byte. (The content hash was tried here once
    // — the fetches all succeeded and the renderer never found a single one.)
    let path = image_cache_path(url, dir);
    if !path.exists() {
        sn_core::atomic::write_atomic(&path, &bytes)?;
    }
    Ok(path)
}

/// True when `final_url`'s host may serve `url`'s body: the same host, or a
/// pinned CDN for the model hosts this app downloads from.
///
/// The pinned list is not "anything that looks like a CDN". It is the set of
/// hosts the catalog's own URLs have been *observed* redirecting to — at the
/// time of writing, every huggingface.co LFS/xet file lands on
/// `<region>.aws.cdn.hf.co`, and the suffix rule covers the regional
/// variants. A hostile `Location:` to any other host is refused exactly as
/// before.
fn host_allowed(url: &str, final_url: &str) -> bool {
    // A URL with no host has no authority to vouch for anything — including
    // itself. `host_of` returns None for schemeless strings and `None ==
    // None` would otherwise read as "same host".
    if host_of(url).is_some() && host_of(url) == host_of(final_url) {
        return true;
    }
    host_of(final_url)
        .map(|h| h.ends_with(".hf.co") || h.ends_with(".huggingface.co"))
        .unwrap_or(false)
        && host_of(url).as_deref().is_some_and(|requested| {
            requested == "huggingface.co" || requested.ends_with(".huggingface.co")
        })
}

fn host_of(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    // Strip userinfo: `https://example.com@evil.net/` is evil.net.
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    Some(host.to_ascii_lowercase())
}

/// The last path segment, or `download` when the URL ends in `/`.
/// Characters that could escape the directory are filtered out: a hostile
/// server must not choose where on disk its payload lands.
fn file_name_from(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    // Everything after the authority is the path; a URL with no path at all
    // has no file name to take.
    let rest = match path.split_once("://") {
        Some((_, after_scheme)) => after_scheme.split_once('/').map(|(_, p)| p),
        None => Some(path.trim_start_matches('/')),
    };
    let tail = rest
        .map(|p| p.rsplit('/').next().unwrap_or(""))
        .filter(|t| !t.is_empty())
        .unwrap_or("");
    let safe: String = tail
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | '\0' | ':'))
        .collect();
    if safe.is_empty() {
        "download".to_string()
    } else {
        safe
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowed_matches_a_plain_pair() {
        assert!(host_allowed(
            "https://example.com/a.onnx",
            "https://example.com/a.onnx"
        ));
    }

    #[test]
    fn host_allowed_is_case_insensitive_and_ignores_path() {
        assert!(host_allowed(
            "https://Example.COM/a/b.onnx",
            "https://example.com/c.onnx"
        ));
    }

    #[test]
    fn a_different_host_is_refused() {
        assert!(!host_allowed(
            "https://example.com/a.onnx",
            "https://cdn.other.net/a.onnx"
        ));
    }

    #[test]
    fn the_hf_cdn_is_the_one_off_host_exception() {
        // The exact chain every catalog URL walks today.
        assert!(host_allowed(
            "https://huggingface.co/x/resolve/main/model.onnx",
            "https://us.aws.cdn.hf.co/xet-bridge-us/abc?sig=1"
        ));
        // Regional variants are covered by the suffix.
        assert!(host_allowed(
            "https://huggingface.co/x/resolve/main/model.onnx",
            "https://eu.aws.cdn.hf.co/xet-bridge-eu/abc"
        ));
    }

    #[test]
    fn the_hf_cdn_exception_is_not_for_other_requests() {
        // A request to some other host cannot launder itself through the
        // CDN: the exception requires the *requested* host be HF's.
        assert!(!host_allowed(
            "https://evil.example/model.onnx",
            "https://us.aws.cdn.hf.co/xet-bridge-us/abc"
        ));
        // A lookalike suffix is not the CDN.
        assert!(!host_allowed(
            "https://huggingface.co/x/resolve/main/model.onnx",
            "https://us.aws.cdn.not-hf.co/abc"
        ));
        assert!(!host_allowed(
            "https://huggingface.co/x/resolve/main/model.onnx",
            "https://us.aws.cdn.hfco/abc"
        ));
    }

    #[test]
    fn userinfo_does_not_disguise_the_host() {
        assert!(!host_allowed(
            "https://example.com/a.onnx",
            "https://example.com@evil.net/a.onnx"
        ));
    }

    #[test]
    fn a_url_with_no_scheme_has_no_host() {
        assert!(host_of("not a url").is_none());
        assert!(host_of("example.com/a").is_none());
        // Two URLs with no host at all: no scheme means the caller's own URL
        // construction is the thing that is broken, and the pinned-CDN rule
        // does not open a hole for it.
        assert!(!host_allowed("example.com/a", "example.com/a"));
    }

    #[test]
    fn a_bare_filename_survives_the_sanitiser() {
        assert_eq!(file_name_from("https://x/model.onnx"), "model.onnx");
        assert_eq!(
            file_name_from("https://x/a/b/model.onnx?v=1"),
            "model.onnx"
        );
        assert_eq!(file_name_from("https://x/"), "download");
        // A URL that is nothing but a scheme and a host has no path, so the
        // last segment is the whole host — fall back to a neutral name.
        assert_eq!(file_name_from("https://x"), "download");
    }

    #[test]
    fn a_filename_with_path_characters_is_neutralised() {
        // A hostile server must not be able to choose the destination.
        assert_eq!(
            file_name_from("https://x/..%2F..%2Fetc%2Fpasswd"),
            "..%2F..%2Fetc%2Fpasswd"
        );
        assert_eq!(file_name_from("https://x/../../etc/passwd"), "passwd");
    }

    // The web-images-in-notes regression (2026-11-07): a fetched
    // image must land exactly at `image_cache_path(url, dir)`. The renderer
    // answers "is it already here?" from that path before any fetch; the
    // fetcher once named its files by a content hash instead, so every
    // download succeeded and the renderer never found a single one — the
    // cryptorchidism note's 40 images were on disk while the note spun
    // forever. This pins fetch and lookup to the same path.
    #[test]
    fn a_fetched_image_lands_where_the_cache_lookup_will_find_it() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 8 * 1024];
            let mut request = String::new();
            loop {
                let n = sock.read(&mut buf).expect("read request");
                request.push_str(&String::from_utf8_lossy(&buf[..n]));
                if request.contains("\r\n\r\n") {
                    break;
                }
            }
            // A real PNG signature: the sniff must accept the body.
            let body: Vec<u8> = b"\x89PNG\r\n\x1a\nrest-of-picture".to_vec();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(response.as_bytes());
            let _ = sock.write_all(&body);
        });

        let url = format!("http://127.0.0.1:{port}/pics/one.png");
        let dir = std::env::temp_dir().join("sn-net-image-cache-key");
        let landed = fetch_image(&url, &dir, 1_000_000).expect("fetch from the local server");
        assert_eq!(
            landed,
            image_cache_path(&url, &dir),
            "the fetch must land exactly where resolve_image looks"
        );
        assert!(landed.is_file());
        server.join().expect("server thread");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The download-failure regression (2026-10-29): `download`
    // must send `Accept-Encoding: identity`. With ureq's `gzip` feature
    // advertising gzip, the model hosts answer every small catalog file
    // (the pinned JSONs) with a gzip'd chunked body that carries no
    // Content-Length, which this function's length checks must refuse —
    // so no small file could ever install. The header is the fix; this
    // pins it against a local server that echoes back what was asked.
    #[test]
    fn a_download_asks_for_the_identity_encoding() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 8 * 1024];
            let mut request = String::new();
            loop {
                let n = sock.read(&mut buf).expect("read request");
                request.push_str(&String::from_utf8_lossy(&buf[..n]));
                if request.contains("\r\n\r\n") {
                    break;
                }
            }
            let asked_identity = request
                .to_ascii_lowercase()
                .contains("accept-encoding: identity");
            let body = "0123456789";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes());
            asked_identity
        });

        let dir = std::env::temp_dir().join("sn-net-identity-probe");
        let mut progress = |_written: u64| Ok::<(), NetError>(());
        let download = download(
            &format!("http://127.0.0.1:{port}/small.json"),
            &dir,
            1_000_000,
            &mut progress,
        )
        .expect("download from the local server");
        assert_eq!(download.bytes, 10);
        // The response only lands whole if the length checks passed; the
        // assertion that matters is what the server was asked for.
        assert!(
            server.join().expect("server thread"),
            "the request must carry Accept-Encoding: identity"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
