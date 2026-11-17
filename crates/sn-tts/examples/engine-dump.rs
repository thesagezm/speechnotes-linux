//! Engine probe: synthesise sentences with one engine and write each result
//! as a WAV, so the audible quality loop can listen to exactly what the
//! pipeline would play — chunk by chunk, with the leading/trailing silence
//! and any trim the engine applied visible in the file.
//!
//! ```text
//! cargo run --release -p sn-tts --example engine-dump -- \
//!     --engine supertonic --voice M4 --rate 1.1 --out /tmp/tts out.wav=Text to speak...
//! ```
//!
//! Each argument without a `--` prefix is `NAME.wav=TEXT`; the WAV is the
//! whole synthesis of that text as one plan-shaped pass (sentence chunks
//! only — the point is per-chunk listening).

use std::sync::Arc;

use sn_tts::manager;

fn main() {
    let mut engine_name = String::from("supertonic");
    let mut voice = String::new();
    let mut rate = 1.0f64;
    let mut out_dir = std::env::temp_dir();
    let mut jobs: Vec<(String, String)> = Vec::new();
    let mut args = std::env::args().skip(1);
    let mut file_job: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--engine" => engine_name = args.next().unwrap_or_default(),
            "--voice" => voice = args.next().unwrap_or_default(),
            "--rate" => rate = args.next().and_then(|v| v.parse().ok()).unwrap_or(1.0),
            "--out" => out_dir = args.next().map(std::path::PathBuf::from).unwrap_or(out_dir),
            "--file" => file_job = args.next(),
            other => {
                if let Some((name, text)) = other.split_once('=') {
                    jobs.push((name.to_string(), text.to_string()));
                }
            }
        }
    }
    if let Some(path) = file_job {
        let text = std::fs::read_to_string(&path).expect("read text file");
        jobs.push(("file".into(), text));
    }
    if jobs.is_empty() {
        eprintln!("usage: engine-dump [--engine E] [--voice V] [--rate R] [--out DIR] name.wav=TEXT ...");
        std::process::exit(2);
    }
    let engine: Arc<dyn sn_tts::Engine> =
        match manager::build_engine(&engine_name, &voice, rate, "") {
            Ok(e) => Arc::from(e),
            Err(e) => {
                eprintln!("engine: {e}");
                std::process::exit(1);
            }
        };
    for (name, text) in &jobs {
        // One synthesis per sentence-shaped chunk, like the plan makes.
        let cleaned = sn_format::sanitizer::clean(text);
        let raw = sn_format::chunker::plan_chunks(&cleaned, 300, 0);
        println!("{name} RAW chunker: {} chunk(s)", raw.len());
        for c in &raw {
            println!("    raw [{}..{}] {:?}", c.start, c.end(), c.text);
        }
        let plan = sn_tts::plan::SpeechPlan::build(text, 0);
        println!("{name}: {} chunk(s)", plan.len());
        for c in &plan.chunks {
            println!("    chunk {} [{}..{}] {:?}", c.index, c.start, c.end(), c.text);
        }
        for chunk in &plan.chunks {
            let syn = engine.synthesize(&chunk.text).expect("synth");
            let path = out_dir.join(format!(
                "{name}-{:02}.wav",
                chunk.index
            ));
            std::fs::write(
                &path,
                sn_format::wav::to_wav_bytes(syn.sample_rate, &syn.samples),
            )
            .expect("write wav");
            println!(
                "  chunk {} [{}..{}] {} chars -> {} ({} ms @ {} Hz)",
                chunk.index,
                chunk.start,
                chunk.end(),
                chunk.text.chars().count(),
                path.display(),
                (syn.audio_time().as_secs_f64() * 1000.0) as u64,
                syn.sample_rate
            );
        }
    }
}
