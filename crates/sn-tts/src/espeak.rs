//! The espeak-ng engine: shells out to the CLI.
//!
//! The box has the `espeak-ng` runtime and `libespeak-ng.so.1` but **no dev
//! headers and no pkg-config**, so there is no FFI here — every synthesis is
//! one CLI process writing a WAV file we then parse. At espeak's real-time
//! factor (~0.015 measured on this box) the process spawn overhead is noise,
//! and it buys complete isolation: a wedged synthesis is killed at the
//! timeout and the sentence is skipped, instead of hanging the pipeline.
//!
//! Speed is requested from espeak itself (`-s`, words per minute) rather than
//! resampled afterwards: a time-stretch would change pitch, and a pitch
//! shift is exactly the quality regression the engine tier exists to avoid.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::engine::{Engine, Synthesis, Voice};
use crate::TtsError;

/// espeak's own default rate. `rateMultiplier` scales from here, matching
/// what "1.0×" means everywhere else in the family.
pub const DEFAULT_WPM: u32 = 175;
/// espeak refuses rates outside roughly 80–450 wpm; clamped at the boundary.
pub const MIN_WPM: u32 = 80;
pub const MAX_WPM: u32 = 450;
/// One chunk is at most a few hundred characters; a wedged espeak gets this
/// long before it is killed and the chunk is skipped.
const SYNTH_TIMEOUT: Duration = Duration::from_secs(30);

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// The espeak-ng engine. Construct with [`EspeakEngine::new`] from prefs.
pub struct EspeakEngine {
    /// The voice code passed to `-v` (e.g. `en`, `en-gb`, `de`). Empty means
    /// espeak's own default.
    voice: String,
    wpm: u32,
}

impl EspeakEngine {
    /// Build from the prefs shape: a per-engine voice code and a rate
    /// multiplier around 1.0.
    pub fn new(voice: impl Into<String>, rate_multiplier: f64) -> Self {
        let multiplier = rate_multiplier.clamp(0.5, 2.0);
        let wpm = ((DEFAULT_WPM as f64) * multiplier).round() as u32;
        Self::with_wpm(voice, wpm)
    }

    /// Build with a raw words-per-minute rate, clamped to what espeak accepts.
    pub fn with_wpm(voice: impl Into<String>, wpm: u32) -> Self {
        Self {
            voice: voice.into(),
            wpm: wpm.clamp(MIN_WPM, MAX_WPM),
        }
    }

    /// The espeak binary is a runtime requirement, not a build dependency, so
    /// availability is checked where the result can be shown to a person.
    pub fn available() -> Result<(), TtsError> {
        let ok = Command::new("espeak-ng")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            Err(TtsError::Unavailable {
                engine: "espeak",
                detail: "the espeak-ng command was not found on PATH".to_string(),
            })
        }
    }
}

impl Engine for EspeakEngine {
    fn name(&self) -> &'static str {
        "espeak"
    }

    fn synthesize(&self, text: &str) -> Result<Synthesis, TtsError> {
        if text.trim().is_empty() {
            return Err(TtsError::Empty);
        }
        let path = std::env::temp_dir().join(format!(
            "sn-tts-{}-{}.wav",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));

        let started = Instant::now();
        self.run_espeak(text, &path)?;
        let parsed = sn_format::wav::read_samples(&path).map_err(|e| {
            TtsError::Synthesis(format!("espeak wrote an unreadable WAV: {e}"))
        });
        // Best effort: a temp file left behind by a crash is in /tmp, where
        // the system sweep belongs to the distro, not to us.
        let _ = std::fs::remove_file(&path);
        let (sample_rate, samples) = parsed?;
        if samples.is_empty() {
            return Err(TtsError::Synthesis(
                "espeak produced no samples for speakable text".to_string(),
            ));
        }

        Ok(Synthesis {
            sample_rate,
            samples,
            synth_time: started.elapsed(),
        })
    }

    fn voices(&self) -> Vec<Voice> {
        list_voices()
    }
}

/// The espeak voices from the CLI, without an engine in hand. The picker
/// needs the list before it has built anything, and one process spawn is
/// cheap enough to do on demand.
pub fn list_voices() -> Vec<Voice> {
    parse_voices(
        &match Command::new("espeak-ng").arg("--voices").output() {
            Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
            Err(_) => String::new(),
        },
    )
}

impl EspeakEngine {
    fn run_espeak(&self, text: &str, path: &std::path::Path) -> Result<(), TtsError> {
        let mut child = Command::new("espeak-ng")
            .args(self.synth_args(path))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| TtsError::Unavailable {
                engine: "espeak",
                detail: format!("could not spawn espeak-ng: {e}"),
            })?;

        // A drain thread owns stderr: a child that writes more than a pipe
        // buffer of diagnostics must not block on a pipe nobody reads while
        // the timeout loop waits for an exit that cannot happen.
        let stderr_pipe = child.stderr.take();
        let stderr_drain = std::thread::spawn(move || {
            let mut buf = String::new();
            if let Some(mut pipe) = stderr_pipe {
                use std::io::Read;
                let _ = pipe.read_to_string(&mut buf);
            }
            buf
        });

        // The chunk is at most a few hundred characters, far under the pipe
        // buffer, so the write cannot deadlock against espeak's output.
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }

        match wait_with_timeout(child, SYNTH_TIMEOUT) {
            Ok(status) => {
                let stderr = stderr_drain.join().unwrap_or_default();
                if !status.success() {
                    return Err(TtsError::Synthesis(format!(
                        "espeak-ng exited with {status}: {}",
                        last_line(&stderr)
                    )));
                }
                Ok(())
            }
            Err(e) => {
                // The child is dead by the time wait_with_timeout returns an
                // error (killed on timeout), so the drain thread is at EOF;
                // join it rather than leaving it attached to nothing.
                let _ = stderr_drain.join();
                Err(e)
            }
        }
    }

    fn synth_args(&self, path: &std::path::Path) -> Vec<String> {
        let mut args = Vec::new();
        if !self.voice.is_empty() {
            args.push("-v".to_string());
            args.push(self.voice.clone());
        }
        args.push("-s".to_string());
        args.push(self.wpm.to_string());
        args.push("-w".to_string());
        args.push(path.display().to_string());
        // Text arrives on stdin: no argument-length limits, no quoting, and a
        // chunk that happens to start with `-` cannot be read as a flag.
        args.push("--stdin".to_string());
        args
    }
}



/// Wait for the child with a deadline, killing it if it overshoots.
///
/// `std` has no wait-with-timeout, and a hung espeak must not hang the synth
/// thread: the skip rule ("never worse than the error") needs a deadline to
/// fire against.
fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, TtsError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(status),
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(TtsError::Timeout(timeout));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn last_line(s: &str) -> &str {
    s.lines().last().unwrap_or("")
}

/// Parse `espeak-ng --voices` output.
///
/// The voice *name* may contain spaces ("English (America)"), so the columns
/// are read from both ends: the fixed fields first, the file path last, and
/// whatever lies between is the name. The header line is skipped.
fn parse_voices(text: &str) -> Vec<Voice> {
    let mut voices = Vec::new();
    for line in text.lines().skip(1) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        // Pty Language Age/Gender ... File — five fields at minimum.
        if tokens.len() < 5 {
            continue;
        }
        let code = tokens[1].to_string();
        let file = tokens[tokens.len() - 1].to_string();
        let name = tokens[3..tokens.len() - 1].join(" ");
        // espeak's voice CODE is the language tag ("en-gb", "de"): the
        // language selector reads it back out of the same field.
        let locale = code.clone();
        voices.push(Voice {
            code,
            name: if name.is_empty() { file } else { name },
            locale: Some(locale),
        });
    }
    voices
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> EspeakEngine {
        EspeakEngine::with_wpm("en", DEFAULT_WPM)
    }

    #[test]
    fn espeak_is_available_on_this_box() {
        EspeakEngine::available().expect("espeak-ng should be installed");
    }

    #[test]
    fn synthesizes_a_sentence_at_a_sane_rate() {
        let out = engine()
            .synthesize("The quick brown fox jumps over the lazy dog.")
            .expect("synthesis");
        assert_eq!(out.sample_rate, 22_050, "espeak's native rate");
        // Half a second of speech for a nine-word sentence at minimum — a
        // rate far off would mean the WAV was parsed at the wrong rate.
        assert!(
            out.audio_time().as_secs_f64() > 0.4,
            "audio too short: {:?}",
            out.audio_time()
        );
        // RTF is deliberately NOT bounded here, for the reason the neural
        // engines' tests give: this test thread shares the box with several
        // ONNX sessions, and espeak's synthesis is a process spawn whose wall
        // clock the scheduler decides. The bound below 0.5 passed alone and
        // failed under contention, which is noise, not a regression.
        // espeak's real-time factor is measured sequentially by `--speak`,
        // which is the gate that reports it.
    }

    #[test]
    fn empty_text_is_rejected_before_spawning() {
        assert!(matches!(engine().synthesize("   \n\t "), Err(TtsError::Empty)));
    }

    #[test]
    fn wpm_clamps_to_espeaks_range() {
        assert_eq!(EspeakEngine::with_wpm("", 1).wpm, MIN_WPM);
        assert_eq!(EspeakEngine::with_wpm("", 100_000).wpm, MAX_WPM);
        // 1.0× is espeak's own default.
        assert_eq!(EspeakEngine::new("", 1.0).wpm, DEFAULT_WPM);
        // 2× is the multiplier ceiling; espeak's default rate scales from it.
        assert_eq!(EspeakEngine::new("", 9.0).wpm, (MAX_WPM as f64 * 0.0 + 2.0 * DEFAULT_WPM as f64) as u32);
    }

    #[test]
    fn an_unknown_voice_fails_once_with_a_message() {
        // The failure must be a clean Err (the pipeline skips and beeps), not
        // a panic and not an empty synthesis.
        let e = EspeakEngine::with_wpm("zz-not-a-voice", DEFAULT_WPM);
        match e.synthesize("Hello there.") {
            Err(TtsError::Synthesis(msg)) => assert!(!msg.is_empty()),
            Err(other) => panic!("unexpected error: {other}"),
            Ok(_) => panic!("an unknown voice must not synthesise"),
        }
    }

    #[test]
    fn voice_listing_parses_names_with_spaces() {
        let voices = engine().voices();
        assert!(!voices.is_empty(), "espeak-ng --voices returned nothing");
        // espeak 1.51 lists no bare `en`; its default English is en-gb.
        assert!(voices.iter().any(|v| v.code == "en-gb"), "english missing");
        for v in &voices {
            assert!(!v.code.is_empty() && !v.name.is_empty());
        }
    }

    #[test]
    fn parse_voices_skips_the_header_and_short_lines() {
        let text = "Pty Language Age/Gender VoiceName          File  Other\n\
                    Ignore me\n\
                     5  af       --/M      Afrikaans          gmw/af\n\
                     5  en-us    --/M      English (America)  gmw/en-US";
        let voices = parse_voices(text);
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[0].code, "af");
        assert_eq!(voices[0].name, "Afrikaans");
        assert_eq!(voices[1].code, "en-us");
        assert_eq!(voices[1].name, "English (America)");
    }
}
