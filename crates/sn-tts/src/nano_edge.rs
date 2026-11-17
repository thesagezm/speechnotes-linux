//! The nano-edge-tts engine: Kokoro-82M's text side with its 53.3M-parameter
//! waveform decoder replaced by drmhse's 1.75M-parameter harmonic-plus-noise
//! decoder ([drmhse/nano-edge-tts](https://huggingface.co/drmhse/nano-edge-tts),
//! Apache-2.0, distilled from Kokoro's own decoder — same voices, same
//! pronunciation, same 24 kHz). The published weights are PyTorch; the
//! catalog installs the ONNX export this repo produces from them, in the
//! same two-graph split the runtime runs:
//!
//! * `text-side.onnx` — input_ids [1, N], style [1, 256], speed [1] → the
//!   decoder inputs (asr [1, 512, T], F0 [1, T], N [1, T]) plus the style
//!   slice the decoder itself takes (s [1, 128]). The BOS/EOS wrap the
//!   reference `KModel.forward` applies lives inside the graph, so the ids
//!   fed here are the same unwrapped stream kokoro gets.
//! * `nano-decoder.onnx` — those four tensors → waveform [1, S].
//!
//! Two sessions instead of one because the decoder's interpolations size
//! themselves off input shapes: the export keeps them symbolic through the
//! dynamo path, and the handoff between the graphs is where the runtime
//! meets the frame count — which is also the cap the creak-noise window was
//! precomputed at.
//!
//! The voice banks, the tokenizer and the phoneme front end are kokoro's
//! own, shared through [`crate::kokoro::tokenize_for_kokoro_family`] — the
//! text side IS kokoro's, so the engines tokenize identically and a voice
//! code means the same speaker in both.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use sn_format::wav::f32_to_i16;

use crate::engine::{Engine, Synthesis, Voice};
use crate::kokoro::tokenize_for_kokoro_family;
use crate::ortrt::{self, Session};
use crate::TtsError;

/// The frame count the creak-noise window was precomputed at in the export
/// (16384 frames at 80 Hz — 204 s of audio, far past anything the app's
/// 510-token chunk cap can produce). The count is the F0 axis's — the text
/// side emits asr at half that rate, and the decoder's mapper aligns them.
/// A text side that predicts past it has nothing to draw the jitter from;
/// erroring the chunk beats reading the window off its end.
const MAX_FRAMES: usize = 16_384;

/// Style-dim: every voice bank row is 256 floats — 128 for the decoder, the
/// rest for the prosody predictor. The graph does the split.
const REF_STYLE_DIM: usize = 256;

pub struct NanoEdgeEngine {
    text: Session,
    decoder: Session,
    vocab: HashMap<String, i64>,
    space_id: i64,
    voices_dir: std::path::PathBuf,
    voice: String,
    /// The loaded voice bank, rows of [`REF_STYLE_DIM`] floats. Loaded with
    /// the engine so a missing `.bin` is a load error, not a mid-sentence one.
    voice_flat: Vec<f32>,
    rate_multiplier: f64,
}

impl NanoEdgeEngine {
    /// Load from the nano-edge-tts model directory (`text-side.onnx`,
    /// `nano-decoder.onnx`, `tokenizer.json`, `voices/<voice>.bin`).
    pub fn load(model_dir: &Path, voice: &str, rate_multiplier: f64) -> Result<Self, TtsError> {
        let text_graph = model_dir.join("text-side.onnx");
        let decoder_graph = model_dir.join("nano-decoder.onnx");
        for graph in [&text_graph, &decoder_graph] {
            if !graph.exists() {
                return Err(TtsError::Unavailable {
                    engine: "nano-edge-tts",
                    detail: format!("{} not found", graph.display()),
                });
            }
        }
        let text = Session::load(&text_graph)?;
        let decoder = Session::load(&decoder_graph)?;

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
        let space_id = vocab.get(" ").copied().unwrap_or(16);

        let voices_dir = model_dir.join("voices");
        let voice_flat = load_voice(&voices_dir, voice)?;

        tracing::info!(
            vocab = vocab.len(),
            voice = %voice,
            "nano-edge-tts loaded"
        );
        Ok(Self {
            text,
            decoder,
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

    fn synthesize_inner(&self, chunk: &str) -> Result<Synthesis, TtsError> {
        let started = Instant::now();
        let (tokens, phoneme_chars) =
            tokenize_for_kokoro_family(chunk, &self.vocab, self.space_id, &self.voice)?;

        // The full 256-wide style row, by phoneme length — the reference
        // pipeline's ref_s; the text-side graph slices the decoder's half.
        let rows = self.voice_flat.len() / REF_STYLE_DIM;
        let adjusted = (phoneme_chars.saturating_sub(1)).min(rows.saturating_sub(1));
        let offset = adjusted * REF_STYLE_DIM;
        let style: Vec<f32> = self.voice_flat[offset..offset + REF_STYLE_DIM].to_vec();

        let speed = self.rate_multiplier as f32;
        let inputs = vec![
            (
                "input_ids",
                ortrt::tensor_i64(&[1, tokens.len() as i64], tokens.clone())?,
            ),
            ("style", ortrt::tensor_f32(&[1, 256], style)?),
            ("speed", ortrt::tensor_f32(&[1], vec![speed])?),
        ];
        let taken = self
            .text
            .run_f32_shaped(inputs, &["asr", "f0", "n", "s"])?;
        let (asr_shape, asr) = &taken[0];
        let asr_frames = *asr_shape.get(2).ok_or_else(|| {
            TtsError::Synthesis("nano-edge-tts text side gave no frame count".to_string())
        })? as usize;
        let (_, f0) = &taken[1];
        let (_, n) = &taken[2];
        let (_, s) = &taken[3];
        if f0.len() != n.len() {
            return Err(TtsError::Synthesis(format!(
                "nano-edge-tts text side: f0 {} vs n {} frames",
                f0.len(),
                n.len()
            )));
        }
        // The text side emits asr at half the F0 frame count (the duration
        // expansion keys the prosody predictor at 80 Hz, the text encoder at
        // 40); the decoder's mapper upsamples asr internally, so each tensor
        // feeds at its own length. The creak-noise window is precomputed at
        // MAX_FRAMES against THIS axis (F0, the 80 Hz one).
        let f0_frames = f0.len();
        if f0_frames > MAX_FRAMES {
            return Err(TtsError::Synthesis(format!(
                "nano-edge-tts chunk predicts {f0_frames} frames, past the {MAX_FRAMES}-frame window"
            )));
        }

        let wave = self.decoder.run_f32(
            vec![
                ("asr", ortrt::tensor_f32(&[1, 512, asr_frames as i64], asr.clone())?),
                ("f0", ortrt::tensor_f32(&[1, f0_frames as i64], f0.clone())?),
                ("n", ortrt::tensor_f32(&[1, f0_frames as i64], n.clone())?),
                ("s", ortrt::tensor_f32(&[1, 128], s.clone())?),
            ],
            &["waveform"],
        )?;
        let samples = wave.into_iter().next().unwrap_or_default();
        if samples.is_empty() {
            return Err(TtsError::Synthesis(
                "nano-edge-tts produced no samples for speakable text".to_string(),
            ));
        }
        let mono: Vec<i16> = samples.iter().copied().map(f32_to_i16).collect();
        tracing::debug!(
            tokens = tokens.len(),
            audio_s = mono.len() as f64 / 24_000.0,
            synth_ms = started.elapsed().as_millis() as u64,
            "nano-edge-tts chunk synthesised"
        );
        let mut syn = Synthesis {
            sample_rate: 24_000,
            samples: mono,
            synth_time: started.elapsed(),
        };
        // The chunk ends the way kokoro's do — the fade contract is shared.
        syn.fade_edges();
        Ok(syn)
    }
}

/// Load a voice bank: a headerless plane of f32, 256-wide — the same
/// published banks the kokoro set ships, so one voice code is one speaker
/// in both engines. Whole rows only, and at least one row: a truncated
/// download fails here, at load.
fn load_voice(voices_dir: &Path, voice: &str) -> Result<Vec<f32>, TtsError> {
    let path = voices_dir.join(format!("{voice}.bin"));
    let bytes = std::fs::read(&path).map_err(|e| TtsError::Unavailable {
        engine: "nano-edge-tts",
        detail: format!("voice bank {}: {e}", path.display()),
    })?;
    if bytes.is_empty() || bytes.len() % (REF_STYLE_DIM * 4) != 0 {
        return Err(TtsError::ModelLoad(format!(
            "{}: {} bytes, not whole rows of {REF_STYLE_DIM} f32",
            path.display(),
            bytes.len()
        )));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

impl Engine for NanoEdgeEngine {
    fn name(&self) -> &'static str {
        "nano-edge-tts"
    }

    fn synthesize(&self, text: &str) -> Result<Synthesis, TtsError> {
        self.synthesize_inner(text)
    }

    /// The bank files beside the graphs — the same enumeration the model
    /// manager's picker shows for this engine.
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

    fn nano_dir() -> std::path::PathBuf {
        crate::manager::ModelManager::set_for("nano-edge-tts")
            .expect("nano-edge-tts in the catalog")
            .path()
    }

    fn installed() -> bool {
        let dir = nano_dir();
        dir.join("text-side.onnx").exists() && dir.join("nano-decoder.onnx").exists()
    }

    /// The engine loads and speaks, and the graph chain accepts the same
    /// shape family the export verified: a sentence, not a fixed size.
    #[test]
    fn nano_loads_and_synthesises_a_sentence() {
        if !installed() {
            panic!("nano-edge-tts not installed; the catalog lists it");
        }
        let engine = NanoEdgeEngine::load(&nano_dir(), "af_heart", 1.0).expect("load");
        let out = Engine::synthesize(&engine, "Hello from the nano tier.").expect("synth");
        assert_eq!(out.sample_rate, 24_000);
        assert!(out.audio_time().as_secs_f64() > 0.5, "{:?}", out.audio_time());
    }

    /// Two lengths through the same sessions: the dynamic frame axis the
    /// export kept symbolic must hold at runtime, not just at the probe.
    #[test]
    fn nano_synthesises_short_and_long_chunks_through_the_same_sessions() {
        if !installed() {
            panic!("nano-edge-tts not installed; the catalog lists it");
        }
        let engine = NanoEdgeEngine::load(&nano_dir(), "am_michael", 1.0).expect("load");
        let short = Engine::synthesize(&engine, "Short one.").expect("short");
        let long = Engine::synthesize(
            &engine,
            "The decoder that made this has under two million parameters, and the text \
             side it borrows is the one the app already runs, so a long sentence and a \
             short one travel the same two graphs with the frame count as the only \
             thing that moves between them.",
        )
        .expect("long");
        assert!(long.audio_time() > short.audio_time());
    }

    /// A truncated voice bank fails at load, never mid-sentence.
    #[test]
    fn a_voice_bank_of_the_wrong_shape_fails_at_load() {
        let bad = std::env::temp_dir().join("sn-nano-test-bad-voice.bin");
        std::fs::write(&bad, vec![0u8; 100]).unwrap();
        assert!(
            load_voice(&std::env::temp_dir(), "sn-nano-test-bad-voice").is_err(),
            "a mid-row bank must not load"
        );
        let _ = std::fs::remove_file(&bad);
    }
}
