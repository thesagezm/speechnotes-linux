//! espeak-ng IPA phonemisation — the G2P the neural engines share.
//!
//! The kokoro-onnx reference ships an espeak fallback, so one CLI covers
//! the neural engines' front ends: `espeak-ng -q --ipa` per chunk. Coverage
//! is verified, not assumed — see the tests, which run the real binary
//! against the real vocab map.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::TtsError;

/// Phonemise one chunk with espeak-ng's IPA output.
///
/// `voice` is an espeak voice code (`en-us`, `en`, `de`, …); the engines
/// derive it from their own voice naming (Kokoro's `am_eric` → `en-us`,
/// British `bf_alice` → `en`).
pub fn phonemize(text: &str, voice: &str) -> Result<String, TtsError> {
    if text.trim().is_empty() {
        return Err(TtsError::Empty);
    }
    let mut child = Command::new("espeak-ng")
        .args(["-q", "--ipa", "--stdin", "-v", voice])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| TtsError::Unavailable {
            engine: "espeak-ng",
            detail: format!("phonemiser could not spawn: {e}"),
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(TtsError::Synthesis(format!(
            "espeak-ng --ipa exited with {}",
            output.status
        )));
    }
    let ipa = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ipa.is_empty() {
        return Err(TtsError::Synthesis(
            "phonemiser produced nothing for speakable text".to_string(),
        ));
    }
    Ok(ipa)
}

/// Map a Kokoro voice name to the espeak voice that phonemises its language.
///
/// Kokoro's voice names are `<lang><gender>_<name>`: `a`merican, `b`ritish,
/// `e` spanish, `f` french, `h`indi, `i`talian, `j`apanese, `p`ortuguese,
/// `z`hinese. Unknown prefixes default to American English — a new voice
/// pack must not make the engine unusable, and English is this app's
/// centre of gravity.
pub fn espeak_voice_for_kokoro(voice: &str) -> &'static str {
    match voice.chars().next() {
        Some('b') => "en",
        Some('e') => "es",
        Some('f') => "fr",
        Some('h') => "hi",
        Some('i') => "it",
        Some('j') => "ja",
        Some('p') => "pt",
        Some('z') => "cmn",
        _ => "en-us",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phonemises_english_prose_into_ipa() {
        let ipa = phonemize("The quick brown fox.", "en-us").expect("phonemise");
        assert!(ipa.contains('ˈ') || ipa.contains('ð'), "not IPA: {ipa}");
        assert!(!ipa.contains("The"), "verbatim text leaked through: {ipa}");
    }

    #[test]
    fn every_phonemised_char_lands_in_kokoros_vocab() {
        let ipa = phonemize(
            "Voice quality, priced fairly, brought forty jets to the quay.",
            "en-us",
        )
        .expect("phonemise");
        let vocab = kokoro_test_vocab();
        let missing: Vec<char> = ipa
            .chars()
            .filter(|c| *c != '\n' && !vocab.contains(c))
            .collect();
        assert!(missing.is_empty(), "chars outside kokoro's vocab: {missing:?}");
    }

    #[test]
    fn empty_text_is_rejected() {
        assert!(matches!(phonemize("  ", "en-us"), Err(TtsError::Empty)));
    }

    #[test]
    fn kokoro_voice_prefixes_map_to_espeak_voices() {
        assert_eq!(espeak_voice_for_kokoro("am_eric"), "en-us");
        assert_eq!(espeak_voice_for_kokoro("bf_alice"), "en");
        assert_eq!(espeak_voice_for_kokoro("jf_tebukuro"), "ja");
        assert_eq!(espeak_voice_for_kokoro("zm_yunjian"), "cmn");
        assert_eq!(espeak_voice_for_kokoro("unknown"), "en-us");
    }

    /// The real kokoro vocab keys, if the model is installed.
    fn kokoro_test_vocab() -> std::collections::HashSet<char> {
        let path = kokoro_model_dir().join("tokenizer.json");
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => return test_fallback(),
        };
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["model"]["vocab"]
            .as_object()
            .expect("vocab")
            .keys()
            .filter_map(|k| k.chars().next())
            .collect()
    }

    fn kokoro_model_dir() -> std::path::PathBuf {
        dirs_home().join(".local/share/speechnotes/models/kokoro")
    }

    fn dirs_home() -> std::path::PathBuf {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default()
    }

    /// Without the models, the coverage tests would silently pass on an
    /// empty map — fail loudly instead. (The models are part of this box's
    /// contract; a CI box without them should install them.)
    fn test_fallback() -> std::collections::HashSet<char> {
        panic!("models not installed; coverage cannot be checked")
    }
}
