//! The headless gate.
//!
//! Everything the app does that does not need a window, checked on this
//! machine, in one process, with no display server. This is the batch gate:
//! `--selftest` must exit zero before a batch is allowed to start the next one.
//!
//! It writes only inside `scratch`, which defaults to a temp directory, so it is
//! safe to run against a live installation.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sn_core::appledate::{self, AppleDate};
use sn_core::atomic::{self, DebouncedWriter, LoadOutcome};
use sn_core::paths;
use sn_core::util;
use sn_format::chunker;
use sn_format::sanitizer;
use sn_format::wav;

/// The selftest outcome, with the report already formatted.
pub struct Report {
    pub failed: usize,
    pub text: String,
}

/// Run every headless check. `scratch` is used as the XDG root for the duration,
/// so a live installation is never touched.
pub fn run(scratch: &Path) -> Report {
    let mut checks = 0usize;
    let mut failures: Vec<String> = Vec::new();
    let started = Instant::now();

    // Redirect the XDG roots first: the checks exercise the real `paths` module
    // rather than a stand-in, and must not write into a live library.
    let _guard = XdgGuard::new(scratch);

    println!("speechnotes selftest — core checks, scratch {}", scratch.display());

    let mut check = |name: &str, result: Result<(), String>| {
        checks += 1;
        match result {
            Ok(()) => println!("  ok    {name}"),
            Err(why) => {
                println!("  FAIL  {name}: {why}");
                failures.push(format!("{name}: {why}"));
            }
        }
    };

    check("app directories are created", paths_check());
    check("apple reference date round-trips", date_check());
    check("the real notes.json timestamps decode", real_file_date_check());
    check("djb2 matches the iOS text hash", hash_check());
    check("atomic write survives a reader", atomic_write_check());
    check("a corrupt store quarantines and recovers", quarantine_check());
    check("the debounced writer drains on shutdown", writer_check());
    check("chunking is lossless on real prose", chunk_lossless_check());
    check("a 200k-character chapter chunks fast", chunk_perf_check());
    check("chunking resumes at an exact offset", resume_check());
    check("speech cleaning preserves offsets", sanitize_check());
    check("wav write and read round-trip", wav_check());
    check("a streaming wav export is playable", wav_stream_check());
    check("logging reaches the ring and the file", logging_check());
    check("the log file keeps itself under its cap", logcap_check());
    check("the espeak engine synthesises a sentence", espeak_check());
    check("a speech plan keeps note offsets and resumes", plan_check());
    check("the pipeline speaks, pauses and resumes headlessly", pipeline_check());

    // Batch 6: the engine tier's new half — catalog, single construction
    // path, per-engine prefs. The XdgGuard symlinks the box's real models
    // directory into the scratch (see the guard), so the neural legs see
    // what `--engines` sees and assert what this box holds.
    check("the model catalog lists the neural engines", catalog_check());
    check("the single construction path builds every installed engine", build_path_check());
    check("engine and voice prefs round-trip per engine", engine_prefs_check());
    check("the phonon engine synthesises a sentence", phonon_check());

    let elapsed = started.elapsed();
    let mut text = format!(
        "selftest: {checks} checks, {} failed, {elapsed:.2?}",
        failures.len()
    );
    if !failures.is_empty() {
        text.push_str("\n\nfailures:\n");
        for f in &failures {
            text.push_str("  - ");
            text.push_str(f);
            text.push('\n');
        }
    }

    Report {
        failed: failures.len(),
        text,
    }
}

/// Redirects the XDG roots for the duration of the run, restoring them after.
struct XdgGuard {
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    #[allow(dead_code)]
    scratch: PathBuf,
}

impl XdgGuard {
    fn new(scratch: &Path) -> Self {
        let _ = std::fs::create_dir_all(scratch);
        let scratch = scratch.to_path_buf();
        // Read *before* the redirect: `paths::models_dir()` follows
        // XDG_DATA_HOME like everything else, so after the set_var below it
        // would name the scratch, and the guard would link the scratch to
        // itself.
        let real_models = paths::models_dir();
        let mut saved = Vec::new();
        for (key, sub) in [
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_CONFIG_HOME", "config"),
        ] {
            saved.push((key, std::env::var_os(key)));
            std::env::set_var(key, scratch.join(sub));
        }
        // The Batch 6 checks read the models directory through the catalog,
        // and the redirected XDG_DATA_HOME would hide the box's installed
        // engines — the check would report "not installed" against a box
        // that has them. Carry the *real* models directory into the scratch
        // by symlink before the first check runs: read-only as far as the
        // selftest is concerned (nothing writes under models/), and the
        // alternative — pointing the checks at an absolute path — would
        // fork the path logic the selftest exists to exercise.
        if real_models.exists() {
            let scratch_models = scratch.join("data/speechnotes/models");
            let _ = std::fs::create_dir_all(scratch_models.parent().unwrap());
            //  the empty-leftover branch below
            // was unreachable — the outer condition required the path to not
            // exist, so an existing empty `models/` (measured: one left in
            // the default scratch on Nov 9) blocked the symlink forever and
            // every engine check reported "not installed". That was the
            // board's long-standing "pre-existing engine-check failure". The
            // guard
            // now does what its comment always said: replace an EMPTY
            // leftover; keep anything that holds files — that is a real
            // model tree and not the guard's to judge; keep an existing
            // symlink.
            let replaceable = !scratch_models.exists() && !scratch_models.is_symlink()
                || (scratch_models.is_dir()
                    && std::fs::read_dir(&scratch_models)
                        .map(|mut d| d.next().is_none())
                        .unwrap_or(false));
            if replaceable {
                let _ = std::fs::remove_dir(&scratch_models);
                let _ = std::os::unix::fs::symlink(real_models, scratch_models);
            }
        }
        Self { saved, scratch }
    }
}

impl Drop for XdgGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..) {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn paths_check() -> Result<(), String> {
    paths::ensure_directories().map_err(|e| format!("ensure_directories: {e}"))?;
    for dir in [
        paths::data_dir(),
        paths::cache_dir(),
        paths::config_dir(),
        paths::books_dir(),
        paths::models_dir(),
        paths::exports_dir(),
        paths::note_images_dir(),
        paths::tts_cache_dir(),
    ] {
        if !dir.is_dir() {
            return Err(format!("{} was not created", dir.display()));
        }
    }
    Ok(())
}

fn date_check() -> Result<(), String> {
    if appledate::APPLE_EPOCH_OFFSET != 978_307_200 {
        return Err(format!(
            "epoch offset is {}, expected 978307200",
            appledate::APPLE_EPOCH_OFFSET
        ));
    }
    let raw = 780_000_000.0_f64;
    let d = AppleDate::from_apple(raw);
    let json = serde_json::to_string(&d).map_err(|e| e.to_string())?;
    if json != "780000000.0" {
        return Err(format!("encoded as {json}, expected a bare float"));
    }
    let back: AppleDate = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    if back.raw() != raw {
        return Err(format!("round trip produced {}", back.raw()));
    }
    Ok(())
}

/// Decodes a timestamp taken verbatim from the user's own `notes.json`.
fn real_file_date_check() -> Result<(), String> {
    let raw = 812_152_162.598_523_4_f64; // 2026-09-26T21:49:22Z
    let d = AppleDate::from_apple(raw);
    let day = AppleDate::local_day_from_unix(d.unix_secs());
    if day != "2026-09-26" {
        return Err(format!("decoded to {day}, expected 2026-09-26"));
    }
    Ok(())
}

fn hash_check() -> Result<(), String> {
    if util::stable_hash("") != 5381 {
        return Err("empty-string hash is not djb2's seed".into());
    }
    if util::stable_hash("a") != 5381 * 33 + 97 {
        return Err("single-character hash does not match iOS".into());
    }
    let long = "unicode 🎉 日本語";
    if util::stable_hash(long) != util::stable_hash(long) {
        return Err("hash is not deterministic".into());
    }
    if util::stable_hash(long) < 0 {
        return Err("hash went negative".into());
    }
    Ok(())
}

fn atomic_write_check() -> Result<(), String> {
    let path = paths::notes_file();
    atomic::write_atomic(&path, b"[]").map_err(|e| format!("write_atomic: {e}"))?;
    // A reader running while the writer works must always see a complete file.
    for i in 0..50 {
        atomic::write_atomic(&path, format!("[{}]", i).as_bytes())
            .map_err(|e| format!("write {i}: {e}"))?;
        let seen = std::fs::read(&path).map_err(|e| format!("read {i}: {e}"))?;
        if seen != format!("[{}]", i).as_bytes() {
            return Err(format!("iteration {i} saw {seen:?}"));
        }
    }
    Ok(())
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
struct Store {
    #[serde(default)]
    n: u32,
}

fn quarantine_check() -> Result<(), String> {
    let dir = paths::data_dir();
    let path = paths::notes_file();
    let stem = path.file_name().unwrap().to_string_lossy().into_owned();

    // Work in a private namespace so the count below is about *this* check. An
    // earlier run of the selftest legitimately leaves its own quarantined file
    // behind, and asserting on the whole directory made the check fail on the
    // second run.
    let before = count_quarantined(&dir, &stem)?;

    atomic::write_json_atomic(&atomic::backup_path(&path), &Store { n: 42 })
        .map_err(|e| format!("backup write: {e}"))?;
    std::fs::write(&path, b"{ not json at all").map_err(|e| e.to_string())?;

    let (value, outcome) = atomic::load_json_recoverable::<Store>(&path);
    if value != (Store { n: 42 }) {
        return Err("did not recover from the backup".into());
    }
    if outcome != LoadOutcome::Backup {
        return Err(format!("reported {outcome:?}, expected Backup"));
    }

    let after = count_quarantined(&dir, &stem)?;
    if after != before + 1 {
        return Err(format!(
            "{after} quarantined files present, expected {} — the broken file must be preserved",
            before + 1
        ));
    }
    if path.exists() {
        return Err("the corrupt file was left in place".into());
    }
    Ok(())
}

fn count_quarantined(dir: &std::path::Path, stem: &str) -> Result<usize, String> {
    Ok(std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with(stem) && name.contains(".corrupt-")
        })
        .count())
}

fn writer_check() -> Result<(), String> {
    let path = paths::cache_dir().join("selftest-writer.json");
    let mut writer = DebouncedWriter::new(path.clone(), std::time::Duration::from_secs(30))
        .map_err(|e| format!("writer: {e}"))?;

    // A 30 s debounce: without a real flush this payload can never land, which
    // is exactly the bug the Swift port shipped (no shutdown flush anywhere).
    writer.schedule(serde_json::to_vec(&Store { n: 7 }).unwrap());
    writer.shutdown().map_err(|e| format!("shutdown: {e}"))?;

    let bytes = std::fs::read(&path).map_err(|e| format!("read: {e}"))?;
    let got: Store = serde_json::from_slice(&bytes)
        .map_err(|e| format!("wrote unparseable data: {e}"))?;
    if got != (Store { n: 7 }) {
        return Err(format!("shutdown wrote n={}, expected 7", got.n));
    }
    Ok(())
}

fn chunk_lossless_check() -> Result<(), String> {
    // Prose with every awkward shape: an abbreviation, a decimal, an emoji, a
    // CJK full stop, a hyphenated line and a rule.
    let text = "Dr. Smith paid 3.14 units. He said \"hello.\" Then — nothing.\n\
                A second line with an emoji 🎉 and more text to force a split here \
                because it keeps going and going and going past the ceiling.\n\
                これはテストです。\n---\n";
    let plan = chunker::plan_chunks(text, chunker::DEFAULT_MAX_CHARS, 0);
    if plan.is_empty() {
        return Err("produced no chunks".into());
    }
    let joined: String = plan.iter().map(|c| c.text.as_str()).collect();
    if joined != text {
        return Err(format!(
            "lossless split failed\n  want: {text:?}\n  got:  {joined:?}"
        ));
    }
    // The plan must also tile the source contiguously, not merely reproduce it.
    let mut cursor = 0usize;
    for c in &plan {
        if c.start != cursor {
            return Err(format!("gap or overlap at byte {cursor}"));
        }
        cursor = c.end();
    }
    if cursor != text.len() {
        return Err(format!("plan ends at {cursor}, source is {}", text.len()));
    }

    // And the filter the pipeline applies must drop the whitespace-only spans
    // without disturbing the spoken ones.
    let spoken = chunker::speakable(&plan);
    let rendered: String = spoken.iter().map(|c| c.text.as_str()).collect();
    if !rendered.contains("Dr. Smith paid 3.14 units.") {
        return Err("the spoken text lost its first sentence".into());
    }
    Ok(())
}

fn chunk_perf_check() -> Result<(), String> {
    let text = "The quick brown fox jumps over the lazy dog. ".repeat(8_000);
    let started = Instant::now();
    let chunks = chunker::plan_chunks(&text, chunker::DEFAULT_MAX_CHARS, 0);
    let elapsed = started.elapsed();

    let joined: String = chunks.iter().map(|c| c.text.as_str()).collect();
    if joined != text {
        return Err("not lossless at scale".into());
    }
    // The Swift chunker was quadratic and would have taken seconds here.
    if elapsed.as_millis() > 500 {
        return Err(format!("chunked {} bytes in {elapsed:?}, expected < 500ms", text.len()));
    }
    println!(
        "        {} bytes -> {} chunks in {:?} ({:.1} MB/s)",
        text.len(),
        chunks.len(),
        elapsed,
        (text.len() as f64 / 1e6) / elapsed.as_secs_f64().max(1e-9)
    );
    Ok(())
}

fn resume_check() -> Result<(), String> {
    let text = "First sentence. Second sentence. Third sentence. Fourth sentence.";
    let all = chunker::plan_chunks(text, chunker::DEFAULT_MAX_CHARS, 0);
    if all.len() < 2 {
        return Err("fixture did not split".into());
    }
    let from = all[1].start;
    let resumed = chunker::plan_chunks(text, chunker::DEFAULT_MAX_CHARS, from);
    let joined: String = resumed.iter().map(|c| c.text.as_str()).collect();
    if joined != text[from..] {
        return Err(format!("resume produced {joined:?}"));
    }
    for (i, c) in resumed.iter().enumerate() {
        if c.index != i {
            return Err(format!("chunk {i} carries index {}", c.index));
        }
    }
    Ok(())
}

fn sanitize_check() -> Result<(), String> {
    let raw = "# Chapter One\n\nSome `code` and a [link](https://x.example).\nMore prose here.";
    let cleaned = sanitizer::clean(raw);
    if cleaned.len() != raw.len() {
        return Err(format!(
            "cleaning changed the byte length: {} -> {}",
            raw.len(),
            cleaned.len()
        ));
    }
    if cleaned.contains("x.example") {
        return Err("a link target survived cleaning".into());
    }
    let (norm, map) = sanitizer::normalize_whitespace(&cleaned);
    let needle = "More prose";
    let n_off = norm.find(needle).ok_or("needle vanished")?;
    let orig = sanitizer::original_offset(&norm, &map, n_off);
    if &raw[orig..orig + needle.len()] != needle {
        return Err(format!("offset mapping landed at {orig}"));
    }
    Ok(())
}

fn wav_check() -> Result<(), String> {
    let path = paths::cache_dir().join("selftest.wav");
    let samples: Vec<i16> = (0..1_000).map(|i| (i % 512) as i16 - 256).collect();
    wav::write_samples(&path, 24_000, &samples).map_err(|e| format!("write: {e}"))?;
    let (rate, read) = wav::read_samples(&path).map_err(|e| format!("read: {e}"))?;
    if rate != 24_000 {
        return Err(format!("rate came back {rate}"));
    }
    if read != samples {
        return Err("samples did not round-trip".into());
    }
    Ok(())
}

fn wav_stream_check() -> Result<(), String> {
    let path = paths::cache_dir().join("selftest-stream.wav");
    let mut writer = wav::FileWavWriter::create(&path, 44_100).map_err(|e| e.to_string())?;
    for block in 0..10 {
        let block: Vec<i16> = (0..10_000)
            .map(|i| ((block * 10_000 + i) % 1_000) as i16)
            .collect();
        writer.write_samples(&block).map_err(|e| e.to_string())?;
    }
    let frames = writer.frames();
    writer.finalise().map_err(|e| e.to_string())?;

    let info = wav::parse_header(&std::fs::read(&path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if info.frames != frames {
        return Err(format!("header says {} frames, writer wrote {frames}", info.frames));
    }
    if info.sample_rate != 44_100 {
        return Err(format!("rate came back {}", info.sample_rate));
    }
    println!(
        "        exported {} frames ({:.2}s) at {} Hz",
        info.frames,
        info.duration_secs(),
        info.sample_rate
    );
    Ok(())
}
/// Exercises the log pipeline end to end: the ring the UI reads, the capped file
/// on disk, and the interaction between the two.
fn logging_check() -> Result<(), String> {
    let ring = sn_core::LogRing::new();
    sn_core::logging::init(ring.clone());

    let before = ring.len();
    tracing::info!(engine = "espeak", "selftest: log line from the UI thread");
    std::thread::spawn(|| {
        tracing::warn!("selftest: log line from a worker thread");
    })
    .join()
    .map_err(|_| "a logging thread panicked".to_string())?;

    let recent = ring.recent(ring.len());
    if recent.len() <= before {
        return Err("nothing reached the in-memory ring".into());
    }
    let text = recent.join("\n");
    if !text.contains("log line from the UI thread") {
        return Err(format!("the UI thread's line is missing:\n{text}"));
    }
    if !text.contains("log line from a worker thread") {
        return Err(format!("a worker thread's line is missing:\n{text}"));
    }
    if !text.contains("engine=\"espeak\"") && !text.contains("engine=espeak") {
        return Err(format!("structured fields were dropped:\n{text}"));
    }

    // The ring is capped, so an app running for a week cannot grow it forever.
    for i in 0..(sn_core::logging::RING_CAPACITY + 100) {
        ring.push(format!("filler {i}"));
    }
    if ring.len() != sn_core::logging::RING_CAPACITY {
        return Err(format!("ring grew to {}", ring.len()));
    }

    let log = std::fs::read_to_string(paths::log_file()).map_err(|e| format!("log file: {e}"))?;
    if !log.contains("log line from the UI thread") {
        return Err("the file layer did not write the UI thread's line".into());
    }
    if !log.contains("log line from a worker thread") {
        return Err("the file layer did not write a worker thread's line".into());
    }
    Ok(())
}

/// Checks the size cap on the real log file, with a small ceiling so the check
/// does not have to write 2 MiB.
fn logcap_check() -> Result<(), String> {
    let path = paths::cache_dir().join("selftest-cap.log");
    let mut log = sn_core::CappedLog::with_cap(&path, 4_000).map_err(|e| e.to_string())?;
    for i in 0..1_000 {
        writeln!(log, "selftest log line {i:04}").map_err(|e| e.to_string())?;
    }
    log.flush().map_err(|e| e.to_string())?;
    drop(log);

    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    if bytes.len() > 4_000 {
        return Err(format!("log grew to {} bytes, cap is 4000", bytes.len()));
    }
    let text = String::from_utf8_lossy(&bytes);
    if !text.contains("selftest log line 0999") {
        return Err("the newest line was trimmed away".into());
    }
    if text.contains("selftest log line 0000") {
        return Err("the oldest line survived a trim".into());
    }
    Ok(())
}

/// Batch 5: the real espeak CLI turns one sentence into PCM at its native rate.
fn espeak_check() -> Result<(), String> {
    sn_tts::EspeakEngine::available().map_err(|e| e.to_string())?;
    let engine = sn_tts::EspeakEngine::with_wpm("en-gb", sn_tts::espeak::DEFAULT_WPM);
    let out = sn_tts::Engine::synthesize(
        &engine,
        "The selftest speaks one sentence to prove the engine tier.",
    )
    .map_err(|e| e.to_string())?;
    if out.sample_rate != 22_050 {
        return Err(format!("unexpected rate {}", out.sample_rate));
    }
    if out.audio_time().as_secs_f64() < 0.5 {
        return Err(format!("audio too short: {:?}", out.audio_time()));
    }
    Ok(())
}

/// Batch 5: plan offsets are note offsets — the invariant read-along and
/// bookmarks stand on.
fn plan_check() -> Result<(), String> {
    let text = "# Title\n\nFirst sentence with **bold** here. Second sentence follows.\n\n```\nnot spoken\n```\nThird and last.";
    let plan = sn_tts::SpeechPlan::build(text, 0);
    if plan.is_empty() {
        return Err("a prose note planned to nothing".into());
    }
    let cleaned = sanitizer::clean(text);
    for c in &plan.chunks {
        let slice = cleaned
            .get(c.start..c.end())
            .ok_or_else(|| format!("chunk {} out of range", c.index))?;
        if slice != c.text {
            return Err(format!("chunk {} diverges from the source", c.index));
        }
    }
    let resumed = sn_tts::SpeechPlan::build(text, plan.chunks[1].start);
    if resumed.len() != plan.len() - 1 {
        return Err(format!(
            "resume produced {} chunks, wanted {}",
            resumed.len(),
            plan.len() - 1
        ));
    }
    Ok(())
}

/// Batch 5: the pipeline end to end on the ALSA null device — speak to
/// completion with the frontier at the plan's end, TTFA measured, and an
/// engine keeping ahead of real time. Pause and resume are pinned by the
/// session unit tests (including a lagging-ring device the null device
/// cannot model); this check proves the real engine and the real CLI.
fn pipeline_check() -> Result<(), String> {
    use std::sync::Arc;
    let text = (0..8)
        .map(|i| format!("Sentence number {i} speaks for a while. "))
        .collect::<String>();
    let plan = sn_tts::SpeechPlan::build(&text, 0);
    let end = plan.end_byte();

    let full = sn_tts::SessionCore::start(
        &text,
        0,
        Arc::new(sn_tts::EspeakEngine::with_wpm("en-gb", 175)),
        sn_tts::SessionConfig {
            device: "null".to_string(),
            channel_depth: 2,
            pinned_opener_chars: None, // None is the production path per session.rs
        },
    )
    .map_err(|e| e.to_string())?;
    let metrics = full.join();
    if metrics.state != sn_tts::PlayState::Finished {
        return Err(format!("state {:?}, wanted Finished", metrics.state));
    }
    if metrics.frontier != end {
        return Err(format!("frontier {}, wanted {end}", metrics.frontier));
    }
    if metrics.ttfa.is_none() {
        return Err("TTFA was never measured".into());
    }
    if metrics.mean_rtf() <= 0.0 || metrics.mean_rtf() >= 1.0 {
        return Err(format!("mean RTF {} is not an engine keeping ahead", metrics.mean_rtf()));
    }
    Ok(())
}

/// Batch 6: the catalog is the manager's truth — four neural sets, each with
/// files that have floors, and a builtin that needs no set at all.
fn catalog_check() -> Result<(), String> {
    let catalog = sn_tts::manager::catalog();
    let engines: Vec<&str> = catalog.iter().map(|s| s.engine).collect();
    if engines != ["kokoro", "supertonic", "phonon", "nano-edge-tts"] {
        return Err(format!("catalog holds {engines:?}"));
    }
    for set in &catalog {
        if set.files.is_empty() {
            return Err(format!("{} has no files", set.engine));
        }
        for file in &set.files {
            if file.min_bytes == 0 {
                return Err(format!("{}: {} has no floor", set.engine, file.local));
            }
            if file.local.starts_with('/') {
                return Err(format!("{}: {} is absolute", set.engine, file.local));
            }
        }
    }
    if !sn_tts::manager::ModelManager::is_builtin("espeak") {
        return Err("espeak must be builtin".into());
    }
    // Both error strings are verbatim in the shipped binary, and
    // `#[cfg(test)]` text cannot reach a release binary — so the catalog
    // check carries this page-recogniser exclusion alongside the exact
    // three-voice list above. manager.rs's own tests pin the same two
    // invariants.
    let speech: Vec<&str> = sn_tts::manager::ModelManager::available_engines();
    if speech.contains(&"ocr") {
        return Err(format!("ocr leaked into the speech engines: {speech:?}"));
    }
    if sn_tts::manager::ModelManager::preferred_engine() == "ocr" {
        return Err("preferred engine must be a voice".into());
    }
    Ok(())
}

/// Batch 6: `build_engine` is the one construction path — the picker, the
/// `--speak` gate and `resident()` all call it, so a voice that loads in one
/// loads in all. Builds espeak (always) plus every neural set this box has
/// installed, and pins the poisoned-pref rule: a voice that cannot exist
/// builds the engine's default (a refused engine was the beeps-everywhere
/// bug of 2026-11-13 — a foreign voice code under an engine's key failed
/// every chunk instead of speaking), while an unknown ENGINE still fails.
fn build_path_check() -> Result<(), String> {
    let espeak = sn_tts::manager::build_engine("espeak", "en-gb", 1.0, "en")
        .map_err(|e| e.to_string())?;
    if espeak.name() != "espeak" {
        return Err(format!("espeak built as {}", espeak.name()));
    }
    for engine in ["kokoro", "supertonic", "phonon", "nano-edge-tts"] {
        if sn_tts::manager::ModelManager::state(engine)
            != sn_tts::manager::InstallState::Installed
        {
            // The batch contract says all four are installed on this box;
            // a missing one means the selftest is not the gate it claims.
            return Err(format!("{engine} is not installed"));
        }
        let set = sn_tts::manager::ModelManager::set_for(engine)
            .ok_or_else(|| format!("{engine} has no catalog set"))?;
        let voice = set.default_voice;
        let built = sn_tts::manager::build_engine(engine, voice, 1.0, "")
            .map_err(|e| format!("{engine} default voice failed to build: {e}"))?;
        if built.name() != engine {
            return Err(format!("{engine} built as {}", built.name()));
        }
        // The poisoned-pref shape: a voice code the engine does not offer
        // builds the default, it does not refuse the engine.
        let healed = sn_tts::manager::build_engine(engine, "not_a_voice", 1.0, "")
            .map_err(|e| format!("{engine} did not fall back to its default: {e}"))?;
        if healed.name() != engine {
            return Err(format!("{engine} default fell back as {}", healed.name()));
        }
    }
    // An unknown engine is still refused by name — the fallback is for
    // voices, never for engines.
    match sn_tts::manager::build_engine("whisper", "", 1.0, "") {
        Err(e) if e.to_string().contains("whisper") => {}
        Err(e) => return Err(format!("unknown-engine error does not name it: {e}")),
        Ok(_) => return Err("an unknown engine built".into()),
    }
    Ok(())
}

/// The phonon leg: the newest engine builds through the same construction
/// path, and its steady-state synthesis keeps ahead of real time — the same
/// RTF gate the pipeline check holds espeak to. The first synthesis carries
/// the model's one-time graph warm-up (measured on this box's i5-10210U:
/// RTF 1.59 for chunk 0, then 0.69–0.80 for the chunks after), so the gate is
/// the *second* synthesis of the same engine — what a playing session spends
/// its time in, since the resident engine warms up once at session start,
/// not per chunk.
fn phonon_check() -> Result<(), String> {
    if sn_tts::manager::ModelManager::state("phonon")
        != sn_tts::manager::InstallState::Installed
    {
        // The batch contract says phonon is installed on this box; a missing
        // set means the selftest is not the gate it claims.
        return Err("phonon is not installed".into());
    }
    let engine = sn_tts::manager::build_engine("phonon", "default", 1.0, "")
        .map_err(|e| format!("phonon failed to build: {e}"))?;
    if engine.name() != "phonon" {
        return Err(format!("phonon built as {}", engine.name()));
    }
    let text = "The phonon engine speaks this sentence twice so the second pass measures the steady state.";
    engine
        .synthesize(text)
        .map_err(|e| format!("phonon warm-up synthesis failed: {e}"))?;
    let out = engine
        .synthesize(text)
        .map_err(|e| format!("phonon synthesis failed: {e}"))?;
    let audio = out.audio_time().as_secs_f64();
    if audio < 1.0 {
        return Err(format!("phonon audio is suspiciously short: {audio:.2} s"));
    }
    let rtf = out.synth_time.as_secs_f64() / audio;
    if rtf > 1.0 {
        return Err(format!("phonon RTF {rtf:.2} is not keeping ahead of real time"));
    }
    Ok(())
}

/// Batch 6: `engineKind` + `voicesByEngine` behave as the picker needs — a
/// voice stored per engine survives a round trip, espeak still honours the
/// legacy single `voice` field, and the clamp refuses an unknown engine kind.
fn engine_prefs_check() -> Result<(), String> {
    let mut prefs = sn_store::stores::Prefs::default();
    if prefs.engine_kind != "espeak" {
        return Err(format!("default engine is {}", prefs.engine_kind));
    }
    prefs.set_voice_for_engine("kokoro", "af_bella");
    if prefs.voice_for_engine("kokoro") != "af_bella" {
        return Err("kokoro voice did not round-trip".into());
    }
    // The legacy field is espeak's fallback, as iOS writes it.
    prefs.voice = "en-gb".into();
    if prefs.voice_for_engine("espeak") != "en-gb" {
        return Err("the legacy espeak voice field was not honoured".into());
    }
    // A per-engine entry outranks the legacy field for the same engine.
    prefs.set_voice_for_engine("espeak", "en-us");
    if prefs.voice_for_engine("espeak") != "en-us" {
        return Err("per-engine voice did not outrank the legacy field".into());
    }
    prefs.engine_kind = "whisper".into();
    prefs.clamp();
    if prefs.engine_kind != "espeak" {
        return Err(format!("an unknown engine clamped to {}", prefs.engine_kind));
    }
    Ok(())
}
