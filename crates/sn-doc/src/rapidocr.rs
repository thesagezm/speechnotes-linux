//! RapidOCR: PaddleOCR's PP-OCRv4 detection + recognition models through
//! ONNX Runtime — the OCR engine, and tesseract's better across the board
//! on scans and photographs (tesseract stays as the fallback for a box
//! without the runtime or the models).
//!
//! The shapes are the standard three-stage pipeline. Detect: the page image
//! is scaled so its short side is at least 736 px (the model's comfort
//! zone), normalised to ±1, and the network returns a probability map of
//! "text-ish here"; the map is binarised, its connected components become
//! minimum-area rectangles (convex hull, rotating calipers), each rectangle
//! is scored against the map and inflated by the unclip ratio so the crop
//! keeps whole glyphs. Recognise: each rectangle is warped upright, resized
//! to a 48-px strip, and the CTC head reads it left to right against the
//! character dictionary. Assemble: lines are ordered top-to-bottom,
//! left-to-right, and their texts feed the same papero layout the text
//! layer gets — paragraphs, columns, headings — by becoming pseudo-glyphs
//! with even spacing across each line's box.
//!
//! The runtime is found, not linked: `libonnxruntime.so` from the same
//! install sn-tts uses (`ORT_DYLIB_PATH`, the tools prefix, or the Swift-era
//! `~/.local/share` path), the models from `<data>/speechnotes/models/ocr`.
//! Either missing means "not available": `available()` is the one gate, and
//! the caller falls back to tesseract.

use std::path::PathBuf;
use std::sync::{Mutex, Once, OnceLock};

use ort::value::Tensor;

use crate::ocr::data_root;
use crate::pdftext::{self, Char};

/// One recognised line of text, its confidence, and its box in the page
/// image's pixels (origin top-left, like everything screen-space here).
#[derive(Debug, Clone)]
pub struct Line {
    pub text: String,
    pub score: f32,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

// ---------------------------------------------------------------------------
// Runtime discovery: the dylib, the models, one init per process.
// ---------------------------------------------------------------------------

/// The ONNX Runtime dylib: `ORT_DYLIB_PATH` (set by the app for sn-tts's
/// sake), then the package layouts — system install, user install, the
/// tools prefix, and the bare onnxruntime release layout older installs
/// used. Mirrors sn-tts's lookup.
fn dylib_path() -> PathBuf {
    if let Some(path) = std::env::var_os("ORT_DYLIB_PATH") {
        return PathBuf::from(path);
    }
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("/usr/lib/speechnotes/libonnxruntime.so"),
        data_root().join("tools/lib/libonnxruntime.so"),
    ];
    if let Some(appdir) = std::env::var_os("APPDIR") {
        let mut p = PathBuf::from(appdir);
        p.push("usr/lib/speechnotes/libonnxruntime.so");
        candidates.push(p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut home = PathBuf::from(home);
        home.push(".local/lib/speechnotes/libonnxruntime.so");
        candidates.push(home);
        let mut legacy = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        legacy.push(".local/share/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so");
        candidates.push(legacy);
    }
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("/usr/lib/speechnotes/libonnxruntime.so"))
}

/// `ort` reads the dylib path once per process from this env var at the
/// first use of the API. Set it if it is not already set.
fn ensure_dylib_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("ORT_DYLIB_PATH").is_none() {
            std::env::set_var("ORT_DYLIB_PATH", dylib_path());
        }
    });
}

/// `<data>/speechnotes/models/ocr` — where install.sh (or a no-sudo drop)
/// puts the det/rec models and the recognition dictionary. (`data_root`
/// is the bare XDG root; the app's own subtree hangs off `speechnotes/`.)
fn models_dir() -> PathBuf {
    data_root().join("speechnotes/models/ocr")
}

struct Runtime {
    det: Mutex<ort::session::Session>,
    rec: Mutex<ort::session::Session>,
    /// The CTC dictionary: dictionary line `i` is class `i + 1`; class 0 is
    /// the blank. PaddleOCR's exports may append a space class.
    dict: Vec<String>,
    /// True when the class count implies a trailing space class.
    space_class: bool,
}

static RUNTIME: OnceLock<Option<Runtime>> = OnceLock::new();

/// The loaded runtime, or `None` when anything is missing — the caller
/// falls back to tesseract. Errors are logged, never panicked.
fn runtime() -> Option<&'static Runtime> {
    RUNTIME.get_or_init(|| {
        ensure_dylib_env();
        if !dylib_path().is_file() {
            log::info!("rapidocr: no onnxruntime dylib at {}", dylib_path().display());
            return None;
        }
        let dir = models_dir();
        let det_path = dir.join("ch_PP-OCRv4_det_infer.onnx");
        let rec_path = dir.join("ch_PP-OCRv4_rec_infer.onnx");
        let dict_path = dir.join("rec_dict.txt");
        if !(det_path.is_file() && rec_path.is_file() && dict_path.is_file()) {
            log::info!("rapidocr: models incomplete under {}", dir.display());
            return None;
        }
        let dict: Vec<String> = std::fs::read_to_string(&dict_path)
            .ok()?
            .lines()
            .map(str::to_string)
            .collect();
        let det = load_session(&det_path)?;
        let rec = load_session(&rec_path)?;
        log::info!("rapidocr ready: {} dictionary entries", dict.len());
        Some(Runtime {
            det: Mutex::new(det),
            rec: Mutex::new(rec),
            space_class: false, // refined against the rec output's width below
            dict,
        })
    })
    .as_ref()
}

fn load_session(path: &PathBuf) -> Option<ort::session::Session> {
    ort::session::Session::builder()
        .map_err(|e| log::warn!("rapidocr: session builder: {e}"))
        .ok()?
        .commit_from_file(path)
        .map_err(|e| log::warn!("rapidocr: {}: {e}", path.display()))
        .ok()
}

/// Whether OCR through RapidOCR can run: the dylib and both models resolved.
pub fn available() -> bool {
    runtime().is_some()
}

// ---------------------------------------------------------------------------
// The pipeline, top level.
// ---------------------------------------------------------------------------

/// Recognise one page image (PNG bytes, as pdftoppm writes them) and return
/// its lines in reading order.
///
/// One recognition at a time, process-wide. The sessions are behind their
/// own mutexes, but the tensors are built *outside* them, and ONNX
/// Runtime's value API is not thread-safe in this build: two threads
/// recognising at once — the background warm-up and the reader's own read
/// of the page on screen — crashed the inference with `GetElementType is
/// not implemented` on a fraction of the pages (12 failures in one
/// morning's log, every one of them while two threads were mid-page). The
/// engine is the bottleneck anyway; serialising it costs nothing but the
/// queue a second caller was going to wait for regardless.
pub fn recognize(png: &[u8]) -> Result<Vec<Line>, String> {
    static ENGINE: Mutex<()> = Mutex::new(());
    let _engine = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    let rt = runtime().ok_or_else(|| "rapidocr runtime not available".to_string())?;
    let img = image::load_from_memory(png).map_err(|e| format!("ocr image: {e}"))?;
    let img = img.to_rgb8();
    let quads = detect(rt, &img)?;
    log::debug!("rapidocr: {} detection boxes", quads.len());
    let mut lines = Vec::with_capacity(quads.len());
    for quad in &quads {
        let (text, score) = recognize_line(rt, &img, quad)?;
        if score < 0.5 || text.trim().is_empty() {
            // The tuning loop's recall evidence: a box the detector found
            // but the recogniser could not read is a miss this log names.
            log::debug!(
                "rapidocr: line dropped (score {score:.2}, text {:?})",
                text.chars().take(40).collect::<String>()
            );
            continue;
        }
        let (x0, y0, x1, y1) = quad.bounds();
        lines.push(Line {
            text: restore_sentence_spaces(&text),
            score,
            x0,
            y0,
            x1,
            y1,
        });
    }
    order_lines(&mut lines);
    log::debug!("rapidocr: {} lines recognised", lines.len());
    Ok(lines)
}

/// The rec model drops the space after a sentence period now and then
/// ("cervix.Note"): restore one where a 3+-letter lowercase run runs
/// straight into a dot and a capitalised word. Shorter runs ("e.g.",
/// "x.Ray") and everything a digit or lowercase letter follows ("3.14",
/// "file.txt") stay untouched, and an uppercase before the dot ("MMR.Vax")
/// is no sentence either.
fn restore_sentence_spaces(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 2);
    for (i, c) in chars.iter().copied().enumerate() {
        out.push(c);
        if c == '.'
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_uppercase())
            && chars.get(i + 2).is_some_and(|n| n.is_alphabetic())
        {
            let mut run = 0;
            while run < i && chars[i - 1 - run].is_ascii_lowercase() {
                run += 1;
            }
            if run >= 3 {
                out.push(' ');
            }
        }
    }
    out
}

/// One page's OCR text: the lines' own reading order, blank lines between
/// paragraphs. The plain join — `page_image_text` is the papero-layout
/// version the reader prefers.
pub fn page_text(png: &[u8]) -> Result<String, String> {
    Ok(join_lines(&recognize(png)?))
}

/// One page's OCR text through papero's layout: the lines become
/// pseudo-glyphs (even spacing across the box, the box height as the em)
/// and come back as paragraphs, columns and headings, exactly like a text
/// layer's page.
pub fn page_image_text(png: &[u8]) -> Result<String, String> {
    let lines = recognize(png)?;
    Ok(pdftext::layout_text(chars_from_lines(&lines)).text)
}

/// The lines' texts joined the way tesseract prints: one visual line per
/// line, blank lines between paragraphs (a vertical gap over a body-size
/// line height).
fn join_lines(lines: &[Line]) -> String {
    let mut out = String::new();
    let body = body_size(lines);
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            let gap = line.y0 - lines[i - 1].y1;
            if gap > body * 0.8 {
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str(line.text.trim());
    }
    out
}

/// The modal line height, the OCR page's stand-in for a body font size.
fn body_size(lines: &[Line]) -> f32 {
    let mut tally: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for l in lines {
        *tally.entry((l.y1 - l.y0).round() as u32).or_default() += 1;
    }
    tally
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(h, _)| h as f32)
        .unwrap_or(20.0)
}

/// Sort lines top-to-bottom, left-to-right: rows are boxes whose centres
/// sit within 60% of the smaller box's height; rows sort by centre, boxes
/// within a row by left edge.
fn order_lines(lines: &mut [Line]) {
    lines.sort_by(|a, b| {
        a.cy()
            .partial_cmp(&b.cy())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Reorder within rows in place: walk the sorted list, collect each row,
    // sort it by x.
    let mut start = 0;
    while start < lines.len() {
        let row_cy = lines[start].cy();
        let row_h = lines[start].y1 - lines[start].y0;
        let mut end = start + 1;
        while end < lines.len() && (lines[end].cy() - row_cy).abs() < 0.6 * row_h {
            end += 1;
        }
        lines[start..end].sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap_or(std::cmp::Ordering::Equal));
        start = end;
    }
}

impl Line {
    fn cy(&self) -> f32 {
        (self.y0 + self.y1) / 2.0
    }
}

/// OCR lines as papero glyphs: each line's characters spread evenly across
/// its box, the box height standing in for the em. Spaces in the text ride
/// as pending flags, the text layer's own convention.
fn chars_from_lines(lines: &[Line]) -> Vec<Char> {
    let mut out = Vec::new();
    for (run, line) in lines.iter().enumerate() {
        let n = line.text.chars().filter(|c| !c.is_whitespace()).count();
        if n == 0 {
            continue;
        }
        let size = (line.y1 - line.y0).max(6.0) as f64;
        let w = (line.x1 - line.x0).max(1.0) / n as f32;
        let mut pending = false;
        for (k, c) in line.text.chars().enumerate() {
            if c.is_whitespace() {
                pending = true;
                continue;
            }
            out.push(Char {
                c,
                x0: (line.x0 + w * k as f32) as f64,
                x1: (line.x0 + w * (k + 1) as f32) as f64,
                y0: line.y0 as f64,
                y1: line.y1 as f64,
                size,
                space: std::mem::take(&mut pending),
                run: run as u32,
            });
        }
    }
    out
}

/// The first output of a run, flattened: (dims, data). PaddleOCR's exports
/// name their single outputs inconsistently; the first is the only one.
fn first_tensor_f32(
    outputs: &mut ort::session::SessionOutputs<'_>,
    what: &str,
) -> Result<(Vec<i64>, Vec<f32>), String> {
    let key = outputs
        .iter()
        .next()
        .map(|(k, _)| k.to_string())
        .ok_or_else(|| format!("{what} returned nothing"))?;
    let value = outputs
        .remove(key.as_str())
        .ok_or_else(|| format!("{what}: output vanished"))?;
    let (shape, data) = value
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("{what} output: {e}"))?;
    Ok((shape.to_vec(), data.to_vec()))
}

// ---------------------------------------------------------------------------
// Detection: image → scaled probability map → text boxes.
// ---------------------------------------------------------------------------

/// Detection thresholds, PaddleOCR's own (the RapidOCR config verbatim).
const DET_THRESH: f32 = 0.3;
const DET_BOX_THRESH: f32 = 0.5;
const DET_UNCLIP: f32 = 1.6;
const DET_LIMIT_SIDE: u32 = 736;
const DET_MAX_SIDE: u32 = 2000;

fn detect(rt: &Runtime, img: &image::RgbImage) -> Result<Vec<Quad>, String> {
    let (w, h) = (img.width(), img.height());
    // Short side to at least 736 (upscale a small page), long side capped
    // at 2000; the input itself rounds to a multiple of 32 by padding.
    let mut scale = 1.0f32;
    if w.min(h) < DET_LIMIT_SIDE {
        scale = DET_LIMIT_SIDE as f32 / w.min(h) as f32;
    }
    if (w.max(h) as f32 * scale) > DET_MAX_SIDE as f32 {
        scale = DET_MAX_SIDE as f32 / w.max(h) as f32;
    }
    let sw = ((w as f32 * scale).round() as u32).max(32);
    let sh = ((h as f32 * scale).round() as u32).max(32);
    let pw = (sw + 31) / 32 * 32;
    let ph = (sh + 31) / 32 * 32;

    let scaled = image::imageops::resize(img, sw, sh, image::imageops::FilterType::Lanczos3);
    let (mw, mh) = (pw as usize, ph as usize);
    let mut input = vec![0.5f32; mw * mh * 3];
    for y in 0..sh as usize {
        for x in 0..sw as usize {
            let px = scaled.get_pixel(x as u32, y as u32);
            // PaddleOCR's models are OpenCV-trained: the planes go in BGR
            // order even though the crate hands us RGB.
            let bgr = [px.0[2], px.0[1], px.0[0]];
            for (c, v) in bgr.iter().enumerate() {
                input[(c * mh + y) * mw + x] = (*v as f32 / 255.0 - 0.5) / 0.5;
            }
        }
    }
    let mut det = rt.det.lock().unwrap_or_else(|e| e.into_inner());
    let input = Tensor::from_array((vec![1i64, 3, ph as i64, pw as i64], input))
        .map_err(|e| format!("det input: {e}"))?
        .into_dyn();
    let mut outputs = det
        .run(vec![("x", input)])
        .map_err(|e| format!("det inference: {e}"))?;
    let (shape, map) = first_tensor_f32(&mut outputs, "det")?;
    log::debug!("rapidocr det: output shape {shape:?}");
    let mh = shape.get(2).copied().unwrap_or(0) as usize;
    let mw = shape.get(3).copied().unwrap_or(0) as usize;
    if log::log_enabled!(log::Level::Debug) {
        let at = |y: usize, x: usize| map.get(y * mw + x).copied().unwrap_or(f32::NAN);
        log::debug!("rapidocr det: samples y1065: {} {} {} | transposed y*1440+x-ish {} {}",
            at(1065, 300), at(1065, 301), at(1065, 302), at(300, 500), at(301, 500));
    }
    if mh == 0 || mw == 0 {
        return Ok(Vec::new());
    }

    // Binarise and dilate (one 3x3 pass), then find components.
    let map_max = map.iter().copied().fold(0.0f32, f32::max);
    let map_hot = map.iter().filter(|&&v| v >= DET_THRESH).count();
    log::debug!("rapidocr det: map max {map_max}, pixels over thresh: {map_hot}");
    let bin = dilate(&map, mw, mh, DET_THRESH);
    let comps = components(&bin, mw, mh);
    log::debug!("rapidocr det: prob map {}x{}, {} components", mw, mh, comps.len());

    let mut quads: Vec<(Quad, f32)> = Vec::new();
    for pts in comps {
        let hull = convex_hull(&pts);
        let quad = min_area_rect(&hull);
        if log::log_enabled!(log::Level::Debug) {
            let (bx0, by0, bx1, by1) = quad.bounds();
            log::debug!("rapidocr det: comp {} px, bounds {bx0:.0},{by0:.0} {bx1:.0},{by1:.0}", pts.len());
        }
        let score = comp_score(&map, mw, &pts);
        if score < DET_BOX_THRESH {
            log::debug!("rapidocr det: box dropped, score {score}");
            continue;
        }
        let big = unclip(&quad, DET_UNCLIP);
        let big = min_area_rect(&convex_hull(&big.pts()));
        // Drop specks and one-dimensional slivers, then scale back to the
        // page image's own pixels.
        let (x0, y0, x1, y1) = big.bounds();
        if (x1 - x0) < 3.0 || (y1 - y0) < 3.0 || pts.len() < 10 {
            continue;
        }
        let pts: Vec<(f32, f32)> = big
            .pts()
            .iter()
            .map(|(x, y)| (x / scale, y / scale))
            .collect();
        quads.push((Quad::new(pts.try_into().unwrap()), score));
    }
    // Same text twice at the same place is a double detection, never a
    // layout: suppress boxes that overlap an earlier, higher-scored one.
    quads.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<Quad> = Vec::new();
    'keep: for (q, _) in &quads {
        for k in &kept {
            if iou(q.bounds(), k.bounds()) > 0.5 {
                continue 'keep;
            }
        }
        kept.push(*q);
    }
    Ok(kept)
}

/// Intersection-over-union of two axis-aligned bounds.
fn iou(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> f32 {
    let iw = (a.2.min(b.2) - a.0.max(b.0)).max(0.0);
    let ih = (a.3.min(b.3) - a.1.max(b.1)).max(0.0);
    let inter = iw * ih;
    let area_a = (a.2 - a.0) * (a.3 - a.1);
    let area_b = (b.2 - b.0) * (b.3 - b.1);
    let denom = (area_a + area_b - inter).max(1e-6);
    inter / denom
}

/// The probability map binarised at `thresh` and dilated by one pixel —
/// detection works on grown regions so thin strokes survive.
fn dilate(map: &[f32], w: usize, h: usize, thresh: f32) -> Vec<bool> {
    let at = |x: usize, y: usize| map[y * w + x] >= thresh;
    let mut out = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if at(x, y) {
                for dy in 0..3 {
                    for dx in 0..3 {
                        let nx = x + dx - 1;
                        let ny = y + dy - 1;
                        if nx < w && ny < h {
                            out[ny * w + nx] = true;
                        }
                    }
                }
            }
        }
    }
    out
}

/// Connected components (4-neighbour, iterative — recursion would blow the
/// stack on a full-page region). Each component is its pixel list.
fn components(bin: &[bool], w: usize, h: usize) -> Vec<Vec<(f32, f32)>> {
    let mut seen = vec![false; w * h];
    let mut out = Vec::new();
    for start in 0..w * h {
        if !bin[start] || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let mut pts = Vec::new();
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            pts.push((x as f32, y as f32));
            let push = |nx: usize, ny: usize, seen: &mut Vec<bool>, stack: &mut Vec<usize>| {
                if nx < w && ny < h && bin[ny * w + nx] && !seen[ny * w + nx] {
                    seen[ny * w + nx] = true;
                    stack.push(ny * w + nx);
                }
            };
            push(x + 1, y, &mut seen, &mut stack);
            push(x.wrapping_sub(1), y, &mut seen, &mut stack);
            push(x, y + 1, &mut seen, &mut stack);
            push(x, y.wrapping_sub(1), &mut seen, &mut stack);
        }
        if !pts.is_empty() {
            out.push(pts);
        }
    }
    out
}

/// Mean detection probability over a component's own pixels — PaddleOCR's
/// "fast" score. (A rect-grid sample over the bounding box was tried first
/// and diluted toward zero: a line box is mostly whitespace.)
fn comp_score(map: &[f32], w: usize, pts: &[(f32, f32)]) -> f32 {
    if pts.is_empty() {
        return 0.0;
    }
    let mut sum = 0.0f32;
    for (x, y) in pts {
        sum += map[*y as usize * w + *x as usize];
    }
    sum / pts.len() as f32
}

// ---------------------------------------------------------------------------
// Recognition: one warped strip → CTC text.
// ---------------------------------------------------------------------------

fn recognize_line(rt: &Runtime, img: &image::RgbImage, quad: &Quad) -> Result<(String, f32), String> {
    let (x0, y0, x1, y1) = quad.bounds();
    let ratio = (x1 - x0).max(1.0) / (y1 - y0).max(1.0);
    let out_w = ((48.0 * ratio).round() as u32).clamp(8, 1000);
    let strip = warp_quad(img, quad, out_w, 48);
    if std::env::var_os("SN_DUMP_STRIP").is_some() && !strip.iter().all(|&v| v == strip[0]) {
        static STRIP_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "sn-rec-strip-{}.png",
            STRIP_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = image::save_buffer(&path, &strip, out_w, 48, image::ColorType::Rgb8);
        log::debug!("rapidocr rec: dumped strip {}x{} to {}", out_w, 48, path.display());
    }
    let mut input = vec![0.5f32; (48 * out_w * 3) as usize];
    for y in 0..48 {
        for x in 0..out_w as usize {
            // BGR planes, as above — same OpenCV upbringing.
            for c in 0..3 {
                input[(c * 48 * out_w as usize) + y * out_w as usize + x] =
                    (strip[(y * out_w as usize + x) * 3 + (2 - c)] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
    }
    let mut rec = rt.rec.lock().unwrap_or_else(|e| e.into_inner());
    let input = Tensor::from_array((vec![1i64, 3, 48, out_w as i64], input))
        .map_err(|e| format!("rec input: {e}"))?
        .into_dyn();
    let mut outputs = rec
        .run(vec![("x", input)])
        .map_err(|e| format!("rec inference: {e}"))?;
    let (shape, logits) = first_tensor_f32(&mut outputs, "rec")?;
    let steps = shape.get(1).copied().unwrap_or(0) as usize;
    let classes = shape.get(2).copied().unwrap_or(0) as usize;
    if steps == 0 || classes == 0 {
        return Ok((String::new(), 0.0));
    }

    // The dictionary is fixed but the class count is the model's: reconcile
    // once. PaddleOCR's `use_space_char` appends one space class.
    let dict_len = rt.dict.len();
    let space_class = match classes.cmp(&dict_len) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Equal => false,
        std::cmp::Ordering::Less => {
            return Ok((String::new(), 0.0)); // model and dictionary disagree
        }
    };
    if rt.space_class != space_class {
        log::debug!("rapidocr: rec uses {} classes (dict {}), space class = {space_class}", classes, dict_len);
    }

    // Greedy CTC: best class per step, repeats collapsed, blanks skipped.
    let (mut text, mut total, mut frames) = (String::new(), 0.0f32, 0usize);
    let mut prev = 0usize;
    for t in 0..steps {
        let row = &logits[t * classes..(t + 1) * classes];
        let (best, best_p) = row
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, p)| (i, *p))
            .unwrap_or((0, 0.0));
        if best != 0 && best != prev {
            // Class `classes - 1` is PaddleOCR's appended space class; it has
            // no dictionary line of its own.
            if space_class && best == classes - 1 {
                text.push(' ');
            } else if let Some(ch) = rt.dict.get(best - 1) {
                text.push_str(ch);
            }
            total += best_p;
            frames += 1;
        }
        prev = best;
    }
    let score = if frames == 0 { 0.0 } else { total / frames as f32 };
    log::debug!("rapidocr rec: {} steps, {} classes, frames {frames}, score {score}, text {:?}",
        steps, classes, text.chars().take(40).collect::<String>());
    Ok((text, score))
}

// ---------------------------------------------------------------------------
// Geometry: hull, minimum-area rectangle, unclip, warp.
// ---------------------------------------------------------------------------

/// An ordered quadrilateral in pixel coordinates.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Quad {
    pts: [(f32, f32); 4],
}

impl Quad {
    fn new(pts: [(f32, f32); 4]) -> Self {
        Self { pts }
    }

    fn pts(&self) -> [(f32, f32); 4] {
        self.pts
    }

    /// The axis-aligned bounds.
    fn bounds(&self) -> (f32, f32, f32, f32) {
        let xs = self.pts.map(|p| p.0);
        let ys = self.pts.map(|p| p.1);
        (
            xs.iter().copied().fold(f32::MAX, f32::min),
            ys.iter().copied().fold(f32::MAX, f32::min),
            xs.iter().copied().fold(f32::MIN, f32::max),
            ys.iter().copied().fold(f32::MIN, f32::max),
        )
    }

    fn area(&self) -> f32 {
        let p = &self.pts;
        let mut a = 0.0;
        for i in 0..4 {
            let j = (i + 1) % 4;
            a += p[i].0 * p[j].1 - p[j].0 * p[i].1;
        }
        (a / 2.0).abs()
    }

    fn perimeter(&self) -> f32 {
        let p = &self.pts;
        let mut per = 0.0;
        for i in 0..4 {
            let j = (i + 1) % 4;
            per += ((p[j].0 - p[i].0).powi(2) + (p[j].1 - p[i].1).powi(2)).sqrt();
        }
        per
    }
}

/// Convex hull, Andrew's monotone chain. Collinear points collapse.
fn convex_hull(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut pts = points.to_vec();
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let mut lower: Vec<(f32, f32)> = Vec::new();
    for p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<(f32, f32)> = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

/// The minimum-area rectangle around a convex hull: rotate calipers along
/// each hull edge, keep the covering box with the least area. The four
/// corners come back in hull order.
fn min_area_rect(hull: &[(f32, f32)]) -> Quad {
    assert!(!hull.is_empty(), "min_area_rect of nothing");
    if hull.len() == 1 {
        let (x, y) = hull[0];
        return Quad::new([(x, y); 4]);
    }
    let mut best_area = f32::MAX;
    let mut best: [(f32, f32); 4] = [hull[0]; 4];
    for i in 0..hull.len() {
        let a = hull[i];
        let b = hull[(i + 1) % hull.len()];
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-6 {
            continue;
        }
        // Unit vectors along the edge and perpendicular to it.
        let (ux, uy) = (dx / len, dy / len);
        let (vx, vy) = (-uy, ux);
        let mut u_min = f32::MAX;
        let mut u_max = f32::MIN;
        let mut v_min = f32::MAX;
        let mut v_max = f32::MIN;
        for p in hull {
            let u = p.0 * ux + p.1 * uy;
            let v = p.0 * vx + p.1 * vy;
            u_min = u_min.min(u);
            u_max = u_max.max(u);
            v_min = v_min.min(v);
            v_max = v_max.max(v);
        }
        let area = (u_max - u_min) * (v_max - v_min);
        if area < best_area {
            best_area = area;
            let corner = |u: f32, v: f32| (u * ux + v * vx, u * uy + v * vy);
            best = [
                corner(u_min, v_min),
                corner(u_max, v_min),
                corner(u_max, v_max),
                corner(u_min, v_max),
            ];
        }
    }
    Quad::new(best)
}

/// Inflate a rectangle outward by Laplace's approximation — the offset that
/// grows the area by `ratio` — so the recognition crop keeps whole glyphs
/// instead of shaving their edges (PaddleOCR's `unclip`).
fn unclip(quad: &Quad, ratio: f32) -> Quad {
    let area = quad.area();
    let per = quad.perimeter().max(1e-6);
    let d = area * ratio / per;
    let p = quad.pts();
    // Offset each edge outward along its own outward normal, then
    // intersect consecutive offset edges — the polygon offset, done small.
    let mut out = [(0.0f32, 0.0f32); 4];
    for i in 0..4 {
        let prev = p[(i + 3) % 4];
        let cur = p[i];
        let next = p[(i + 1) % 4];
        // The polygon winds counter-clockwise in screen space when area is
        // positive from the shoelace as written; normals point outward.
        let n1 = edge_normal(prev, cur);
        let n2 = edge_normal(cur, next);
        let e1 = line_through(cur, n1, d);
        let e2 = line_through(cur, n2, d);
        out[i] = line_intersection(e1, e2).unwrap_or(cur);
    }
    Quad::new(out)
}

/// Outward unit normal of the edge `a → b` (screen coordinates, y down:
/// the right-hand side of the walk direction is outward).
fn edge_normal(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    (dy / len, -dx / len)
}

/// The line through `p` along direction `dir`, offset by `d` along `n`.
/// Returns (point, direction).
fn line_through(p: (f32, f32), n: (f32, f32), d: f32) -> ((f32, f32), (f32, f32)) {
    (
        (p.0 + n.0 * d, p.1 + n.1 * d),
        (-n.1, n.0),
    )
}

/// Intersection of two (point, direction) lines.
fn line_intersection(l1: ((f32, f32), (f32, f32)), l2: ((f32, f32), (f32, f32))) -> Option<(f32, f32)> {
    let ((p1, d1), (p2, d2)) = (l1, l2);
    let den = d1.0 * d2.1 - d1.1 * d2.0;
    if den.abs() < 1e-9 {
        return None;
    }
    let t = ((p2.0 - p1.0) * d2.1 - (p2.1 - p1.1) * d2.0) / den;
    Some((p1.0 + d1.0 * t, p1.1 + d1.1 * t))
}

/// Warp the quadrilateral `quad` out of `img` into a `w`×`h` upright
/// rectangle: a homography from the quad's corners, bilinear-sampled.
/// Output is RGB, row-major.
fn warp_quad(img: &image::RgbImage, quad: &Quad, w: u32, h: u32) -> Vec<u8> {
    let p = quad.pts();
    // Corners ordered top-left, top-right, bottom-right, bottom-left.
    let quad_pts = order_corners(p);
    let rect = [(0.0f32, 0.0f32), (w as f32, 0.0), (w as f32, h as f32), (0.0, h as f32)];
    // homography(a, b) solves the map b → a; what sampling needs is
    // output-pixel → source-pixel, i.e. rect → quad.
    let inv = homography(&quad_pts, &rect);
    let mut out = vec![0u8; (w * h * 3) as usize];
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = apply(&inv, x as f32 + 0.5, y as f32 + 0.5);
            out[((y * w + x) * 3) as usize] = sample(img, sx, sy, 0);
            out[((y * w + x) * 3 + 1) as usize] = sample(img, sx, sy, 1);
            out[((y * w + x) * 3 + 2) as usize] = sample(img, sx, sy, 2);
        }
    }
    out
}

/// Order four corners top-left, top-right, bottom-right, bottom-left.
///
///  the lexicographic sort this replaces named the top-left corner
/// by smallest (x, y) — true only while the quad's left edge is exactly
/// vertical. A line tilted the other way sorts its bottom-left corner
/// first, the "diagonal" then joins two adjacent corners, and the
/// homography becomes a bowtie: the recogniser received horizontally
/// shredded rows and answered blank, dropping 4 of 13 readable lines on a
/// page of the user's Obstetrics deck (measured 2026-10-30, ocr-dump +
/// SN_DUMP_STRIP). The x+y / y−x rule names each corner by geometry, not
/// by axis luck.
fn order_corners(pts: [(f32, f32); 4]) -> [(f32, f32); 4] {
    let by = |key: &dyn Fn(&(f32, f32)) -> f32| {
        pts.iter()
            .copied()
            .min_by(|a, b| key(a).total_cmp(&key(b)))
            .unwrap()
    };
    let tl = by(&|p| p.0 + p.1);
    let br = pts
        .iter()
        .copied()
        .max_by(|a, b| (a.0 + a.1).total_cmp(&(b.0 + b.1)))
        .unwrap();
    let tr = by(&|p| p.1 - p.0);
    let bl = pts
        .iter()
        .copied()
        .max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)))
        .unwrap();
    [tl, tr, br, bl]
}

/// Homography mapping `src` corners onto `dst` corners (both TL,TR,BR,BL),
/// solved as an 8×8 linear system by Gaussian elimination.
fn homography(src: &[(f32, f32); 4], dst: &[(f32, f32); 4]) -> [f32; 9] {
    let mut a = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let (x, y) = (dst[i].0 as f64, dst[i].1 as f64);
        let (u, v) = (src[i].0 as f64, src[i].1 as f64);
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..8 {
        let piv = (col..8)
            .max_by(|r1: &usize, r2: &usize| a[*r1][col].abs().total_cmp(&a[*r2][col].abs()))
            .unwrap();
        a.swap(col, piv);
        let d = a[col][col];
        for j in col..9 {
            a[col][j] /= d;
        }
        for r in 0..8 {
            if r != col && a[r][col] != 0.0 {
                let f = a[r][col];
                for j in col..9 {
                    a[r][j] -= f * a[col][j];
                }
            }
        }
    }
    [
        a[0][8] as f32,
        a[1][8] as f32,
        a[2][8] as f32,
        a[3][8] as f32,
        a[4][8] as f32,
        a[5][8] as f32,
        a[6][8] as f32,
        a[7][8] as f32,
        1.0,
    ]
}

fn apply(h: &[f32; 9], x: f32, y: f32) -> (f32, f32) {
    let d = h[6] * x + h[7] * y + h[8];
    let d = if d.abs() < 1e-9 { 1e-9 } else { d };
    (
        (h[0] * x + h[1] * y + h[2]) / d,
        (h[3] * x + h[4] * y + h[5]) / d,
    )
}

/// Bilinear RGB sample; out-of-bounds reads clamp, which the padded input
/// already makes rare.
fn sample(img: &image::RgbImage, x: f32, y: f32, c: usize) -> u8 {
    let (w, h) = (img.width() as i64, img.height() as i64);
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let cl = |v: i64, m: i64| v.clamp(0, m - 1) as u32;
    let px = |xx: i64, yy: i64| {
        let p = img.get_pixel(cl(xx, w), cl(yy, h));
        p.0[c] as f32
    };
    let top = px(x0, y0) * (1.0 - fx) + px(x0 + 1, y0) * fx;
    let bot = px(x0, y0 + 1) * (1.0 - fx) + px(x0 + 1, y0 + 1) * fx;
    (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8
}

// ---------------------------------------------------------------------------
// Tests: the geometry has to be right before the models are trusted.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.6
    }

    #[test]
    fn a_min_area_rect_of_an_axis_aligned_cloud_is_the_cloud() {
        let mut pts = Vec::new();
        for x in 10..=50 {
            for y in 20..=40 {
                pts.push((x as f32, y as f32));
            }
        }
        let quad = min_area_rect(&convex_hull(&pts));
        let (x0, y0, x1, y1) = quad.bounds();
        assert!(close(x0, 10.0) && close(y0, 20.0) && close(x1, 50.0) && close(y1, 40.0));
    }

    #[test]
    fn a_rotated_block_gets_a_rotated_rect() {
        // A 40×20 block rotated 30°: the min-area rect must be ~40×20, not
        // the 45×45 axis-aligned bounds.
        let mut pts = Vec::new();
        let (cx, cy, ang) = (100.0f32, 100.0f32, 30.0f32.to_radians());
        for i in 0..40 {
            for j in 0..20 {
                let (x, y) = (i as f32 - 20.0, j as f32 - 10.0);
                pts.push((cx + x * ang.cos() - y * ang.sin(), cy + x * ang.sin() + y * ang.cos()));
            }
        }
        let quad = min_area_rect(&convex_hull(&pts));
        // The rect's own diagonal — corner to corner, not the axis-aligned
        // bounds', which a rotation inflates — stays √(40²+20²).
        let p = quad.pts();
        let dist = |a: (f32, f32), b: (f32, f32)| {
            ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
        };
        let diag = dist(p[0], p[2]).max(dist(p[1], p[3]));
        let want = (40.0f32.powi(2) + 20.0f32.powi(2)).sqrt();
        // The grid-quantized hull's stair steps shave the corners a little
        // (the earlier probe fits 39×19), hence the generous tolerance.
        assert!((diag - want).abs() < 1.5, "diagonal {diag} vs {want}");
    }

    #[test]
    fn unclip_grows_a_rect_by_the_ratio() {
        let quad = Quad::new([(10.0, 10.0), (50.0, 10.0), (50.0, 30.0), (10.0, 30.0)]);
        let big = unclip(&quad, 1.6);
        let (x0, y0, x1, y1) = big.bounds();
        // d = area·ratio/perimeter = 800·1.6/120 ≈ 10.7 on every side.
        assert!(close(x0, 10.0 - 10.7) && close(x1, 50.0 + 10.7), "{x0} {x1}");
        assert!(close(y0, 10.0 - 10.7) && close(y1, 30.0 + 10.7), "{y0} {y1}");
    }

    #[test]
    fn a_warped_upright_quad_returns_the_upright_pixels() {
        let mut img = image::RgbImage::new(40, 20);
        for y in 0..20 {
            for x in 0..40 {
                let v = if x < 20 { 255 } else { 0 };
                img.put_pixel(x, y, image::Rgb([v, v, v]));
            }
        }
        let quad = Quad::new([(5.0, 5.0), (35.0, 5.0), (35.0, 15.0), (5.0, 15.0)]);
        let warped = warp_quad(&img, &quad, 30, 10);
        // Left half white, right half black — the warp is the identity map
        // onto the sub-rectangle.
        let left = warped[5 * 30 * 3];
        let right = warped[5 * 30 * 3 + 29 * 3];
        assert!(left > 200, "left pixel {left}");
        assert!(right < 60, "right pixel {right}");
    }

    #[test]
    fn a_tilted_line_warps_upright_whatever_order_the_corners_came_in() {
        // A line slanting down to the right, its corners handed over in the
        // order that sorts the bottom-left corner first — the input the
        // lexicographic order_corners turned into a bowtie and shredded.
        let mut img = image::RgbImage::new(40, 20);
        for y in 0..20 {
            for x in 0..40 {
                let v = if x < 20 { 255 } else { 0 };
                img.put_pixel(x, y, image::Rgb([v, v, v]));
            }
        }
        let lt = (6.0, 4.0);
        let rt = (34.0, 6.0);
        let rb = (33.0, 16.0);
        let lb = (5.0, 14.0);
        for order in [
            [lt, rt, rb, lb],
            [lb, lt, rt, rb],
            [rb, lb, lt, rt],
            [rt, rb, lb, lt],
            [lb, rb, rt, lt],
        ] {
            let warped = warp_quad(&img, &Quad::new(order), 30, 10);
            let at = |x: usize, y: usize| warped[(y * 30 + x) * 3];
            assert!(at(2, 5) > 200, "top-left white, order {order:?}");
            assert!(at(2, 9) > 200, "bottom-left white, order {order:?}");
            assert!(at(27, 5) < 60, "top-right black, order {order:?}");
            assert!(at(27, 9) < 60, "bottom-right black, order {order:?}");
        }
    }

    #[test]
    fn ctc_ordering_sorts_rows_then_columns() {
        let mut lines = vec![
            Line { text: "right".into(), score: 1.0, x0: 100.0, y0: 10.0, x1: 140.0, y1: 20.0 },
            Line { text: "left".into(), score: 1.0, x0: 10.0, y0: 12.0, x1: 50.0, y1: 22.0 },
            Line { text: "second row".into(), score: 1.0, x0: 10.0, y0: 40.0, x1: 80.0, y1: 50.0 },
        ];
        order_lines(&mut lines);
        assert_eq!(lines[0].text, "left");
        assert_eq!(lines[1].text, "right");
        assert_eq!(lines[2].text, "second row");
    }
}

