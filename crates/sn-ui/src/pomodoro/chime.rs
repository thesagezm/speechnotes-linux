//! The Pomodoro's soft two-note chime. Split out of `pomodoro.rs`
//! (Round 4, pure code-motion).


// ---------------------------------------------------------------------------
// The chime
// ---------------------------------------------------------------------------

/// A soft two-note chime, if the config says so and a player exists.
///
/// The WAV is synthesised once into the cache — no asset to ship, no codec
/// dependency — and played by whatever system player is present (`paplay`
/// on this box), detached: the timer's thread must not wait for the last
/// note. A missing player is not an error; the banner carries the news
/// alone.
pub(crate) fn chime(enabled: bool) {
    if !enabled {
        return;
    }
    let path = sn_core::paths::cache_dir().join("pomodoro-chime.wav");
    if !path.exists() {
        if let Err(e) = write_chime_wav(&path) {
            tracing::debug!(error = %e, "could not synthesise the chime");
            return;
        }
    }
    let played = std::process::Command::new("paplay")
        .arg(&path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok();
    if !played {
        // One fallback, quiet about its own failure too.
        let _ = std::process::Command::new("ffplay")
            .args(["-nodisp", "-autoexit", "-loglevel", "quiet"])
            .arg(&path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Write a small mono 16-bit WAV: E5 rising to C5, each note fading in and
/// out so nothing clicks. 22050 Hz keeps the file under 25 KB.
pub(crate) fn write_chime_wav(path: &std::path::Path) -> std::io::Result<()> {
    const RATE: u32 = 22050;
    const NOTES: &[(f64, f64)] = &[(659.25, 0.20), (523.25, 0.34)];
    let rate = RATE as f64;
    let mut samples: Vec<f64> = Vec::new();
    for &(freq, secs) in NOTES {
        let n = (secs * rate) as usize;
        for i in 0..n {
            let t = i as f64 / rate;
            // A 12 ms fade at each end of each note; no envelope, and the
            // waveform's own edges click.
            let fade_in = (t / 0.012).min(1.0);
            let fade_out = ((secs - t) / 0.08).min(1.0);
            let envelope = fade_in.clamp(0.0, 1.0) * fade_out.clamp(0.0, 1.0);
            samples.push(0.32 * envelope * (2.0 * std::f64::consts::PI * freq * t).sin());
        }
    }
    let data_len = (samples.len() * 2) as u32;
    let mut bytes: Vec<u8> = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&RATE.to_le_bytes());
    bytes.extend_from_slice(&(RATE * 2).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    // The chime is regenerable decoration; a temp-then-rename keeps two
    // timers finishing together from tearing the file.
    let tmp = path.with_extension("wav.part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path)
}
