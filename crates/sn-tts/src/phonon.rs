//! The Phonon engine: Gradium's open on-device TTS (`gradium/phonon-31m-en`,
//! Apache-2.0) behind the `ptts` crate, running on the CPU from the model
//! manager's checkpoint directory.
//!
//!  authored 2026-11-10 (new engine, user ask). The model is a
//! continuous audio LM, not an ONNX graph: `Checkpoint::locate` resolves the
//! local folder the manager downloaded (config + `model.q8.gguf` + tokenizer +
//! voices), and the loader's own rules pick the voices up — every
//! `voices/*.safetensors` registers under its file stem (the catalog's
//! `voice_id`) and the model's bundled `default-voice.safetensors` registers
//! as `"default"` — so this file never parses the voices in, it only
//! enumerates them for the picker (`catalog_voices`, shared with
//! `ModelManager::voices` so the two lists cannot disagree).
//!
//! Deliberate limitation: the model has no native speed control (no speed
//! input anywhere in `ptts::synth`), so [`Engine::rate`] stays 1.0 and the
//! rate slider is a no-op for this engine — the trait's documented contract
//! for engines that cannot change it. A rate change still rebuilds the
//! resident engine (the reuse key includes the rate); harmless, just useless.
//!
//! Output is mono f32 at the engine's own sample rate (24 kHz for the 31M
//! model); converted to the pipeline's i16 currency at the boundary, same as
//! kokoro. Language is pinned to `Lang::En` — the 31M checkpoint is the
//! English model; the multilingual 40M sets are gated downloads and are not
//! in the catalog.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ptts::checkpoint::{Checkpoint, ResolveOptions};
use ptts::preprocess::Lang;
use ptts::synth::{DeviceKind, Quant, SpeechOptions, Synth};

use sn_format::wav::f32_to_i16;

use crate::engine::{Engine, Synthesis, Voice};
use crate::TtsError;

/// The quantization the CPU path runs. `Q80` resolves `model.q8.gguf` first
/// and falls back to `model.safetensors`; the catalog ships the GGUF (the
/// fp32 safetensors file is deliberately not downloaded — 103 MB vs 64 MB
/// for the same audio).
const QUANT: Quant = Quant::Q80;

pub struct PhononEngine {
    synth: Synth,
    voice: String,
    /// Kept only so [`Engine::voices`] can enumerate from the same directory
    /// the engine loaded from.
    model_dir: PathBuf,
}

impl PhononEngine {
    /// Load from the phonon checkpoint directory (`config.json`,
    /// `model.q8.gguf`, `tokenizer.json`, `default-voice.safetensors`,
    /// `voices.jsonl`, `voices/*.safetensors`).
    ///
    /// An empty `voice` resolves to the model's default; an unknown one is a
    /// load error naming the voice, like every other engine — a bad voice
    /// code must fail loudly at session start, not silently fall back to
    /// something the user did not pick.
    pub fn load(model_dir: &Path, voice: &str) -> Result<Self, TtsError> {
        let config = model_dir.join("config.json");
        if !config.exists() {
            return Err(TtsError::Unavailable {
                engine: "phonon",
                detail: format!("{} not found", config.display()),
            });
        }
        let started = Instant::now();
        // `locate` takes the path as a string (it dispatches on `hf://…`
        // schemes); a plain directory path falls through to the local-folder
        // resolution.
        let checkpoint =
            Checkpoint::locate(&model_dir.to_string_lossy(), ResolveOptions { quant: QUANT, weights: None })
                .map_err(|e| TtsError::ModelLoad(format!("phonon checkpoint: {e}")))?;
        let mut synth = checkpoint
            .builder(Lang::En)
            .device(DeviceKind::Cpu)
            .build()
            .map_err(|e| TtsError::ModelLoad(format!("phonon build: {e}")))?;
        checkpoint.register_voices(&mut synth);

        let known = synth.voices();
        let voice = if voice.is_empty() {
            synth
                .default_voice()
                .or_else(|| known.first().cloned())
                .ok_or_else(|| TtsError::ModelLoad("phonon checkpoint registered no voices".into()))?
        } else if known.iter().any(|v| v == voice) {
            voice.to_string()
        } else {
            return Err(TtsError::Unavailable {
                engine: "phonon",
                detail: format!(
                    "voice {voice:?} is not registered ({} known, e.g. {:?})",
                    known.len(),
                    known.first().cloned().unwrap_or_default()
                ),
            });
        };

        tracing::info!(
            voices = known.len(),
            voice = %voice,
            device = %synth.device_name(),
            sample_rate = synth.sample_rate(),
            load_ms = started.elapsed().as_millis() as u64,
            "phonon loaded"
        );
        Ok(Self {
            synth,
            voice,
            model_dir: model_dir.to_path_buf(),
        })
    }

    /// The voice this engine speaks with — the code prefs store, and the
    /// `voice_id` the loader registered.
    pub fn voice(&self) -> &str {
        &self.voice
    }

    fn synthesize_inner(&self, text: &str) -> Result<Synthesis, TtsError> {
        let started = Instant::now();
        let opts = SpeechOptions::default().voice(self.voice.clone());
        let pcm = self
            .synth
            .say_with(text, &opts)
            .map_err(|e| TtsError::Synthesis(format!("phonon: {e}")))?;
        if pcm.is_empty() {
            return Err(TtsError::Synthesis(
                "phonon produced no samples for speakable text".to_string(),
            ));
        }
        let samples: Vec<i16> = pcm.iter().copied().map(f32_to_i16).collect();
        tracing::debug!(
            chars = text.chars().count(),
            audio_s = samples.len() as f64 / self.synth.sample_rate() as f64,
            synth_ms = started.elapsed().as_millis() as u64,
            "phonon chunk synthesised"
        );
        Ok(Synthesis {
            sample_rate: self.synth.sample_rate(),
            samples,
            synth_time: started.elapsed(),
        })
    }
}

impl Engine for PhononEngine {
    fn name(&self) -> &'static str {
        "phonon"
    }

    fn synthesize(&self, text: &str) -> Result<Synthesis, TtsError> {
        self.synthesize_inner(text)
    }

    /// The same list the model manager's picker shows for this engine, from
    /// the same directory: `ModelManager::voices("phonon")` and this are one
    /// answer.
    fn voices(&self) -> Vec<Voice> {
        catalog_voices(&self.model_dir)
    }
}

/// The voices a phonon checkpoint offers, read off the disk without loading
/// the model. The `voices.jsonl` catalog (downloaded beside the model from
/// `gradium/phonon-voices`) carries the human metadata; the codes are the
/// loader's own registered names, so what the picker writes into prefs is
/// exactly what [`PhononEngine::load`] resolves.
///
/// Without a readable catalog the fallback is the raw file stems of
/// `voices/*.safetensors` plus `default` — a picker must list something a
/// load will accept, and the stems ARE the registered names. `default`
/// always sorts last so the bundled voice is a deliberate choice, not a
/// first row by accident.
pub(crate) fn catalog_voices(dir: &Path) -> Vec<Voice> {
    let mut out: Vec<Voice> = Vec::new();
    let catalog = std::fs::read_to_string(dir.join("voices.jsonl")).ok();
    match catalog {
        Some(raw) => {
            for line in raw.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                // filename "voices/<id>.safetensors" → the stem the loader
                // registers; fall back to voice_id, then name.
                let code = v["filename"]
                    .as_str()
                    .and_then(|f| Path::new(f).file_stem())
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
                    .or_else(|| v["voice_id"].as_str().map(str::to_string));
                let Some(code) = code else { continue };
                let name = voice_display_name(&v, &code);
                // The catalog's own locale — "en-us", or the literal
                // "characters" for the twelve character voices. The stem
                // fallback below has no catalog, so it has no locale.
                let locale = v["locale"]
                    .as_str()
                    .map(str::to_string);
                out.push(Voice { code, name, locale });
            }
        }
        None => {
            // The catalog file is missing: the stems are the truth.
            let mut codes: Vec<String> = std::fs::read_dir(dir.join("voices"))
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .filter_map(|e| e.file_name().into_string().ok())
                        .filter(|n| n.ends_with(".safetensors"))
                        .map(|n| n.trim_end_matches(".safetensors").to_string())
                        .collect()
                })
                .unwrap_or_default();
            codes.sort();
            out.extend(codes.into_iter().map(|code| Voice {
                name: code.clone(),
                code,
                locale: None,
            }));
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    if dir.join("default-voice.safetensors").is_file() {
        out.push(Voice {
            code: "default".to_string(),
            name: "Default".to_string(),
            locale: None,
        });
    }
    out
}

/// The picker's row for a catalog voice: the human name, with the locale and
/// gender when the catalog has them. The twelve "characters" voices (an ogre,
/// a dragon, a fairy godmother…) carry `locale: "characters"` and no gender —
/// shown by name alone rather than a nonsensical "(characters)" tag.
fn voice_display_name(v: &serde_json::Value, code: &str) -> String {
    let name = v["name"].as_str().unwrap_or(code);
    let mut parts: Vec<&str> = Vec::new();
    match v["locale"].as_str() {
        Some(locale) if locale != "characters" => parts.push(locale),
        _ => {}
    }
    if let Some(gender) = v["gender"].as_str() {
        parts.push(gender);
    }
    if parts.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({})", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phonon_dir() -> PathBuf {
        std::env::var_os("HOME")
            .map(|h| Path::new(&h).join(".local/share/speechnotes/models/phonon"))
            .expect("HOME set")
    }

    fn engine() -> PhononEngine {
        let dir = phonon_dir();
        if !dir.join("config.json").exists() {
            panic!("phonon model not installed");
        }
        PhononEngine::load(&dir, "").expect("load")
    }

    #[test]
    fn phonon_loads_and_synthesises_a_sentence() {
        let e = engine();
        let out = Engine::synthesize(&e, "Hello from the phonon engine, speaking from a small model.")
            .expect("synth");
        assert_eq!(out.sample_rate, 24_000);
        assert!(out.audio_time().as_secs_f64() > 1.0, "{:?}", out.audio_time());
    }

    #[test]
    fn an_empty_voice_resolves_to_default() {
        let e = engine();
        assert_eq!(e.voice(), "default");
    }

    #[test]
    fn an_unknown_voice_fails_at_load_naming_the_voice() {
        let dir = phonon_dir();
        if !dir.join("config.json").exists() {
            panic!("phonon model not installed");
        }
        let Err(err) = PhononEngine::load(&dir, "not_a_voice") else {
            panic!("a voice with no embedding must not load");
        };
        let text = err.to_string();
        assert!(text.contains("not_a_voice"), "the voice must be named: {text}");
    }

    #[test]
    fn catalog_voices_lists_default_and_the_pack() {
        let dir = phonon_dir();
        if !dir.join("config.json").exists() {
            panic!("phonon model not installed");
        }
        let voices = catalog_voices(&dir);
        assert!(voices.len() > 100, "the pack has 102 voices plus default, got {}", voices.len());
        let last = voices.last().expect("non-empty");
        assert_eq!(last.code, "default", "default must sort last");
        // The engine's own enumeration and the manager's must agree.
        let e = engine();
        let from_engine: Vec<String> = Engine::voices(&e).into_iter().map(|v| v.code).collect();
        let from_catalog: Vec<String> = voices.into_iter().map(|v| v.code).collect();
        assert_eq!(from_engine, from_catalog);
    }

    #[test]
    fn a_catalog_voice_loads_by_its_code() {
        let dir = phonon_dir();
        if !dir.join("voices.jsonl").exists() {
            panic!("phonon voice pack not installed");
        }
        let voices = catalog_voices(&dir);
        let pick = voices
            .iter()
            .find(|v| v.code != "default")
            .expect("the pack has non-default voices")
            .code
            .clone();
        let e = PhononEngine::load(&dir, &pick).expect("load");
        assert_eq!(e.voice(), pick);
        let out = Engine::synthesize(&e, "A pack voice speaks a whole sentence.").expect("synth");
        assert!(out.audio_time().as_secs_f64() > 1.0, "{:?}", out.audio_time());
    }

    #[test]
    fn a_checkpoint_without_config_names_the_missing_file() {
        let empty = std::env::temp_dir().join("sn-phonon-test-empty-dir");
        let _ = std::fs::remove_dir_all(&empty);
        std::fs::create_dir_all(&empty).unwrap();
        let Err(err) = PhononEngine::load(&empty, "") else {
            panic!("an empty dir must not load");
        };
        assert!(err.to_string().contains("config.json"), "{err}");
        let _ = std::fs::remove_dir_all(&empty);
    }
}
