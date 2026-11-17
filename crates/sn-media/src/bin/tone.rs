//! `alsa-tone` — Batch 4's gate, in one file.
//!
//! Plays a sweep and reports, from measurements rather than assertions:
//!
//!  * the rate and channels the device actually negotiated,
//!  * how many underruns had to be recovered mid-stream (any is a fail: a
//!    recovery is a potential click, and the gate claims there were none),
//!  * whether any write failed outright,
//!  * whether `position().played` stayed within the buffering budget — the
//!    device's ring plus its plugin's own latency headroom — for the whole run,
//!  * whether the position reached the end.
//!
//! The position check is the gate's real content: "a tone plays with no clicks
//! or gaps; the reported position matches what is audible to within one
//! buffer". A tone that plays and a position that are wrong both pass a naive
//! smoke test.
//!
//! With the optional fourth argument the tone is generated at an engine rate
//! (what espeak-ng or Kokoro will emit) and resampled to the device's rate on
//! the way out — the exact shape the Batch 5 pipeline will have, exercised
//! here so the resampler is proven under real-time playback, not only in
//! unit tests.
//!
//! `--stop-test` is the stop path's coverage: it plays for a moment, stops
//! mid-stream, and checks the two contracts stop must hold on real hardware —
//! the written count rewinds to what was actually played, and a write after
//! stop is refused rather than resurrecting the stream.
//!
//! Usage: alsa-tone [--stop-test] [device] [seconds] [hz] [engine-rate]

use std::time::{Duration, Instant};

use sn_media::{mono_to_stereo, AudioError, Resampler, Sink, CHANNELS, PERIOD_FRAMES, SAMPLE_RATE};

/// A tone sweep segment: one pitch, envelope-faded at both ends.
///
/// A sweep rather than one pitch on purpose: a single sine wave at a rate the
/// device resamples cleanly can hide a resampling fault that a sweep exposes.
fn tone_buffer(device_rate: u32, channels: u16, seconds_per_segment: f64, hz: f64) -> Vec<i16> {
    let frames = (device_rate as f64 * seconds_per_segment) as usize;
    let mut mono = Vec::with_capacity(frames);
    // A short fade at each end. Without it a segment boundary is a step
    // discontinuity, which is exactly the click the gate looks for — generated
    // by the *generator*, not the device.
    let fade = (device_rate as f64 * 0.01).max(1.0) as usize;
    for i in 0..frames {
        let t = i as f64 / device_rate as f64;
        let sample = (t * hz * 2.0 * std::f64::consts::PI).sin();
        let envelope = if i < fade {
            i as f64 / fade as f64
        } else if i >= frames - fade {
            (frames - i) as f64 / fade as f64
        } else {
            1.0
        };
        let value = (sample * envelope * 12_000.0) as i16;
        mono.push(value);
    }
    if channels == 1 {
        mono
    } else {
        mono_to_stereo(&mono)
    }
}

/// How far `played` may lag `written` before the position is called wrong.
///
/// `played` is *supposed* to trail `written` by the depth of the playback
/// chain — that offset is what position() exists to report — so the bound is
/// one full ring plus headroom for the plugin's own latency. Measured on this
/// box, PipeWire's plugin adds ~7.6k frames to what `delay()` reports; the
/// quarter second below covers it with room. A lag beyond this bound means the
/// playhead has drifted away from what is audible, which is the read-along
/// failure this crate exists to prevent.
fn max_lag_frames(device_rate: u32, buffer_frames: u64) -> u64 {
    (buffer_frames + device_rate as u64 / 4).max(PERIOD_FRAMES as u64)
}

/// Feed one interleaved buffer to the device in period-sized writes, tracking
/// the accounting the verdict is built from.
///
/// The position is sampled after every write, mid-stream: the worst lag across
/// many samples is the number the gate bounds, and a single end-of-stream
/// sample would hide every transient.
fn write_buffer(
    sink: &Sink,
    buffer: &[i16],
    state: &mut Measure,
    notes: &mut Vec<String>,
) {
    let frames_per_write = PERIOD_FRAMES as usize;
    for chunk in buffer.chunks(frames_per_write * CHANNELS as usize) {
        if !sink.wait_for(frames_per_write, Duration::from_millis(500)) {
            notes.push("device stopped accepting samples".to_string());
            return;
        }
        match sink.write(chunk) {
            Ok(frames) => state.expected_written += frames as u64,
            Err(e) => {
                notes.push(format!("write failed: {e}"));
                return;
            }
        }
        let p = sink.position();
        if p.written > 0 {
            state.worst_lag = state.worst_lag.max(p.written.saturating_sub(p.played));
            state.samples_checked += 1;
        }
    }
}

/// Everything the verdict is measured from.
#[derive(Default)]
struct Measure {
    expected_written: u64,
    worst_lag: u64,
    samples_checked: usize,
}

/// Interleaved frames waiting for the device: device-rate mono held back so
/// steady-state writes go out in whole periods of the widened stereo stream
/// (one `Pending` period is two ALSA periods of samples).
struct Pending(Vec<i16>);

impl Pending {
    /// Drain and write everything of at least one period.
    fn flush_ready(
        &mut self,
        sink: &Sink,
        state: &mut Measure,
        notes: &mut Vec<String>,
    ) {
        let period_samples = PERIOD_FRAMES as usize * CHANNELS as usize;
        while self.0.len() >= period_samples {
            let rest = self.0.split_off(period_samples);
            let stereo = mono_to_stereo(&std::mem::take(&mut self.0));
            write_buffer(sink, &stereo, state, notes);
            self.0 = rest;
        }
    }

    /// Write whatever remains, period or not.
    ///
    /// Only for end of stream: a partial period dropped here is the tail of
    /// the last word, and the gate's "no gaps" claim does not survive it.
    fn flush_all(&mut self, sink: &Sink, state: &mut Measure, notes: &mut Vec<String>) {
        if self.0.is_empty() {
            return;
        }
        let stereo = mono_to_stereo(&std::mem::take(&mut self.0));
        write_buffer(sink, &stereo, state, notes);
    }
}

/// The stop path's coverage, on the real device.
///
/// Play for a moment, stop mid-stream, and check the two contracts: the
/// written count rewinds to what was actually played (no phantom audio past
/// the stop), and a write afterwards is refused — the producer cannot
/// resurrect the stream. Needs hardware because `stop` is about the device's
/// queued frames; a unit test could only test itself.
fn stop_test(device: &str) -> std::process::ExitCode {
    let sink = match Sink::open(device) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("alsa-tone: cannot open {device}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut notes: Vec<String> = Vec::new();

    // Two seconds of tone, fed in periods; stop half-way through.
    let stereo = tone_buffer(SAMPLE_RATE, CHANNELS, 2.0, 440.0);
    let period = PERIOD_FRAMES as usize * CHANNELS as usize;
    let mut stopped_at = 0u64;
    for (i, chunk) in stereo.chunks(period).enumerate() {
        if i == 24 {
            sink.stop();
            stopped_at = sink.position().played;
            break;
        }
        sink.wait_for(PERIOD_FRAMES as usize, Duration::from_millis(500));
        if let Err(e) = sink.write(chunk) {
            notes.push(format!("write before stop failed: {e}"));
            break;
        }
    }

    // The rewind: nothing may be claimed as written that was not played.
    let after = sink.position();
    if after.written != stopped_at {
        notes.push(format!(
            "written did not rewind to played: {written} vs {played}",
            written = after.written,
            played = stopped_at
        ));
    }
    // The refusal: writing after stop must error, not restart playback.
    match sink.write(&vec![0i16; period]) {
        Ok(_) => notes.push("a write after stop was accepted; stop did not hold".into()),
        Err(AudioError::Stopped) => {}
        Err(e) => notes.push(format!("write after stop failed oddly: {e}")),
    }
    // And the stream must be silent: the position may not advance any more.
    std::thread::sleep(Duration::from_millis(150));
    let settled = sink.position();
    if settled.played != after.played {
        notes.push(format!(
            "the position advanced {} frames after stop; audio is still playing",
            settled.played - after.played
        ));
    }

    if notes.is_empty() {
        println!("OK: stop rewound the position to played, refused later writes, and held");
        std::process::ExitCode::SUCCESS
    } else {
        for n in &notes {
            println!("FAIL: {n}");
        }
        std::process::ExitCode::FAILURE
    }
}

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    if first.as_deref() == Some("--stop-test") {
        return stop_test(args.next().unwrap_or_else(|| "default".to_string()).as_str());
    }
    let device = first.unwrap_or_else(|| "default".to_string());
    let seconds: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(6.0);
    let hz: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(440.0);
    // Optional engine rate: the tone is generated here and resampled to the
    // device's rate, the same path TTS output will take.
    let engine_rate: Option<u32> = args.next().and_then(|s| s.parse().ok());

    let sink = match Sink::open(&device) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("alsa-tone: cannot open {device}: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let (rate, channels) = match sink.negotiated() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("alsa-tone: cannot read the negotiated format: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("device {device}: {rate} Hz, {channels} channel(s)");

    let mut notes: Vec<String> = Vec::new();

    // Three pitches, so a fault that only shows up on a frequency change is
    // visible as a gap rather than being smoothed into nothing.
    let pitches = [hz, hz * 1.5, hz];
    let per_segment = (seconds / pitches.len() as f64).max(0.2);

    // Some only when the tone must be resampled; the write loop branches on it.
    let mut resampling = engine_rate.filter(|g| *g != rate).map(|gen| {
        println!("generating at {gen} Hz, resampling to {rate} Hz");
        (gen, Resampler::new(gen, rate), Pending(Vec::new()))
    });

    let started = Instant::now();
    let mut state = Measure::default();

    for pitch in pitches {
        match &mut resampling {
            None => {
                let buffer = tone_buffer(rate, channels, per_segment, pitch);
                write_buffer(&sink, &buffer, &mut state, &mut notes);
            }
            // Generate mono at the engine rate, resample, widen to stereo,
            // write — the exact shape the TTS pipeline will have.
            Some((gen_rate, resampler, pending)) => {
                let mono = tone_buffer(*gen_rate, 1, per_segment, pitch);
                for chunk in mono.chunks(1_024) {
                    pending.0.extend(resampler.push(chunk));
                }
                pending.flush_ready(&sink, &mut state, &mut notes);
            }
        }
    }
    if let Some((_, resampler, pending)) = &mut resampling {
        pending.0.extend(resampler.flush());
        pending.flush_ready(&sink, &mut state, &mut notes);
        pending.flush_all(&sink, &mut state, &mut notes);
    }

    // Drain: the last buffered audio must actually be played before the
    // position is compared to what was written. The device ends its stream in
    // an underrun — the normal way audio ends — and `drain` handles that as
    // well as the still-running case; the wait afterwards just gives the
    // position's monotonic read a moment to settle.
    sink.drain();
    let drain_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let p = sink.position();
        if p.finished() || Instant::now() > drain_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let final_pos = sink.position();

    let elapsed = started.elapsed();
    println!(
        "wrote {} frames in {:.2}s ({} frames expected)",
        final_pos.written,
        elapsed.as_secs_f64(),
        state.expected_written
    );
    println!(
        "position: played {} frames, worst lag seen {} frames ({} samples)",
        final_pos.played, state.worst_lag, state.samples_checked
    );
    if let Some(e) = sink.take_error() {
        notes.push(format!("device reported an error: {e}"));
    }
    // Each recovered underrun is a potential click; the gate claims there
    // were none, so the count is a measurement, not trivia for the log.
    let underruns = sink.underruns();
    if underruns > 0 {
        notes.push(format!("{underruns} underrun(s) were recovered mid-stream"));
    }

    if final_pos.written != state.expected_written {
        notes.push(format!(
            "accounting mismatch: {} written, {} expected",
            final_pos.written, state.expected_written
        ));
    }
    if !final_pos.finished() {
        notes.push(format!(
            "did not drain: {} frames written, {} played",
            final_pos.written, final_pos.played
        ));
    }
    if state.samples_checked == 0 {
        notes.push("the position was never sampled mid-stream".to_string());
    }
    let budget = max_lag_frames(rate, sn_media::BUFFER_FRAMES as u64);
    if state.worst_lag > budget {
        notes.push(format!(
            "position lagged by {} frames, more than one buffer ({budget})",
            state.worst_lag
        ));
    }

    if notes.is_empty() {
        println!("OK: tone played, playhead stayed within the buffering budget, no underruns, no errors");
        std::process::ExitCode::SUCCESS
    } else {
        for n in &notes {
            println!("FAIL: {n}");
        }
        std::process::ExitCode::FAILURE
    }
}
