// Count how many real-note chunks end mid-waveform (the click condition):
// |sample| > 300 within the final 10 ms of the engine output.
use std::sync::Arc;
fn main() {
    let path = std::env::args().nth(1).expect("note json path");
    let idx: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let limit: usize = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(120);
    let raw = std::fs::read_to_string(&path).unwrap();
    let notes: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let note = notes.as_array().unwrap().iter()
        .filter(|n| n["deletedAt"].is_null()).nth(idx).expect("note");
    let text = note["text"].as_str().unwrap();
    let engine: Arc<dyn sn_tts::Engine> =
        Arc::from(sn_tts::manager::build_engine("kokoro", "af_heart", 0.75, "").expect("engine"));
    let plan = sn_tts::plan::SpeechPlan::build(text, 0);
    let mut clicks = 0; let mut checked = 0;
    for (k, c) in plan.chunks.iter().take(limit).enumerate() {
        let syn = match engine.synthesize(&c.text) { Ok(s) => s, Err(_) => continue };
        checked += 1;
        let n = syn.samples.len();
        let tail = &syn.samples[n.saturating_sub(240)..]; // last 10 ms at 24 kHz
        let peak = tail.iter().map(|x| x.abs()).max().unwrap_or(0);
        if peak > 300 {
            clicks += 1;
            let tail_chars: String = c.text.chars().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect();
            println!("CLICK chunk {:4} tail-peak {:5} tail {:?}", k, peak, tail_chars);
        }
    }
    println!("---- {} of {} chunks end mid-waveform (click condition)", clicks, checked);
}
