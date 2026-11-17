// Dump the last 60 ms envelope (1 ms steps) of a specific chunk's engine output.
use std::sync::Arc;
fn main() {
    let path = std::env::args().nth(1).expect("note json path");
    let idx: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let chunk: usize = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(19);
    let raw = std::fs::read_to_string(&path).unwrap();
    let notes: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let note = notes.as_array().unwrap().iter()
        .filter(|n| n["deletedAt"].is_null()).nth(idx).expect("note");
    let text = note["text"].as_str().unwrap();
    let engine: Arc<dyn sn_tts::Engine> =
        Arc::from(sn_tts::manager::build_engine("kokoro", "af_heart", 0.75, "").expect("engine"));
    let plan = sn_tts::plan::SpeechPlan::build(text, 0);
    let syn = engine.synthesize(&plan.chunks[chunk].text).expect("synth");
    let n = syn.samples.len();
    println!("chunk {} ({} chars): audio {:.2}s, last-voice {} ms before end, peak {}",
        chunk, plan.chunks[chunk].text.chars().count(),
        syn.audio_time().as_secs_f64(),
        match syn.samples.iter().rposition(|x| x.abs() > 500) { Some(l) => ((n-1-l) as f64 / 24.0) as i64, None => -1 },
        syn.samples.iter().map(|x| x.abs()).max().unwrap());
    // decay shape: 50ms-envelope of the final 1s
    let env: Vec<i16> = syn.samples[n.saturating_sub(24000)..].chunks(1200).map(|w| w.iter().map(|x| x.abs()).max().unwrap()).collect();
    println!("last 1s (50ms steps): {:?}", env);
    // also the phoneme stream end
    let ipa = sn_tts::phonemes::phonemize(&plan.chunks[chunk].text, "en-us").unwrap();
    let tail_ipa: String = ipa.chars().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect();
    println!("phoneme tail: {:?}", tail_ipa);
}
