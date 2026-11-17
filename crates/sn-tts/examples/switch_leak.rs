//! The Batch 6 switch gate's leak leg: cycle `ModelManager::resident` across
//! engines N times and print RSS before and after, so "switching engines
//! mid-session does not leak" is a measured claim rather than a vibe.
//!
//! Run: `cargo run --release -p sn-tts --example switch_leak -- [cycles]`

use sn_tts::manager::ModelManager;

/// `/proc/self/status` VmRSS in bytes.
fn rss_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .expect("VmRSS is a number");
            return kb * 1024;
        }
    }
    panic!("no VmRSS line in /proc/self/status");
}

fn speak_bytes(name: &str, engine: &dyn sn_tts::Engine, text: &str) -> usize {
    engine
        .synthesize(text)
        .map(|a| a.samples.len())
        .unwrap_or_else(|e| panic!("{name} could not synthesise: {e}"))
}

fn main() {
    let cycles: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(10);
    let text = "The cannula was resited and the infusion rate was recalculated.";

    // A cold cycle first: the runtime's own pools and the first ORT sessions
    // allocate memory that is never returned (the allocator's high-water
    // mark). The leak question is what happens AFTER the machine is warm, so
    // the baseline is taken once every engine has run once.
    let mut manager = ModelManager::new();
    //  the original passes `e` bare; `ModelManager::resident` (original
    // manager.rs) returns `Arc<dyn Engine>`, so the sink needs an explicit
    // unsize deref — applied at all three call sites below.
    for (engine, voice) in [
        ("kokoro", "am_eric"),
        ("supertonic", "F3"),
        ("espeak", "en-gb"),
    ] {
        let e = manager.resident(engine, voice, 1.0, "").expect("warm-up load");
        speak_bytes(engine, &*e, text);
    }
    manager.drop_resident();

    let before = rss_bytes();
    for cycle in 0..cycles {
        for (engine, voice) in [
            ("kokoro", "am_eric"),
            ("supertonic", "F3"),
            ("espeak", "en-gb"),
        ] {
            let e = manager.resident(engine, voice, 1.0, "").expect("load");
            let samples = speak_bytes(engine, &*e, text);
            assert!(samples > 0, "{engine} produced nothing on cycle {cycle}");
        }
    }
    manager.drop_resident();

    // A second warm pass measures the *per-switch* slope: pass 1 includes
    // any allocator warm-up this engine order needs; pass 2 is the steady
    // state. A per-switch leak shows up as pass-2 growth of cycles×leak.
    let mid = rss_bytes();
    for cycle in 0..cycles {
        for (engine, voice) in [
            ("kokoro", "am_eric"),
            ("supertonic", "F3"),
            ("espeak", "en-gb"),
        ] {
            let e = manager.resident(engine, voice, 1.0, "").expect("load");
            let samples = speak_bytes(engine, &*e, text);
            assert!(samples > 0, "{engine} produced nothing on pass-2 cycle {cycle}");
        }
    }
    manager.drop_resident();
    let after = rss_bytes();

    println!(
        "cycles: {cycles}, pass 1: {:+} MB, pass 2: {:+} MB (per-switch {} KB)",
        mid.saturating_sub(before) as f64 / 1_000_000.0,
        after.saturating_sub(mid) as f64 / 1_000_000.0,
        after.saturating_sub(mid) / (cycles as u64 * 4),
    );
}
