//! The resampler: engine rate to device rate, streaming and deterministic.
//!
//! Every TTS engine in this family speaks at its own rate — espeak-ng at
//! 22050 Hz, Kokoro at 24000 — while the device runs at
//! [`crate::SAMPLE_RATE`]. The sink takes one rate only, by design: a rate
//! mismatch surfacing as an error is how a misconfiguration gets noticed,
//! and the conversion belongs in one place where it can be measured.
//!
//! The filter is a Blackman-windowed sinc with linearly-interpolated table
//! lookup — the same construction libsamplerate uses for its mid qualities.
//! For voice the numbers that matter are: DC gain exactly 1, no fold-back of
//! energy above the *input's* Nyquist when downsampling, and bit-identical
//! output regardless of where the caller's chunk boundaries fall. The tests
//! hold all three, and the chunk-boundary test is the one that catches the
//! classic bug (filter state reset per chunk = a click every chunk).
//!
//! The stream conceptually starts and ends in silence: the history is
//! pre-rolled with one window of zeros, so the first outputs are the filter's
//! ramp-in rather than a step, and `flush` emits the tail whose window still
//! overlaps real input. Nothing else is emitted — padding can never
//! synthesise audio the stream did not contain.
//!
//! Mono only. Engines emit mono and [`mono_to_stereo`] duplicates after, so
//! an interleaved stereo stream never crosses this type.

/// Taps per side of the sinc. The window reaches zero at ±[`ZERO_CROSSINGS`],
/// so taps one step further out exist only to make interpolation at the
/// boundary read zeros rather than clamp.
const ZERO_CROSSINGS: usize = 24;

/// Fractional-delay resolution: the sinc is tabulated this many steps per zero
/// crossing and linearly interpolated between steps. 512 puts interpolation
/// error far below the 16-bit floor at ~200 KB of table.
const TABLE_STEPS: usize = 512;

/// Fraction of Nyquist the filter passes. The 0.95 leaves a transition band
/// for the window to work with; a filter cut exactly at Nyquist aliases its
/// own transition band back into the passband.
const CUTOFF: f64 = 0.475;

/// Resample mono i16 from one rate to another.
///
/// Feed [`push`](Resampler::push) as chunks arrive; call
/// [`flush`](Resampler::flush) once at end of stream to emit the tail the
/// filter still holds. Output is bit-identical for any chunking.
pub struct Resampler {
    from_rate: u32,
    to_rate: u32,
    /// Windowed sinc at this instance's cutoff, `TABLE_STEPS` steps per zero
    /// crossing, centred at index `(ZERO_CROSSINGS + 1) * TABLE_STEPS`. Empty
    /// in passthrough mode (equal rates).
    table: Vec<f32>,
    /// Input history. Consumed from the front as the position advances, so it
    /// holds only the taps the next few outputs still need.
    hist: Vec<f32>,
    /// Position of the next output sample, in fractional input samples,
    /// measured in `hist` coordinates.
    t: f64,
    /// Input samples per output sample, `from / to`.
    dt: f64,
    /// Real input samples pushed (pre-roll zeros excluded). The tail loop in
    /// [`Resampler::flush`] uses it to stop once the window no longer touches
    /// anything the stream contained.
    real_len: u64,
    flushed: bool,
}

impl Resampler {
    /// A resampler from one rate to the other. Equal rates short-circuit to a
    /// bit-exact passthrough — running audio through a filter to divide by
    /// one is where the subtle losses live.
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        assert!(from_rate > 0 && to_rate > 0, "rates must be positive");
        let mut table = Vec::new();
        if from_rate != to_rate {
            // Cutoff in input-sample cycles: the input's Nyquist when
            // upsampling, the output's when down. This is what stops a
            // downsampler from folding 19 kHz onto 5 kHz.
            let fc = CUTOFF * (to_rate.min(from_rate) as f64 / from_rate as f64);
            let span = (ZERO_CROSSINGS + 1) as f64;
            let steps = 2.0 * span * TABLE_STEPS as f64;
            table.reserve(steps as usize + 1);
            for j in 0..=(steps as usize) {
                let x = j as f64 / TABLE_STEPS as f64 - span;
                table.push(windowed_sinc(x, fc) as f32);
            }
        }
        Self {
            from_rate,
            to_rate,
            table,
            // Pre-roll: one window of silence before the stream, so the first
            // output is a ramp-in from silence instead of a half-window step.
            hist: vec![0.0; ZERO_CROSSINGS],
            t: ZERO_CROSSINGS as f64,
            dt: from_rate as f64 / to_rate as f64,
            real_len: 0,
            flushed: false,
        }
    }

    /// The rates this instance converts between.
    pub fn rates(&self) -> (u32, u32) {
        (self.from_rate, self.to_rate)
    }

    /// Resample the next chunk, returning everything that is ready.
    ///
    /// After [`flush`](Resampler::flush) this returns empty: the filter has no
    /// future input to wait for, and silently continuing would drop the tail.
    /// The flushed check deliberately precedes the passthrough shortcut — an
    /// equal-rate resampler is still a stream with an end.
    pub fn push(&mut self, input: &[i16]) -> Vec<i16> {
        if self.flushed {
            return Vec::new();
        }
        if self.from_rate == self.to_rate {
            return input.to_vec();
        }
        self.hist.extend(input.iter().map(|s| *s as f32 / 32_768.0));
        self.real_len += input.len() as u64;
        self.drain_output()
    }

    /// End of stream: emit the tail, including the samples whose filter window
    /// still overlaps real input. Idempotent; the second call returns empty.
    pub fn flush(&mut self) -> Vec<i16> {
        if self.from_rate == self.to_rate || self.flushed || self.real_len == 0 {
            self.flushed = true;
            return Vec::new();
        }
        self.flushed = true;
        // `hist` ends exactly where the real input ends (nothing is buffered
        // beyond it before padding), so this snapshot is the boundary the tail
        // loop must not speak past. It is an index into hist *as it stands
        // now* — which is why the loop below must not trim: every trim
        // renumbers hist and would stale the snapshot, speaking past the end.
        let real_end = self.hist.len();
        // Enough zeros for the window to slide fully past the real end, with
        // slack for the coarsest step the instance can have.
        let pad = (2 * ZERO_CROSSINGS + 2) * (1 + self.dt.ceil() as usize);
        self.hist.resize(self.hist.len() + pad, 0.0);
        let mut out = Vec::new();
        let half = ZERO_CROSSINGS as isize;
        while let Some((i0, frac)) = self.next_base_index() {
            if i0 - half >= real_end as isize {
                break; // window entirely inside padding: past the real end
            }
            out.push(self.render_output(i0, frac));
            self.t += self.dt;
        }
        out
    }

    /// Emit outputs while the filter window fits in buffered samples.
    fn drain_output(&mut self) -> Vec<i16> {
        let mut out = Vec::new();
        while let Some((i0, frac)) = self.next_base_index() {
            out.push(self.render_output(i0, frac));
            self.t += self.dt;
            self.trim_history();
        }
        out
    }

    /// The integer base index and fraction of the next output, if enough
    /// history exists to render it. Indexes are `hist` coordinates, which
    /// `trim_history` keeps small and exact.
    fn next_base_index(&self) -> Option<(isize, f32)> {
        let i0 = self.t.round();
        let half = ZERO_CROSSINGS as f64;
        if i0 - half < 0.0 || i0 + half >= self.hist.len() as f64 {
            return None;
        }
        let frac = (self.t - i0) as f32;
        Some((i0 as isize, frac))
    }

    /// One output sample: the windowed sinc at fractional position `t`,
    /// interpolated from the table.
    fn render_output(&self, i0: isize, frac: f32) -> i16 {
        let span = (ZERO_CROSSINGS + 1) as f32;
        let mut acc = 0f32;
        for k in -(ZERO_CROSSINGS as isize)..=(ZERO_CROSSINGS as isize) {
            let x = k as f32 - frac;
            let tx = (x + span) * TABLE_STEPS as f32;
            let j = tx as usize;
            let f = tx - j as f32;
            let h = self.table[j] * (1.0 - f) + self.table[j + 1] * f;
            acc += self.hist[(i0 + k) as usize] * h;
        }
        // Round to nearest, not toward zero: truncation biases quiet passages
        // down by half a step, which over a whole chapter is a DC shift.
        let v = (acc * 32_768.0).round();
        v.clamp(-32_768.0, 32_767.0) as i16
    }

    /// Drop history the rendered outputs will never read again, keeping `t`
    /// coordinates exact by subtracting the same count.
    fn trim_history(&mut self) {
        let keep_from = (self.t.round() as isize - ZERO_CROSSINGS as isize).max(0) as usize;
        if keep_from > 0 {
            self.hist.drain(..keep_from);
            self.t -= keep_from as f64;
        }
    }
}

/// Blackman-windowed sinc at cutoff `fc` input-cycles per sample.
fn windowed_sinc(x: f64, fc: f64) -> f64 {
    let n = x.abs();
    if n >= ZERO_CROSSINGS as f64 {
        return 0.0;
    }
    let window = 0.42 + 0.5 * (std::f64::consts::PI * n / ZERO_CROSSINGS as f64).cos()
        + 0.08 * (2.0 * std::f64::consts::PI * n / ZERO_CROSSINGS as f64).cos();
    let sinc = if n < 1e-9 {
        2.0 * fc
    } else {
        (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x)
    };
    sinc * window
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-noise, so the chunking test compares real content
    /// and a regression is reproducible from the seed.
    struct Lcg(u64);
    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as f32 / u32::MAX as f32) * 2.0 - 1.0
        }
    }

    fn sine(rate: u32, hz: f64, seconds: f64) -> Vec<i16> {
        let frames = (rate as f64 * seconds) as usize;
        (0..frames)
            .map(|i| {
                let t = i as f64 / rate as f64;
                ((t * hz * 2.0 * std::f64::consts::PI).sin() * 12_000.0) as i16
            })
            .collect()
    }

    /// Everything `r` will ever produce for `input`, pushed and flushed.
    fn resample_all(r: &mut Resampler, input: &[i16]) -> Vec<i16> {
        let mut out = r.push(input);
        out.extend(r.flush());
        out
    }

    #[test]
    fn equal_rates_pass_through_bit_exact() {
        let mut r = Resampler::new(48_000, 48_000);
        let input: Vec<i16> = (0..1000_i16).map(|i| i * 31 - 15_000).collect();
        assert_eq!(r.push(&input), input);
        assert!(r.flush().is_empty());
    }

    #[test]
    fn output_length_tracks_the_ratio() {
        // One second at 22050 into 48000. The exact figure is ratio*len plus
        // the flush tail: the filter's window holds ±ZERO_CROSSINGS input
        // samples, and the last outputs are the fade-out of the final real
        // samples — real audio, up to 2*Z/dt frames of it, not padding.
        let mut r = Resampler::new(22_050, 48_000);
        let out = resample_all(&mut r, &sine(22_050, 440.0, 1.0));
        let tail_max = (2 * ZERO_CROSSINGS) as f64 / (22_050 as f64 / 48_000.0);
        assert!(
            out.len() as f64 >= 48_000.0 && (out.len() as f64 - 48_000.0) <= tail_max + 2.0,
            "got {} frames for 1 s (tail bound {tail_max:.0})",
            out.len()
        );
    }

    #[test]
    fn chunk_boundaries_cannot_change_the_output() {
        // The classic streaming-resampler bug is filter state reset per chunk:
        // one click per chunk. Bit-identical output for any chunking is the
        // property that rules it out.
        let mut lcg = Lcg(0x5EED);
        let input: Vec<i16> = (0..20_000).map(|_| (lcg.next_f32() * 20_000.0) as i16).collect();

        let mut one_shot = Resampler::new(22_050, 48_000);
        let whole = resample_all(&mut one_shot, &input);

        for chunk_size in [1, 7, 997, 4096] {
            let mut chunked = Resampler::new(22_050, 48_000);
            let mut out = Vec::new();
            for chunk in input.chunks(chunk_size) {
                out.extend(chunked.push(chunk));
            }
            out.extend(chunked.flush());
            assert_eq!(out, whole, "chunk size {chunk_size} changed the output");
        }
    }

    #[test]
    fn flush_is_idempotent_and_push_after_it_yields_nothing() {
        let mut r = Resampler::new(24_000, 48_000);
        let input = sine(24_000, 440.0, 0.05);
        let mut out = r.push(&input);
        out.extend(r.flush());
        assert!(!out.is_empty());
        assert!(r.flush().is_empty(), "second flush emitted more");
        assert!(r.push(&input).is_empty(), "push after flush emitted more");

        // Passthrough included: an equal-rate resampler is still a stream
        // with an end, and its shortcut must not route around the flush.
        let mut passthrough = Resampler::new(48_000, 48_000);
        assert_eq!(passthrough.push(&input), input);
        assert!(passthrough.flush().is_empty());
        assert!(
            passthrough.push(&input).is_empty(),
            "passthrough push after flush emitted more"
        );
    }

    #[test]
    fn flushing_an_empty_stream_emits_nothing() {
        // The pre-roll zeros are not audio; a stream that never received a
        // sample must not speak on flush.
        let mut r = Resampler::new(24_000, 48_000);
        assert!(r.flush().is_empty());
    }

    #[test]
    fn a_tone_keeps_its_pitch() {
        // 440 Hz at 22050 into 48000: crossings in the measured window must
        // still say 440 Hz. A resampler that smoothed or aliased fails this
        // long before it fails a listening test.
        let mut r = Resampler::new(22_050, 48_000);
        let out = resample_all(&mut r, &sine(22_050, 440.0, 1.0));

        // Measure the middle 0.8 s, away from the filter's ramp-in/out.
        let window = &out[4_800..40_000];
        // Count upward crossings only, zeros included with the rise: 440 Hz
        // and 48 kHz are commensurate (1200/11), so the waveform lands exactly
    // ON zero every 600 frames, and a signum-based count charges each of
        // those samples twice — 59 phantom crossings on this exact window.
        let crossings = window.windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count();
        let expected = 440.0 * window.len() as f64 / 48_000.0;
        let error = (crossings as f64 - expected).abs() / expected;
        assert!(error < 0.02, "{crossings} crossings, expected {expected:.0}");
    }

    #[test]
    fn dc_passes_at_unity() {
        // A constant must come out as the same constant: gain 1, no bias. This
        // is the read-along positions-are-seconds property in disguise — a
        // resampler whose DC gain drifts also drifts everything else.
        let mut r = Resampler::new(24_000, 48_000);
        let out = resample_all(&mut r, &vec![16_384_i16; 4_800]);
        // Skip the ramp-in where the window is still filling, and the flush
        // tail where the window is sliding off the end — both are transition,
        // not steady state.
        let settled = &out[200..out.len() - 150];
        let mean: f64 = settled.iter().map(|s| *s as f64).sum::<f64>() / settled.len() as f64;
        assert!(
            (mean - 16_384.0).abs() < 32.0,
            "DC gain off: mean {mean:.1} for input 16384"
        );
        // And flat must stay flat — ripple here would be filter ringing.
        let spread = settled
            .iter()
            .map(|s| (*s as i32 - 16_384).abs())
            .max()
            .unwrap();
        assert!(spread <= 4, "DC spread {spread} LSB");
    }

    #[test]
    fn downsampling_folds_nothing_above_the_new_nyquist() {
        // 19 kHz at 48k, down to 24k. 19 kHz is above the 12 kHz Nyquist of
        // the output; a resampler without a real anti-alias filter folds it to
        // 5 kHz at full volume, which is the audible disaster this filter
        // exists to prevent.
        let mut r = Resampler::new(48_000, 24_000);
        let loud = sine(48_000, 19_000.0, 0.5);
        let input_rms = rms(&loud);
        let out = resample_all(&mut r, &loud);
        let out_rms = rms(&out[2_000..out.len() - 2_000]);
        assert!(
            out_rms < input_rms * 0.05,
            "alias survived: {out_rms:.0} vs input {input_rms:.0}"
        );

        // In-band content must survive the same trip at full level: RMS is
        // amplitude per sample, so resampling 48k→24k must not change it.
        let mut r = Resampler::new(48_000, 24_000);
        let kept = sine(48_000, 440.0, 0.5);
        let kept_rms = rms(&kept);
        let out = resample_all(&mut r, &kept);
        let out_rms = rms(&out[2_000..out.len() - 2_000]);
        assert!(
            (out_rms - kept_rms).abs() < kept_rms * 0.03,
            "in-band level wrong: {out_rms:.0} vs input {kept_rms:.0}"
        );
    }

    fn rms(samples: &[i16]) -> f64 {
        let energy: f64 = samples.iter().map(|s| (*s as f64) * (*s as f64)).sum();
        (energy / samples.len() as f64).sqrt()
    }

    #[test]
    fn position_stays_bounded_over_a_long_stream() {
        // `t` must not creep: an hour of speech is 170M output samples, and an
        // f64 that only ever adds would drift by then. Trimming subtracts
        // exactly, so the coordinate stays small; this test proves it.
        let mut r = Resampler::new(22_050, 48_000);
        let mut lcg = Lcg(0xC0FFEE);
        let chunk: Vec<i16> = (0..1_024).map(|_| (lcg.next_f32() * 10_000.0) as i16).collect();
        for _ in 0..2_000 {
            r.push(&chunk);
        }
        assert!(r.t.abs() < 1_000.0, "position coordinate crept to {}", r.t);
        assert!(
            r.hist.len() < 4 * ZERO_CROSSINGS + 8,
            "history grew to {} samples",
            r.hist.len()
        );
    }
}
