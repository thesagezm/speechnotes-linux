//! Note images: async, downscaled, cached.
//!
//! **Why.** `gdk::Texture::from_file` on the UI thread, at full resolution,
//! for every image, on every open — that was the preview's image path, and a
//! note with 43 photos spent 1.5–4 s frozen per open. This module replaces
//! it with the shape every image-heavy app uses:
//!
//! * an in-memory LRU of `gdk::Texture`, keyed by `(path, mtime, len)` —
//!   repeat opens of the same note are instant, and a changed file is a
//!   different key (no stale picture);
//! * a miss goes to a small pool of decode workers (capped, so a 43-photo
//!   note does not spawn 43 threads) that read, decode and **downsample to
//!   ≤ `max_edge` on the long edge** before the bytes ever cross back —
//!   an 8 MB photo becomes ~6 MB of RGBA instead of ~32;
//! * the bytes land on the UI thread through one channel, are wrapped in a
//!   `gdk::MemoryTexture` (no second decode) and swapped into the waiting
//!   widget with a fade;
//! * while loading, a quiet placeholder reserves the page's shape: the
//!   picture's size is remembered per key, so the second open of a note
//!   never jumps.
//!
//! The zoom viewer is deliberately not here — full resolution on demand is
//! its job, and it keeps reading the original file.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use gtk4::glib;
use gtk4::prelude::*;

/// Identity of a file's contents as far as a picture is concerned — **and of
/// the size it was asked at**. The same file serves a 480 px card cover and
/// a 96 px list thumbnail; keyed on the file alone, the second caller hit the
/// first one's texture and got 480 px where it asked for 96 (the caller's
/// size request is only a floor, and the uncapped append let the texture
/// win — the list rows grew to ~500 px). `DIMS` stays keyed by path alone:
/// it is about the file's intrinsic shape, not any one request.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    mtime: std::time::SystemTime,
    len: u64,
    max_edge: u32,
}

fn key_of(path: &Path, max_edge: u32) -> Option<Key> {
    let meta = std::fs::metadata(path).ok()?;
    Some(Key {
        path: path.to_path_buf(),
        mtime: meta.modified().ok()?,
        len: meta.len(),
        max_edge,
    })
}

/// The long edge a decoded picture is capped at. 1400 px is beyond any width
/// the preview column ever gets, and halves-to-quarters the RGBA footprint
/// of a modern phone photo.
pub const NOTE_MAX_EDGE: u32 = 1400;
/// Covers sit in small cards; a 480 px long edge is plenty and keeps a shelf
/// rebuild's worth of them out of the cache's budget.
pub const COVER_MAX_EDGE: u32 = 480;

/// What the LRU may hold, in estimated RGBA bytes. A 43-photo note at
/// 1400 px holds ~250 MB at worst; past that the oldest pictures go and are
/// re-decoded (from disk, off the UI thread) when scrolled back to.
const CACHE_BUDGET: usize = 256 * 1024 * 1024;

/// How many decode workers may run at once.
const IN_FLIGHT_CAP: usize = 4;

struct Cache {
    map: HashMap<Key, gtk4::gdk::Texture>,
    /// The footprint of each cached texture, in RGBA bytes. Kept beside the
    /// texture so the eviction policy can be a pure function over
    /// (key, bytes) pairs — testable without a display.
    sizes: HashMap<Key, usize>,
    order: VecDeque<Key>,
    bytes: usize,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache {
        map: HashMap::new(),
        sizes: HashMap::new(),
        order: VecDeque::new(),
        bytes: 0,
    });
    /// Widgets waiting on a decode, plus every size this process has learned,
    /// so a placeholder can reserve the right height before the bytes land.
    static WAITING: RefCell<HashMap<Key, Vec<gtk4::Box>>> = RefCell::new(HashMap::new());
    static DIMS: RefCell<HashMap<PathBuf, (u32, u32, u64)>> = RefCell::new(HashMap::new());
}

/// The keys to evict so the cache fits inside `budget`.
///
/// The policy is **least-recently-used first**: `order` is the recency
/// list, oldest at the front, so the front is what goes.
///
/// Two rules matter:
///
/// * **The last entry is never evicted.** The picture that is on screen
///   right now is the one thing that must not be dropped, even if it alone
///   is over the budget. A cache that evicts everything it has is a cache
///   that re-decodes on every scroll.
/// * **A missing size counts as zero.** A key whose footprint is not known
///   does not contribute to the total, so it cannot keep the cache over
///   the budget on its own.
///
/// Pure, so it can be tested without a GTK texture in sight.
fn eviction_order(
    order: &VecDeque<Key>,
    sizes: &HashMap<Key, usize>,
    bytes: usize,
    budget: usize,
) -> Vec<Key> {
    let mut total = bytes;
    let mut dropped = Vec::new();
    while total > budget && order.len() - dropped.len() > 1 {
        let Some(key) = order.get(dropped.len()) else {
            break;
        };
        total = total.saturating_sub(sizes.get(key).copied().unwrap_or(0));
        dropped.push(key.clone());
    }
    dropped
}

/// A decode's result, as plain bytes plus dimensions.
struct Decoded {
    width: u32,
    height: u32,
    /// RGBA8, **premultiplied** — the alpha format `gdk::MemoryTexture` is
    /// asked for below.
    rgba: Vec<u8>,
}

/// The job queue and the in-flight count share one lock: a worker that
/// finishes pops the next job itself, so the pool never exceeds the cap and
/// never needs a scheduler thread.
static QUEUE: std::sync::Mutex<(VecDeque<Job>, usize)> =
    std::sync::Mutex::new((VecDeque::new(), 0));

struct Job {
    key: Key,
    max_edge: u32,
}

type Channel = (
    std::sync::mpsc::Sender<(Key, u32, Result<Decoded, ()>)>,
    std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<(Key, u32, Result<Decoded, ()>)>>>,
);

static CHANNEL: std::sync::OnceLock<Channel> = std::sync::OnceLock::new();

fn channel() -> &'static Channel {
    CHANNEL.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            tx,
            std::sync::Arc::new(std::sync::Mutex::new(rx)),
        )
    })
}

/// Attach the decode receiver. Called from window construction, alongside
/// the tasks channel — before any preview renders.
pub fn attach() {
    let rx = std::sync::Arc::clone(&channel().1);
    glib::timeout_add_local(std::time::Duration::from_millis(30), move || {
        let mut landed = Vec::new();
        {
            let rx = rx.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            loop {
                match rx.try_recv() {
                    Ok(item) => landed.push(item),
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                }
            }
        }
        for (key, max_edge, result) in landed {
            deliver(&key, max_edge, result);
        }
        glib::ControlFlow::Continue
    });
}

/// A widget showing the picture at `path`, sooner or later.
///
/// Cache hit: the picture is in the returned box already, synchronous and
/// instant. Miss: a quiet placeholder holds the space (the exact space, when
/// this process has decoded the file before) and the decoded picture fades
/// in over it when the worker reports. Either way the box is full-width —
/// the click target is the whole area, the way the preview's images already
/// work.
pub fn picture(path: &Path, max_edge: u32) -> gtk4::Box {
    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    outer.add_css_class("md-image-box");
    let Some(key) = key_of(path, max_edge) else {
        placeholder_box(&outer, None);
        return outer;
    };
    let cached = CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if let Some(texture) = cache.map.get(&key).cloned() {
            if let Some(at) = cache.order.iter().position(|k| k == &key) {
                cache.order.remove(at);
                cache.order.push_back(key.clone());
            }
            return Some(texture);
        }
        None
    });
    if let Some(texture) = cached {
        // A caller that pre-capped this box gets a capped picture even on
        // the hit path — the same rule `deliver` applies when a decode
        // lands. Without it the texture's own size is the picture's size.
        match outer.size_request() {
            (0, 0) => append_picture(&outer, &texture),
            (w, h) => append_sized_picture(&outer, &texture, w, h),
        }
        return outer;
    }
    // Not cached: hold the space with what is known, and go to the pool.
    let dims = DIMS.with(|d| d.borrow().get(&key.path).copied());
    placeholder_box(&outer, dims.filter(|(w, h, _)| *w > 0 && *h > 0));
    WAITING.with(|w| w.borrow_mut().entry(key.clone()).or_default().push(outer.clone()));
    submit(Job { key, max_edge });
    outer
}

/// A picture held at a **fixed size** — a cover thumbnail, not a page image.
///
/// [`picture`] is full-width: the picture fills whatever width it is given
/// and its height follows the image's own aspect. That is right for a note's
/// preview and wrong for a shelf card, where the cover is a fixed object
/// that must not grow with the window: the card's own `height_request` is
/// only a *minimum*, and the picture's aspect-derived height overrode it,
/// so the cards became 660 px monsters.
///
/// This variant gives the picture a hard width and height and lets it fit
/// inside them, keeping its aspect — the cover is the size the shelf says
/// it is, whatever the source image's dimensions.
pub fn sized_picture(path: &Path, max_edge: u32, width: i32, height: i32) -> gtk4::Box {
    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    outer.add_css_class("md-image-box");
    outer.set_size_request(width, height);
    outer.set_halign(gtk4::Align::Center);
    outer.set_valign(gtk4::Align::Center);
    outer.set_overflow(gtk4::Overflow::Hidden);
    let Some(key) = key_of(path, max_edge) else {
        placeholder_box(&outer, None);
        return outer;
    };
    let cached = CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if let Some(texture) = cache.map.get(&key).cloned() {
            if let Some(at) = cache.order.iter().position(|k| k == &key) {
                cache.order.remove(at);
                cache.order.push_back(key.clone());
            }
            return Some(texture);
        }
        None
    });
    if let Some(texture) = cached {
        append_sized_picture(&outer, &texture, width, height);
        return outer;
    }
    placeholder_sized(&outer, width, height);
    WAITING.with(|w| w.borrow_mut().entry(key.clone()).or_default().push(outer.clone()));
    submit(Job { key, max_edge });
    outer
}

/// The placeholder for a fixed-size picture: the space the picture will
/// take, and nothing more.
fn placeholder_sized(outer: &gtk4::Box, width: i32, height: i32) {
    let holder = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    holder.add_css_class("md-image-loading");
    holder.set_size_request(width, height);
    let aspect = crate::aspect_image::AspectImage::new_placeholder(width as u32, height as u32);
    aspect.set_cap(width, height);
    holder.append(&aspect);
    outer.append(&holder);
}


/// The placeholder shown while a decode is in flight. `dims` reserves the
/// aspect-correct height, so the page below a loading image neither jumps
/// when the picture arrives nor snaps back when it scrolls away and returns.
fn placeholder_box(outer: &gtk4::Box, dims: Option<(u32, u32, u64)>) {
    let holder = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    holder.add_css_class("md-image-loading");
    holder.add_css_class("md-image-shimmer");
    if let Some((w, h, _)) = dims {
        let aspect = crate::aspect_image::AspectImage::new_placeholder(w, h);
        holder.append(&aspect);
    } else {
        holder.set_height_request(96);
    }
    outer.append(&holder);
}

/// The picture widget, fading in — a CSS transition animates the opacity
/// change on the next frame.
fn append_picture(outer: &gtk4::Box, texture: &gtk4::gdk::Texture) {
    let picture = crate::aspect_image::AspectImage::new(texture);
    picture.add_css_class("md-image");
    picture.set_opacity(0.0);
    outer.append(&picture);
    glib::idle_add_local_once(move || picture.set_opacity(1.0));
}

/// The picture widget at a fixed size — a cover thumbnail that keeps its
/// aspect inside a box the shelf chose.
fn append_sized_picture(outer: &gtk4::Box, texture: &gtk4::gdk::Texture, width: i32, height: i32) {
    let picture = crate::aspect_image::AspectImage::new(texture);
    picture.add_css_class("md-image");
    picture.set_cap(width, height);
    picture.set_size_request(width, height);
    picture.set_halign(gtk4::Align::Center);
    picture.set_valign(gtk4::Align::Center);
    picture.set_opacity(0.0);
    outer.append(&picture);
    glib::idle_add_local_once(move || picture.set_opacity(1.0));
}

/// A decode landed: wrap, cache, and fill every widget that was waiting.
fn deliver(key: &Key, max_edge: u32, result: Result<Decoded, ()>) {
    let waiters = WAITING.with(|w| w.borrow_mut().remove(key)).unwrap_or_default();
    let Ok(decoded) = result else {
        // Nothing to show; the placeholder simply stays. The failure is not
        // cached, so a re-open tries again (a decode that raced a file being
        // written is not permanent).
        return;
    };
    DIMS.with(|d| {
        d.borrow_mut().insert(
            key.path.clone(),
            (decoded.width, decoded.height, key.len),
        );
    });
    let bytes = glib::Bytes::from_owned(decoded.rgba);
    let texture = gtk4::gdk::MemoryTexture::new(
        decoded.width as i32,
        decoded.height as i32,
        gtk4::gdk::MemoryFormat::R8g8b8a8Premultiplied,
        &bytes,
        (decoded.width as usize * 4) as usize,
    );
    let texture: gtk4::gdk::Texture = texture.upcast();
    let footprint = decoded.width as usize * decoded.height as usize * 4;
    CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if !cache.map.contains_key(key) {
            cache.map.insert(key.clone(), texture.clone());
            cache.sizes.insert(key.clone(), footprint);
            cache.order.push_back(key.clone());
            cache.bytes += footprint;
            // Evict the least recently used until the cache fits inside its
            // budget again. The policy is a pure function so it can be
            // tested without a display.
            for evict in eviction_order(&cache.order, &cache.sizes, cache.bytes, CACHE_BUDGET) {
                cache.map.remove(&evict);
                if let Some(size) = cache.sizes.remove(&evict) {
                    cache.bytes = cache.bytes.saturating_sub(size);
                }
                if let Some(at) = cache.order.iter().position(|k| k == &evict) {
                    cache.order.remove(at);
                }
            }
        }
    });
    let _ = max_edge;
    for outer in waiters {
        while let Some(child) = outer.first_child() {
            outer.remove(&child);
        }
        // A waiter that asked for a fixed size keeps it: the cover
        // thumbnail must land at the size the shelf chose, not at the
        // picture's own aspect.
        match outer.size_request() {
            (0, 0) => append_picture(&outer, &texture),
            (w, h) => append_sized_picture(&outer, &texture, w, h),
        }
    }
}

/// Hand a job to the pool: run it now if a worker slot is free, queue it
/// otherwise. A finishing worker drains the queue itself, so nothing is
/// scheduled twice.
fn submit(job: Job) {
    let mut guard = QUEUE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (queue, in_flight) = &mut *guard;
    if *in_flight >= IN_FLIGHT_CAP {
        queue.push_back(job);
        return;
    }
    *in_flight += 1;
    drop(guard);
    let tx = channel().0.clone();
    std::thread::spawn(move || {
        let mut next = Some(job);
        while let Some(job) = next.take() {
            let result = decode(&job);
            let _ = tx.send((job.key, job.max_edge, result));
            let mut guard = QUEUE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let (queue, in_flight) = &mut *guard;
            next = queue.pop_front();
            if next.is_none() {
                *in_flight -= 1;
            }
        }
    });
}

/// Read, decode, downsample, premultiply — everything expensive, on the
/// worker thread.
fn decode(job: &Job) -> Result<Decoded, ()> {
    let reader = image::ImageReader::open(&job.key.path)
        .map_err(|_| ())?
        // Sniff the bytes, not the name. The remote fetcher caches under
        // content-hash names (`img-<hash>.img`) with no real extension, and
        // at least one book cover is a PNG wearing a .jpg — an extension-
        // driven reader refuses every one of those, and the old GDK path
        // (which sniffs) is what made them all display before this cache
        // existed.
        .with_guessed_format()
        .map_err(|_| ())?;
    let img = reader.decode().map_err(|_| ())?;
    let rgba: Vec<u8>;
    let (width, height);
    if img.width() > job.max_edge || img.height() > job.max_edge {
        // Triangle (bilinear) is the resampler that keeps photos looking
        // like photos at reading size; Lanczos costs three times as much
        // for a difference nobody sees at 1400 px.
        let scale = scale_to_fit(&img, job.max_edge);
        let small = image::imageops::resize(
            &img,
            ((img.width() as f64 * scale).round() as u32).max(1),
            ((img.height() as f64 * scale).round() as u32).max(1),
            image::imageops::FilterType::Triangle,
        );
        width = small.width();
        height = small.height();
        rgba = small.into_raw();
    } else {
        width = img.width();
        height = img.height();
        rgba = img.to_rgba8().into_raw();
    }
    let mut rgba = rgba;
    premultiply(&mut rgba);
    Ok(Decoded {
        width,
        height,
        rgba,
    })
}

fn scale_to_fit(img: &image::DynamicImage, max_edge: u32) -> f64 {
    let long = img.width().max(img.height());
    if long <= max_edge {
        1.0
    } else {
        max_edge as f64 / long as f64
    }
}

/// Straight alpha to premultiplied, in place — the format the texture below
/// is created with, so no conversion happens on the UI thread.
fn premultiply(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u16;
        if a == 255 {
            continue;
        }
        for c in &mut px[..3] {
            *c = (*c as u16 * a / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key for the tests, distinct by name.
    fn key(name: &str) -> Key {
        Key {
            path: PathBuf::from(name),
            mtime: std::time::SystemTime::UNIX_EPOCH,
            len: 1,
            //  the struct's `max_edge` postdates this test helper; the
            // helpers' own value is not load-bearing (distinct by name/path).
            max_edge: 0,
        }
    }

    /// A cache's recency list and sizes.
    fn cache_of(entries: &[(&str, usize)]) -> (VecDeque<Key>, HashMap<Key, usize>) {
        let mut order = VecDeque::new();
        let mut sizes = HashMap::new();
        for (name, size) in entries {
            let key = key(name);
            order.push_back(key.clone());
            sizes.insert(key, *size);
        }
        (order, sizes)
    }

    #[test]
    fn a_cache_inside_its_budget_evicts_nothing() {
        let (order, sizes) = cache_of(&[("a", 100), ("b", 100)]);
        let dropped = eviction_order(&order, &sizes, 200, 1000);
        assert!(dropped.is_empty());
    }

    #[test]
    fn the_least_recently_used_goes_first() {
        let (order, sizes) = cache_of(&[("a", 100), ("b", 100), ("c", 100)]);
        // 300 bytes over a 200-byte budget: the oldest goes.
        let dropped = eviction_order(&order, &sizes, 300, 200);
        assert_eq!(dropped, vec![key("a")]);
    }

    #[test]
    fn eviction_continues_until_the_cache_fits() {
        let (order, sizes) = cache_of(&[("a", 100), ("b", 100), ("c", 100), ("d", 100)]);
        // 400 bytes over a 150-byte budget: three of the four have to go.
        let dropped = eviction_order(&order, &sizes, 400, 150);
        assert_eq!(dropped, vec![key("a"), key("b"), key("c")]);
    }

    #[test]
    fn the_last_entry_is_never_evicted() {
        let (order, sizes) = cache_of(&[("a", 1000)]);
        // One picture, a thousand bytes, a ten-byte budget: the picture is
        // over the budget on its own, but it is the *only* one, and it is
        // the one on screen. It stays.
        let dropped = eviction_order(&order, &sizes, 1000, 10);
        assert!(dropped.is_empty());
    }

    #[test]
    fn a_missing_size_counts_as_nothing() {
        let (order, sizes) = cache_of(&[("a", 0), ("b", 100)]);
        // 'a' has no known size, so evicting it frees nothing. It is still
        // the least recently used, so it goes first — and then 'b' is the
        // last entry, which is never evicted, so the loop stops there with
        // the cache still over the budget. That is the deliberate trade:
        // a picture that is on screen beats a budget.
        let dropped = eviction_order(&order, &sizes, 100, 50);
        assert_eq!(dropped, vec![key("a")]);
    }

    #[test]
    fn eviction_stops_when_one_entry_is_left() {
        let (order, sizes) = cache_of(&[("a", 100), ("b", 100)]);
        // 200 bytes over a zero-byte budget: one of the two has to go, and
        // the other is the last entry and stays.
        let dropped = eviction_order(&order, &sizes, 200, 0);
        assert_eq!(dropped, vec![key("a")]);
    }

    #[test]
    fn an_empty_cache_evicts_nothing() {
        let (order, sizes) = cache_of(&[]);
        let dropped = eviction_order(&order, &sizes, 1000, 0);
        assert!(dropped.is_empty());
    }

    /// The scale-down maths: a picture is capped on its *long* edge, so a
    /// portrait phone photo and a landscape one both come back inside the
    /// cap.
    #[test]
    fn the_long_edge_is_what_is_capped() {
        let tall = image::DynamicImage::new_rgb8(800, 4000);
        let scale = scale_to_fit(&tall, 1400);
        assert!((tall.width() as f64 * scale).round() <= 1400.0);
        assert!((tall.height() as f64 * scale).round() <= 1400.0);
        // The long edge is exactly the cap.
        assert_eq!((tall.height() as f64 * scale).round() as u32, 1400);

        let wide = image::DynamicImage::new_rgb8(4000, 800);
        let scale = scale_to_fit(&wide, 1400);
        assert_eq!((wide.width() as f64 * scale).round() as u32, 1400);
    }

    #[test]
    fn a_picture_inside_the_cap_is_not_scaled() {
        let small = image::DynamicImage::new_rgb8(100, 200);
        assert_eq!(scale_to_fit(&small, 1400), 1.0);
    }

    /// Straight alpha to premultiplied. A half-transparent red must become
    /// a half-transparent *dark* red — the colour channels scaled by the
    /// alpha — or the texture is drawn with the wrong colour where it is
    /// not opaque.
    #[test]
    fn premultiply_scales_the_colour_channels() {
        let mut rgba = vec![255, 0, 0, 128];
        premultiply(&mut rgba);
        assert_eq!(rgba[0], 128);
        assert_eq!(rgba[1], 0);
        assert_eq!(rgba[2], 0);
        // The alpha itself is untouched.
        assert_eq!(rgba[3], 128);
    }

    #[test]
    fn premultiply_leaves_opaque_pixels_alone() {
        let mut rgba = vec![12, 34, 56, 255];
        premultiply(&mut rgba);
        assert_eq!(rgba, vec![12, 34, 56, 255]);
    }

    #[test]
    fn premultiply_leaves_transparent_pixels_transparent() {
        let mut rgba = vec![255, 255, 255, 0];
        premultiply(&mut rgba);
        assert_eq!(rgba, vec![0, 0, 0, 0]);
    }

    /// The reader sniffs bytes, not names. The remote fetcher caches under
    /// `img-<hash>.img` — no extension a decoder could pick from — and a
    /// book cover on this box is a PNG wearing a .jpg. An extension-driven
    /// reader refused every note image this user has; this test pins the
    /// sniff with both lies at once.
    #[test]
    fn a_png_wearing_a_foreign_name_decodes() {
        let dir = std::env::temp_dir().join("imgcache-test");
        std::fs::create_dir_all(&dir).unwrap();
        // A real PNG — a 1x1 the image crate writes itself — saved under a
        // name that says nothing and lies: first as the fetcher's
        // extension-less `.img`, then as a `.jpg` that is not one.
        for lying in ["img-deadbeef.img", "not-really-a-jpg.jpg"] {
            let path = dir.join(lying);
            image::DynamicImage::new_rgb8(1, 1)
                .save_with_format(&path, image::ImageFormat::Png)
                .expect("the 1x1 PNG writes");
            let len = std::fs::metadata(&path).unwrap().len();
            let key = Key {
                path: path.clone(),
                mtime: std::time::SystemTime::UNIX_EPOCH,
                len,
                max_edge: 0,
            };
            let decoded = decode(&Job {
                key,
                max_edge: NOTE_MAX_EDGE,
            })
            .unwrap_or_else(|_| panic!("content sniffing must decode a PNG named {lying}"));
            assert_eq!((decoded.width, decoded.height), (1, 1));
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
