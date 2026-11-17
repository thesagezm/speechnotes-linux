//! The Kokoro engine: one ONNX graph (fp32 only — the uint8 tier is proven
//! corrupt), espeak IPA phonemes into the model's 115-symbol vocab, style
//! vectors from per-voice `.bin` banks, 24 kHz out.
//!
//! Contract (verified against the graph on this box): `input_ids` i64
//! [1, N] — no BOS/EOS wrap, the graph adds them internally; `style` f32
//! [1, 256], one row of the voice bank selected by the *phoneme string's
//! length* (`clamp(chars − 1, 0, rows − 1)` — the reference picks a
//! similar-length utterance of the same speaker, and the row is computed
//! before tokenisation so a substitution cannot move the speaker); `speed`
//! f32 [1] — the model takes speed natively, so 2× is exactly twice as fast
//! with the same pitch. Output `waveform` f32 [1, S] (some exports name it
//! `audio`; resolved from the session).
//!
//! A phoneme the vocab lacks becomes a SPACE (the model's own word
//! separator), never a deletion: deleting silently shortens the phoneme
//! stream and the word comes out wrong with nothing in the log.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use sn_format::wav::f32_to_i16;

use crate::engine::{Engine, Synthesis, Voice};
use crate::ortrt::{self, Session};
use crate::{phonemes, TtsError};

/// The graph refuses (or degenerates on) more than 510 phoneme tokens.
const MAX_TOKENS: usize = 510;
/// Style-dim: every voice bank row is 256 floats.
pub(crate) const STYLE_DIM: usize = 256;

pub struct KokoroEngine {
    session: Session,
    output_name: String,
    vocab: HashMap<String, i64>,
    space_id: i64,
    voices_dir: std::path::PathBuf,
    voice: String,
    /// The loaded voice bank, 510×256 flat. Loaded with the engine so a
    /// missing `.bin` is a load error, not a mid-sentence one.
    voice_flat: Vec<f32>,
    rate_multiplier: f64,
}

impl KokoroEngine {
    /// Load from the kokoro model directory (`model.onnx`, `tokenizer.json`,
    /// `voices/<voice>.bin`).
    pub fn load(model_dir: &Path, voice: &str, rate_multiplier: f64) -> Result<Self, TtsError> {
        let graph = model_dir.join("model.onnx");
        if !graph.exists() {
            return Err(TtsError::Unavailable {
                engine: "kokoro",
                detail: format!("{} not found", graph.display()),
            });
        }
        let session = Session::load(&graph)?;
        let output_name = if session.has_output("waveform") {
            "waveform".to_string()
        } else {
            session.first_output()
        };

        let tokenizer_path = model_dir.join("tokenizer.json");
        let raw = std::fs::read_to_string(&tokenizer_path).map_err(TtsError::Io)?;
        let tokenizer: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| TtsError::ModelLoad(e.to_string()))?;
        let vocab: HashMap<String, i64> = tokenizer["model"]["vocab"]
            .as_object()
            .ok_or_else(|| TtsError::ModelLoad("tokenizer.json has no model.vocab".into()))?
            .iter()
            .filter_map(|(k, v)| v.as_i64().map(|id| (k.clone(), id)))
            .collect();
        if vocab.is_empty() {
            return Err(TtsError::ModelLoad("tokenizer.json vocab is empty".into()));
        }
        // id 16 is the space in this vocab; fall back to the vocab's own
        // entry only if a future renumbering moves it.
        let space_id = vocab.get(" ").copied().unwrap_or(16);

        let voices_dir = model_dir.join("voices");
        let voice_flat = load_voice(&voices_dir, voice)?;

        tracing::info!(
            vocab = vocab.len(),
            voice = %voice,
            output = %output_name,
            "kokoro loaded"
        );
        Ok(Self {
            session,
            output_name,
            vocab,
            space_id,
            voices_dir,
            voice: voice.to_string(),
            voice_flat,
            rate_multiplier: rate_multiplier.clamp(0.5, 2.0),
        })
    }

    /// Switch voice. Reloads the style bank; a missing `.bin` keeps the old
    /// voice and reports.
    pub fn set_voice(&mut self, voice: &str) -> Result<(), TtsError> {
        self.voice_flat = load_voice(&self.voices_dir, voice)?;
        self.voice = voice.to_string();
        Ok(())
    }

    pub fn set_rate(&mut self, rate_multiplier: f64) {
        self.rate_multiplier = rate_multiplier.clamp(0.5, 2.0);
    }

    fn synthesize_inner(&self, text: &str) -> Result<Synthesis, TtsError> {
        let started = Instant::now();
        let (tokens, phoneme_chars) = tokenize_for_kokoro_family(
            text,
            &self.vocab,
            self.space_id,
            &self.voice,
        )?;

        // Style row by length, on the pre-tokenisation count.
        let rows = self.voice_flat.len() / STYLE_DIM;
        let adjusted = (phoneme_chars.saturating_sub(1)).min(rows.saturating_sub(1));
        let offset = adjusted * STYLE_DIM;
        let style: Vec<f32> = self.voice_flat[offset..offset + STYLE_DIM].to_vec();

        let n = tokens.len() as i64;
        let speed = self.rate_multiplier as f32;
        let inputs = vec![
            ("input_ids", ortrt::tensor_i64(&[1, n], tokens.clone())?),
            ("style", ortrt::tensor_f32(&[1, 256], style)?),
            ("speed", ortrt::tensor_f32(&[1], vec![speed])?),
        ];
        let mut taken = self.session.run_f32(inputs, &[&self.output_name])?;
        let samples = taken.remove(0);
        if samples.is_empty() {
            return Err(TtsError::Synthesis(
                "kokoro produced no samples for speakable text".to_string(),
            ));
        }
        let mono: Vec<i16> = samples.iter().copied().map(f32_to_i16).collect();
        tracing::debug!(
            tokens = tokens.len(),
            audio_s = mono.len() as f64 / 24_000.0,
            synth_ms = started.elapsed().as_millis() as u64,
            "kokoro chunk synthesised"
        );
        let mut syn = Synthesis {
            sample_rate: 24_000,
            samples: mono,
            synth_time: started.elapsed(),
        };
        // The chunker cuts run-ons and headings at word boundaries, so the
        // phoneme stream often ends without a sentence-final pause and the
        // model stops mid-waveform — a full-amplitude last sample truncated
        // by the resampler is the click at chunk ends. Soften the edge.
        syn.fade_edges();
        Ok(syn)
    }
}

/// The kokoro family's front end, shared by every engine whose text side is
/// kokoro's: IPA phonemes into the vocab's ids, unknown graphemes as spaces
/// (never deletions — deleting silently shortens the stream and the word
/// comes out wrong with nothing in the log), and the chunk's own terminal
/// punctuation re-attached as the pause symbol the model was trained to end
/// an utterance on. Returns the token ids and the pre-tokenisation phoneme
/// count (the style row keys on it, before any substitution).
pub(crate) fn tokenize_for_kokoro_family(
    text: &str,
    vocab: &HashMap<String, i64>,
    space_id: i64,
    voice: &str,
) -> Result<(Vec<i64>, usize), TtsError> {
    let ipa = phonemes::phonemize(text, phonemes::espeak_voice_for_kokoro(voice))?;
    // The phonemiser echoes back what it cannot classify — hyphens and
    // apostrophes above all — and the reference tokenizer deletes those
    // upstream. Replace before counting, so the style row sees the same
    // length the vocab lookup does.
    let cleaned: String = ipa
        .chars()
        .map(|c| if c == '-' || c == '\'' { ' ' } else { c })
        .collect();
    let phoneme_chars = cleaned.chars().count();

    let mut dropped: Vec<char> = Vec::new();
    let mut tokens: Vec<i64> = Vec::with_capacity(phoneme_chars);
    for ch in cleaned.chars() {
        match vocab.get(ch.to_string().as_str()) {
            Some(id) => tokens.push(*id),
            None => {
                if !dropped.contains(&ch) {
                    dropped.push(ch);
                }
                tokens.push(space_id);
            }
        }
    }
    if !dropped.is_empty() {
        tracing::warn!(
            dropped = ?dropped,
            "phoneme(s) not in kokoro vocab, substituted with space"
        );
    }
    if tokens.len() <= 1 {
        return Err(TtsError::Synthesis(format!(
            "kokoro token count out of range: {} (phoneme chars {phoneme_chars})",
            tokens.len()
        )));
    }
    // The phonemiser's IPA drops punctuation, and the pause symbols are
    // how this model was trained to *end* an utterance: without one the
    // stream stops at the last phoneme mid-waveform and the chunk ends
    // with a chopped final phone — the click at chunk ends. Re-attach
    // the chunk's own terminal punctuation; an unpunctuated end (a
    // run-on cut, a heading) takes the long pause, the "to be
    // continued" prosody a cut boundary wants. Skipped on the one chunk
    // size that would overflow the graph's token cap with it.
    if tokens.len() < MAX_TOKENS {
        if let Some(id) = terminal_pause(text, vocab) {
            tokens.push(id);
        }
    }
    if tokens.len() > MAX_TOKENS {
        return Err(TtsError::Synthesis(format!(
            "kokoro token count out of range: {} (phoneme chars {phoneme_chars})",
            tokens.len()
        )));
    }
    Ok((tokens, phoneme_chars))
}

/// The pause token the chunk's own ending punctuation maps to, or the long
/// pause for an unpunctuated end. `None` only when the vocab lost a symbol
/// it is known to carry — then the chunk simply ends without a pause, as it
/// always has.
fn terminal_pause(text: &str, vocab: &HashMap<String, i64>) -> Option<i64> {
    let last = text.chars().rev().find(|c| !c.is_whitespace());
    let sym = match last {
        Some(c @ ('.' | '!' | '?')) => c.to_string(),
        Some(',') | Some(';') | Some(':') => ",".to_string(),
        _ => "…".to_string(),
    };
    vocab.get(&sym).copied()
}

/// Load a voice bank: a headerless plane of f32, 256-wide.
///
/// The plane is 510 rows for every voice but one — `af.bin` is 512 — so the
/// size check is a *floor* on whole rows, not an equality: a bank with more
/// rows than 510 is the published file, and the style-row lookup clamps to
/// whatever the bank actually holds. Anything that is not whole rows of 256
/// f32 is a corrupt download and fails now, at load — a bank of the wrong
/// width would either panic on the row slice or, worse, silently pick the
/// wrong speaker row.
fn load_voice(voices_dir: &Path, voice: &str) -> Result<Vec<f32>, TtsError> {
    let path = voices_dir.join(format!("{voice}.bin"));
    let bytes = std::fs::read(&path).map_err(|e| {
        TtsError::Unavailable {
            engine: "kokoro",
            detail: format!("voice bank {}: {e}", path.display()),
        }
    })?;
    let min_bank = 510 * STYLE_DIM * 4;
    if bytes.len() < min_bank || bytes.len() % (STYLE_DIM * 4) != 0 {
        return Err(TtsError::ModelLoad(format!(
            "{}: {} bytes, expected at least {} (510×256×f32, whole rows)",
            path.display(),
            bytes.len(),
            min_bank
        )));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

impl Engine for KokoroEngine {
    fn name(&self) -> &'static str {
        "kokoro"
    }

    fn synthesize(&self, text: &str) -> Result<Synthesis, TtsError> {
        self.synthesize_inner(text)
    }

    /// Same list the model manager's picker shows for this engine, so the
    /// two enumerations cannot disagree: `ModelManager::voices("kokoro")`
    /// and an engine asked for its own voices are one answer.
    fn voices(&self) -> Vec<Voice> {
        let mut names: Vec<String> = std::fs::read_dir(&self.voices_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.ends_with(".bin"))
                    .map(|n| n.trim_end_matches(".bin").to_string())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
            .into_iter()
            .map(|code| Voice { name: code.clone(), code, locale: None })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kokoro_dir() -> std::path::PathBuf {
        std::env::var_os("HOME")
            .map(|h| Path::new(&h).join(".local/share/speechnotes/models/kokoro"))
            .expect("HOME set")
    }

    fn engine() -> KokoroEngine {
        let dir = kokoro_dir();
        if !dir.join("model.onnx").exists() {
            panic!("kokoro model not installed");
        }
        KokoroEngine::load(&dir, "am_eric", 1.0).expect("load")
    }

    /// The pause the model needs at a chunk end comes from the chunk's own
    /// terminal punctuation; a cut or heading takes the long pause.
    #[test]
    fn terminal_pause_follows_the_chunks_own_punctuation() {
        let vocab = HashMap::from([
            (".".to_string(), 4),
            ("!".to_string(), 5),
            ("?".to_string(), 6),
            (",".to_string(), 3),
            ("…".to_string(), 10),
        ]);
        assert_eq!(terminal_pause("Stirred gently.", &vocab), Some(4));
        assert_eq!(terminal_pause("Any questions?", &vocab), Some(6));
        assert_eq!(terminal_pause("What a find!", &vocab), Some(5));
        assert_eq!(terminal_pause("First clause; second clause.", &vocab), Some(4));
        assert_eq!(terminal_pause("First clause, second clause", &vocab), Some(10));
        assert_eq!(terminal_pause("No punctuation at the end", &vocab), Some(10));
        assert_eq!(terminal_pause("   ", &vocab), Some(10));
    }

    #[test]
    fn kokoro_loads_and_synthesises_a_sentence() {
        let e = engine();
        let out = Engine::synthesize(&e, "Hello from the neural tier, much improved.").expect("synth");
        assert_eq!(out.sample_rate, 24_000);
        assert!(out.audio_time().as_secs_f64() > 1.0, "{:?}", out.audio_time());
        // RTF is not asserted under parallel test threads (three ONNX
        // sessions contending make wall clock noise); the sequential
        // `--speak` gate measures it.
    }

    #[test]
    fn kokoro_native_speed_shortens_proportionally() {
        let mut fast = engine();
        fast.set_rate(2.0);
        let slow = engine();
        let text = "The same sentence spoken twice, once quickly and once slowly, for the speed contract.";
        let slow_out = Engine::synthesize(&slow, text).expect("slow");
        let fast_out = Engine::synthesize(&fast, text).expect("fast");
        assert!(
            fast_out.audio_time().as_secs_f64() < slow_out.audio_time().as_secs_f64() * 0.65,
            "2× did not halve: slow {:?} fast {:?}",
            slow_out.audio_time(),
            fast_out.audio_time()
        );
    }

    #[test]
    fn a_voice_bank_of_the_wrong_size_fails_at_load() {
        let bad = std::env::temp_dir().join("sn-kokoro-test-bad-voice.bin");
        // Too small: a bank under 510 rows is a truncated download.
        std::fs::write(&bad, vec![0u8; 100]).unwrap();
        let err = load_voice(&std::env::temp_dir(), "sn-kokoro-test-bad-voice")
            .expect_err("a 100-byte bank must not load");
        assert!(err.to_string().contains("at least"), "{err}");
        let _ = std::fs::remove_file(&bad);

        // Not whole rows: a mid-row length is a corrupt file, whatever its size.
        let rows = 500 * STYLE_DIM * 4;
        std::fs::write(&bad, vec![0u8; rows + 1]).unwrap();
        assert!(
            load_voice(&std::env::temp_dir(), "sn-kokoro-test-bad-voice").is_err(),
            "a mid-row bank must not load"
        );
        let _ = std::fs::remove_file(&bad);
    }

    /// `af.bin` is the one published bank with 512 rows rather than 510; the
    /// loader takes the floor rather than refusing it, and the style lookup
    /// clamps to the bank's own row count. Without this, the picker offered
    /// a first voice that could not build.
    #[test]
    fn the_512_row_af_bank_loads_and_speaks() {
        let dir = kokoro_dir();
        if !dir.join("voices/af.bin").exists() {
            panic!("af.bin not installed; the catalog lists it");
        }
        let engine = KokoroEngine::load(&dir, "af", 1.0).expect("af.bin must load");
        let out = Engine::synthesize(&engine, "Af loads like every other voice.")
            .expect("af speaks");
        assert!(out.audio_time().as_secs_f64() > 0.2, "{:?}", out.audio_time());
    }
}
