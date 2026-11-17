//! The engine tier: one trait every voice engine implements.
//!
//! An engine is *configured* at construction (voice, speed) and then only ever
//! asked to turn one chunk of text into PCM. Keeping the configuration out of
//! the call means the pipeline holds an `Arc<dyn Engine>` and never needs to
//! know which engine it is feeding — which is what lets Batch 6 add neural
//! engines without touching the pipeline, and what the single-resident-model
//! rule will later hang off.

use std::time::Duration;

use crate::TtsError;

#[cfg(test)]
mod edge_fade_tests {
    use super::*;

    /// A chunk the model stopped mid-waveform: the fade must bring the very
    /// end to silence without touching the body of the audio.
    #[test]
    fn a_mid_waveform_tail_fades_to_silence() {
        let mut syn = Synthesis {
            sample_rate: 24_000,
            samples: vec![0_i16; 2400]
                .into_iter()
                .chain((0..480).map(|i| (i * 40) as i16)) // rises to ~19000
                .chain(vec![19_000_i16; 96]) // …and stays hot to the end
                .collect(),
            synth_time: Duration::ZERO,
        };
        let body: Vec<i16> = syn.samples[..2400].to_vec();
        let pre_peak = syn.samples[2700..2880].iter().map(|x| x.abs()).max().unwrap();
        syn.fade_edges();
        assert_eq!(&syn.samples[..2400], &body[..], "mid-chunk audio was touched");
        // The raised cosine reaches ~0 at the final sample; one LSB of
        // rounding is silence in every practical sense.
        assert!(
            syn.samples.last().unwrap().abs() <= 2,
            "the last sample is not silence: {}",
            syn.samples.last().unwrap()
        );
        let post_peak = syn.samples[2700..2880].iter().map(|x| x.abs()).max().unwrap();
        assert!(
            (post_peak as f64) < (pre_peak as f64) * 0.75,
            "the fade-out did not soften the tail: {pre_peak} -> {post_peak}"
        );
        // The hot tail earned the LONG ramp: 10 ms from the end the sample
        // must be well into the fade — a 12 ms ramp would not have reached
        // this far back at all.
        let ten_ms_from_end = syn.samples.len() - 240;
        assert!(
            syn.samples[ten_ms_from_end].abs() < pre_peak / 4,
            "the long ramp did not engage: {}",
            syn.samples[ten_ms_from_end].abs()
        );
    }

    /// A chunk that already ends in silence — the normal sentence ending —
    /// must come out bit-identical: the fades multiply zeros.
    #[test]
    fn a_silent_edge_is_untouched() {
        let mut samples: Vec<i16> = vec![0_i16; 1000];
        samples.extend((0..1000).map(|i| ((i * 37) % 7000) as i16));
        samples.extend(vec![0_i16; 1000]);
        let before = samples.clone();
        let mut syn = Synthesis { sample_rate: 24_000, samples, synth_time: Duration::ZERO };
        syn.fade_edges();
        assert_eq!(syn.samples, before, "a silent edge was altered");
    }
}

/// One chunk of synthesised speech.
#[derive(Debug, Clone)]
pub struct Synthesis {
    /// Rate of [`Self::samples`]. The pipeline resamples to the device rate;
    /// engines are free to emit whatever they natively produce.
    pub sample_rate: u32,
    /// Mono samples. `f32` engines convert at the boundary; inside sn-tts the
    /// currency is `i16`, the same as the sink's.
    pub samples: Vec<i16>,
    /// How long the synthesis took. The numerator of the chunk's RTF.
    pub synth_time: Duration,
}

impl Synthesis {
    /// Duration of the audio produced.
    pub fn audio_time(&self) -> Duration {
        if self.sample_rate == 0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(self.samples.len() as f64 / self.sample_rate as f64)
    }

    /// Fade the chunk's first and last few milliseconds to silence.
    ///
    /// Every engine calls this on its output, because a chunk that the model
    /// stops mid-waveform clicks: the chunker ends pieces at word boundaries
    /// (a 300-character run-on cut, a heading, a citation fragment), those
    /// streams carry no sentence-final pause, and the model's last sample
    /// lands at full amplitude — which the per-chunk resampler then truncates
    /// within one filter window, a hard cutoff a listener hears as a tick at
    /// the end of the chunk. A short fade turns the stop into a soft one.
    /// Chunks that already end in silence are untouched: the fade multiplies
    /// zeros with zeros. Mid-chunk audio is never touched.
    pub fn fade_edges(&mut self) {
        const FADE_IN_MS: u32 = 6;
        const FADE_OUT_MS: u32 = 12;
        const FADE_OUT_HOT_MS: u32 = 40;
        /// A tail still this loud in its last 20 ms is a model that stopped
        /// mid-phone despite the pause token — it gets the long ramp.
        const HOT_TAIL: i16 = 1_200;
        let n = self.samples.len();
        if n < 16 {
            return;
        }
        let sr = self.sample_rate.max(1) as usize;
        // Half the chunk is the cap: on a tiny chunk the two fades must not
        // overlap, and a fade longer than its half would fight itself.
        let fade_in = (sr * FADE_IN_MS as usize / 1000).min(n / 2).max(1);
        let tail_probe = (sr * 20 / 1000).min(n / 2).max(1);
        let hot = self.samples[n - tail_probe..]
            .iter()
            .map(|s| s.abs())
            .max()
            .unwrap_or(0)
            > HOT_TAIL;
        let fade_out_ms = if hot { FADE_OUT_HOT_MS } else { FADE_OUT_MS };
        let fade_out = (sr * fade_out_ms as usize / 1000).min(n / 2).max(1);
        for i in 0..fade_in {
            // Raised cosine: the slope is zero at both ends, so the ramp
            // itself adds no step where the linear one would.
            let g = 0.5 * (1.0 - (std::f64::consts::PI * i as f64 / fade_in as f64).cos());
            self.samples[i] = (self.samples[i] as f64 * g).round() as i16;
        }
        for i in 0..fade_out {
            let g = 0.5 * (1.0 + (std::f64::consts::PI * i as f64 / fade_out as f64).cos());
            let idx = n - fade_out + i;
            self.samples[idx] = (self.samples[idx] as f64 * g).round() as i16;
        }
    }
}

/// A voice an engine offers. `code` is what goes back into the engine's
/// configuration; `name` is what a picker shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    pub code: String,
    pub name: String,
    /// The language the voice speaks, as the engine names it — espeak's
    /// voice code (`"en-gb"`), phonon's catalog locale (`"en-us"`), or the
    /// literal `"characters"` group — and `None` for a voice whose
    /// language is not the voice's own business: a single-language bank
    /// (kokoro) or a language-neutral style (supertonic). The
    /// language selector filters with this, through the ONE helper
    /// (`manager::voices_for_language`); `None` voices always show.
    pub locale: Option<String>,
}

/// A text-to-speech engine.
///
/// `Send + Sync` because the pipeline calls it from the synth thread while the
/// UI thread may still hold the `Arc`.
pub trait Engine: Send + Sync {
    /// The engine's stable name — the key prefs store voices under, and the
    /// value `prefs.engineKind` holds.
    fn name(&self) -> &'static str;

    /// The speech rate the engine was built (or last set) at, 1.0 = normal.
    /// The chunk schedule needs it: a faster voice plays the same characters
    /// in less audio, so the same opener *target in seconds* needs more
    /// characters. 1.0 for engines that cannot change it.
    fn rate(&self) -> f64 {
        1.0
    }

    /// Synthesise one chunk. Implementations must be safe to call from any
    /// thread and must not retry internally: the pipeline's failure rule is
    /// one attempt, soft beep, next sentence.
    ///
    /// Implementations must also bound their own latency. The pipeline's
    /// abort points sit *between* calls, so an engine that wedges inside
    /// `synthesize` stalls pause and stop for exactly as long as it runs —
    /// the espeak engine kills its child at a 30 s timeout, and the Batch 6
    /// in-process engines need an equivalent bound before they ship.
    fn synthesize(&self, text: &str) -> Result<Synthesis, TtsError>;

    /// The voices this engine offers. Best-effort: an engine that cannot
    /// enumerate (binary missing, parse failure) returns an empty list rather
    /// than failing playback — enumeration is a settings-pane concern.
    fn voices(&self) -> Vec<Voice> {
        Vec::new()
    }
}
