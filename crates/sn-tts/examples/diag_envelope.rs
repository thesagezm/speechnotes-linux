//! Per-engine audio diagnostics: duration, and where the speech actually
//! ends. A truncated tail shows as speech ending long before the samples do;
//! a padded tail shows as the reverse.

use sn_tts::manager::build_engine;

const TEXT: &str = "The quick brown fox jumps over the lazy dog. Pack my box with five dozen liquor jugs.";
const LONG: &str = "Speech is the movement of air across a vocal tract. A synthesizer models that movement directly, which is why a generated voice can be steady and repeatable where a recorded one cannot. The engines here differ in how they model it, and in how much computation they spend per sentence.";

fn main() {
    for (engine, text) in [
        ("espeak", TEXT),
        ("kokoro", TEXT),
        ("supertonic", TEXT),
        ("espeak", LONG),
        ("kokoro", LONG),
        ("supertonic", LONG),
    ] {
        let voice = match engine {
            "kokoro" => "af_alloy",
            "supertonic" => "M2",
            _ => "",
        };
        let tag = if text == TEXT { "short" } else { "long " };
        let Ok(e) = build_engine(engine, voice, 1.0, "") else {
            println!("{engine:>11} {tag}: unavailable");
            continue;
        };
        let s = sn_tts::Engine::synthesize(e.as_ref(), text).expect("synth");
        let n = s.samples.len();
        let win = (s.sample_rate as usize / 50).max(1); // 20 ms
        let mut last_speech = 0usize;
        let mut speech_windows = 0;
        let mut total_windows = 0;
        for (i, w) in s.samples.chunks(win).enumerate() {
            let rms = (w.iter().map(|v| (*v as f64 / 32768.0).powi(2)).sum::<f64>()
                / w.len() as f64)
                .sqrt();
            total_windows += 1;
            if rms > 0.005 {
                last_speech = i;
                speech_windows += 1;
            }
        }
        let dur = n as f64 / f64::from(s.sample_rate);
        let speech_end = (last_speech + 1) * win;
        let tail = (n - speech_end) as f64 / f64::from(s.sample_rate);
        let speech_dur = speech_end as f64 / f64::from(s.sample_rate);
        let words = text.split_whitespace().count();
        let silence_pct = 100.0 * (1.0 - speech_windows as f64 / total_windows as f64);
        println!(
            "{engine:>11} {tag}: {dur:>5.2}s total, speech ends {speech_dur:>5.2}s, tail {tail:+.2}s, {words:>3} words => {:>3.0} wpm, {silence_pct:.0}% silence",
            words as f64 / (speech_dur / 60.0),
        );
    }
}
