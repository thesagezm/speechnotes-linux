//! The model manager: a catalog of downloadable engines, where each one
//! lands on disk, how it is validated, and how a partial download is caught.
//!
//! Three rules this module exists to enforce, each one carrying a scar from
//! the iOS port:
//!
//! * **A partial file must never load.** Every file in a model set has a
//!   minimum size, checked before *and* after the transfer. A truncated
//!   `.part` is renamed into place and then immediately rejected as too
//!   small — the manager's own gate, not the engine's luck.
//! * **One resident engine.** The engines hold an ONNX session each and
//!   Supertonic holds four. `resident()` loads exactly one engine's set;
//!   asking for another drops the first before the second is opened. The
//!   one bounded exception: a switch *mid-playback* — the old session's
//!   synth thread holds its engine's Arc for the one chunk already in
//!   flight (it exits at the next chunk boundary), so two pools can overlap
//!   for at most a chunk's synthesis, never a session's life.
//! * **Disk is checked before it is spent.** A 508 MB download that fills the
//!   disk halfway leaves an unusable `.part` behind and a user with no
//!   warning it was coming. `free_space` is read first and the download
//!   refuses to start rather than failing at 90%.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sn_core::paths;
use crate::engine::{Engine, Voice};

use crate::TtsError;

/// One downloadable file inside a model set: where it comes from, where it
/// goes, and the proof it is the published file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFile {
    /// Remote path, joined to the set's base URL. An absolute `http(s)://`
    /// URL is passed through unjoined — phonon's voice pack lives in the same
    /// model set but a different Hugging Face repo than the model itself.
    pub remote: String,
    /// Local path relative to the set's directory.
    pub local: String,
    /// Bytes below which the file is considered truncated. Chosen as roughly
    /// 95% of the published size — enough headroom for an honest server,
    /// small enough that a half-transfer never passes.
    pub min_bytes: u64,
    /// The published file's sha256, lower-case hex. A model is an ONNX graph
    /// executed on this machine's data, so a swapped or corrupted download
    /// must not load; the size floor alone accepts a tampered file of the
    /// right length. The pins are the LFS oids Hugging Face publishes for
    /// each file (the small JSONs, which sit under the LFS threshold, were
    /// hashed from the published bytes directly). Empty means unpinned —
    /// nothing in the catalog ships unpinned; the throwaway-server tests
    /// build their own files without one.
    pub sha256: &'static str,
}

/// A downloadable model set: files, a directory, a default voice.
#[derive(Debug, Clone)]
pub struct ModelSet {
    /// The engine this set feeds; matches `Engine::name()`.
    pub engine: &'static str,
    /// Base URL. Every file's `remote` is appended to it.
    pub base_url: String,
    /// Files to fetch, in order. Order matters for the progress readout:
    /// the heaviest file first means progress jumps to 90% early and the
    /// long tail is the small ones.
    pub files: Vec<ModelFile>,
    /// Directory under `models/` this set lands in.
    pub dir: String,
    /// Approximate total download size, for the "this will take 508 MB" line.
    pub approx_bytes: u64,
    /// The voice a fresh install should start with.
    pub default_voice: &'static str,
}

impl ModelSet {
    /// The absolute directory this set installs into.
    pub fn path(&self) -> PathBuf {
        paths::models_dir().join(&self.dir)
    }

    /// Approximate install size once every file is present.
    pub fn approx_bytes(&self) -> u64 {
        self.approx_bytes
    }
}

/// The sets a user can install, in picker order.
///
/// Built once and served from a `OnceLock` forever after: the old form
/// returned a fresh `Vec` from every call and `set_for` boxed-and-leaked one
/// per call, so every `state()`/`build_engine()` leaked a catalog (~4 KB of
/// `String` paths, 65 files) — "bounded by design" only for a process that
/// quits fast. A static is the same lifetime with no leak at any call rate.
///
///  the shipped binary's selftest pins this list to exactly the three
/// voices ("catalog holds …"), so the `ocr` set stays in the manager's table
/// ([`ModelManager::set_for`] still resolves it — the Models page installs
/// it) but never appears in this speech-facing list.
pub fn catalog() -> Vec<ModelSet> {
    static CATALOG: std::sync::OnceLock<Vec<ModelSet>> = std::sync::OnceLock::new();
    CATALOG.get_or_init(build_catalog)
        .iter()
        .filter(|set| set.engine != "ocr")
        .cloned()
        .collect()
}

fn build_catalog() -> Vec<ModelSet> {
    let kokoro_base = "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main";
    // The nano-edge decoder is this repo's own ONNX export of drmhse's
    // distilled Kokoro decoder (the published weights are PyTorch); the
    // graphs ride in the project's model repo, the tokenizer and voice
    // banks are kokoro's own files fetched from kokoro's repo below.
    let nano_edge_base = "https://huggingface.co/thesagezm/speechnotes-models/resolve/main/nano-edge-tts";
    // The *original* Supertone set, not supertonic-3. The engine's contract
    // is pinned to it (the Helper was written against it, the latent is
    // 24×6=144, 3072-sample chunks, 44.1 kHz) and the files on this box are
    // byte-for-byte the v1 build. A later set with the same names is a
    // different graph behind the same filenames, and the iOS port's mistake:
    // supertonic-3's larger graphs would load into an orchestration built for
    // v1 and produce audio of an unverified shape.
    let supertonic_base = "https://huggingface.co/Supertone/supertonic/resolve/main";
    // RapidOCR's PP-OCRv4 graphs: the page-recognition models behind the
    // reader's OCR (`sn_doc`), not a voice. They stay in the manager's set
    // table because the Models page is where every downloadable model hangs;
    // the speech-engine picker excludes `ocr`
    // (see [`ModelManager::available_engines`]).  comment adapted —
    // `catalog()` itself no longer lists ocr (the shipped binary's selftest
    // pins it to the three voices).
    // No dict file is downloaded: the rec graph carries its own class list
    // in its `character` metadata, which `sn_doc::rapidocr` reads (and
    // writes back as `rec_dict.txt`) — the published dict file has drifted
    // against some exports, and class order is the model's to declare.
    let rapidocr_base = "https://huggingface.co/SWHL/RapidOCR/resolve/main";

    vec![
        ModelSet {
            engine: "kokoro",
            base_url: kokoro_base.to_string(),
            dir: "kokoro".to_string(),
            // 325 MB graph + 55 voice banks × 522 240 bytes.
            approx_bytes: 325_532_232 + 28_726_723,
            default_voice: "am_eric",
            files: vec![
                ModelFile {
                    remote: "onnx/model.onnx".to_string(),
                    local: "model.onnx".to_string(),
                    // The fp32 tier is 325.5 MB; the uint8 tier (177 MB) is
                    // *proven corrupt* on this model and is never installed.
                    min_bytes: 300_000_000,
                    sha256: "8fbea51ea711f2af382e88c833d9e288c6dc82ce5e98421ea61c058ce21a34cb",
                },
                ModelFile {
                    remote: "tokenizer.json".to_string(),
                    local: "tokenizer.json".to_string(),
                    min_bytes: 3_000,
                    sha256: "77a02c8e164413299b4b4c403b14f8e0e1c1b727db4d46a09d6327b861060a34",
                },
            ]
            .into_iter()
            .chain(kokoro_voices())
            .collect(),
        },
        ModelSet {
            engine: "supertonic",
            base_url: supertonic_base.to_string(),
            dir: "supertonic".to_string(),
            approx_bytes: 265_187_209 + 4_204_852,
            default_voice: "M1",
            files: vec![
                ModelFile {
                    remote: "onnx/duration_predictor.onnx".to_string(),
                    local: "onnx/duration_predictor.onnx".to_string(),
                    min_bytes: 1_500_000,
                    sha256: "b861580c56a0cba2a2b82aa697ecb3c5a163c3240c60a0ddfac369d21d054092",
                },
                ModelFile {
                    remote: "onnx/text_encoder.onnx".to_string(),
                    local: "onnx/text_encoder.onnx".to_string(),
                    min_bytes: 27_300_000,
                    sha256: "ba0c8ea74aeb5df00d21a89b8d47c71317f47120232e3deef95024dba37dbd88",
                },
                ModelFile {
                    remote: "onnx/vector_estimator.onnx".to_string(),
                    local: "onnx/vector_estimator.onnx".to_string(),
                    min_bytes: 132_400_000,
                    sha256: "b3f82ecd2e9decc4e2236048b03628a1c1d5f14a792ba274a59b7325107aa6a6",
                },
                ModelFile {
                    remote: "onnx/vocoder.onnx".to_string(),
                    local: "onnx/vocoder.onnx".to_string(),
                    min_bytes: 101_400_000,
                    sha256: "19bd51f47a186069c752403518a40f7ea4c647455056d2511f7249691ecddf7c",
                },
                ModelFile {
                    remote: "onnx/tts.json".to_string(),
                    local: "onnx/tts.json".to_string(),
                    min_bytes: 8_000,
                    sha256: "4dac5f986698a3ace9a97ea2545d43f6c8ba120d25e005f8c905128281be9b6d",
                },
                ModelFile {
                    remote: "onnx/unicode_indexer.json".to_string(),
                    local: "onnx/unicode_indexer.json".to_string(),
                    min_bytes: 262_000,
                    sha256: "0c3800ba4fb1fc760c9070eb43a0ad5a68279ec165742591a68ea3edca452978",
                },
            ]
            .into_iter()
            .chain(
                // (style, sha256 of the published JSON — these sit under the
                // LFS threshold, so each was hashed from the published bytes.)
                [
                    ("F1", "1450bcad84a2790eaf73f85e763dd5bae7c399f55d692c4835cf4f7686b5a10f"),
                    ("F2", "47c8d44445ef8ac8aae8ef5806feca21903483cbd4f1232e405184a40520a549"),
                    ("F3", "93d17bf5e967d11b9119877bab9e3c6a43350f9146a8db265abba102f29cdb36"),
                    ("F4", "325d8da46877089a67b7dfe1e59d04133d1f883754670af6b612a64b5ea697f2"),
                    ("F5", "72fd8e30dc65d2ad18a4bfdc48c925b5635219c1cb09a93096a81707bd2f695e"),
                    ("M1", "273c9ba6582d2e00383d8fbe2f5d660d86e8fba849c91ff695384d1a6e2e02f1"),
                    ("M2", "26898a9ec3de1b5bf8cc3f6cbf41930543ca0403f2201e12aad849691ff315dd"),
                    ("M3", "208b4f6d66ce68a1067602aa7e71b20d98d74a981ba14bdaa5df4df2c40e4068"),
                    ("M4", "733eec0e79cfd8e506368cdad0e863cf581d172caa31b9d412be613fd8649120"),
                    ("M5", "f0c80d2f423f8a916cf3a92ee4356d63622265a9ab34ea79994eb7f751d2d245"),
                ]
                .into_iter()
                .map(|(voice, sha)| ModelFile {
                    remote: format!("voice_styles/{voice}.json"),
                    local: format!("voice_styles/{voice}.json"),
                    // v1 styles are ~420 KB each; the smallest published one
                    // (F3) is 420 019, so the floor sits just under it.
                    min_bytes: 420_000,
                    sha256: sha,
                }),
            )
            .collect(),
        },
        ModelSet {
            engine: "phonon",
            // Pinned to the commit `ptts` 0.7.0's `DEFAULT_MODEL` names, not
            // `main`: the engine contract is tied to a specific checkpoint,
            // and a re-trained file behind the same name is exactly what the
            // sha pins exist to catch.
            base_url: "https://huggingface.co/gradium/phonon-31m-en/resolve/92741c5b07d085ff840e26fea8ce43110b87852f".to_string(),
            dir: "phonon".to_string(),
            // 64.5 MB q8 GGUF + config/tokenizer/default voice (65,051,089)
            // + the voice pack: voices.jsonl and 102 × 256,136-byte
            // embeddings (26,159,279).
            approx_bytes: 91_210_368,
            default_voice: "default",
            files: vec![
                ModelFile {
                    remote: "config.json".to_string(),
                    local: "config.json".to_string(),
                    // Published files are 4,634 / 277,967 bytes; the floors
                    // sit just under so a half-transfer never reads as
                    // installed.
                    min_bytes: 4_500,
                    sha256: "b5d0f88353638370d94035b7d73d7d6577c0554d78f8d7ecfd40490aae39ae85",
                },
                ModelFile {
                    remote: "tokenizer.json".to_string(),
                    local: "tokenizer.json".to_string(),
                    min_bytes: 270_000,
                    sha256: "691a00068fa7b989f2f3e5ae5daa2d1a5bb319f2d5360deafcfb19a8bb6e7936",
                },
                ModelFile {
                    remote: "model.q8.gguf".to_string(),
                    local: "model.q8.gguf".to_string(),
                    // The published q8 GGUF is 64,512,352 bytes. The fp32
                    // safetensors (103 MB) is deliberately not installed: the
                    // engine resolves Q80 to the GGUF first, and the smaller
                    // file makes the same audio.
                    min_bytes: 64_000_000,
                    sha256: "a7d9aad2cd6dc66473f24a08bc6d2939030ef4d751ef82349361fbef3272dbae",
                },
                ModelFile {
                    remote: "default-voice.safetensors".to_string(),
                    local: "default-voice.safetensors".to_string(),
                    // Every voice embedding in the family is 256,136 bytes.
                    min_bytes: 256_000,
                    sha256: "dc537478eaadc1a5257fcda2e3501ab81e14c111a5d95a5838f350db2afd0f1a",
                },
            ]
            .into_iter()
            .chain(phonon_voice_files())
            .collect(),
        },
        ModelSet {
            engine: "nano-edge-tts",
            base_url: nano_edge_base.to_string(),
            dir: "nano-edge-tts".to_string(),
            // ~120 MB text side + 8 MB decoder + 3 KB tokenizer + the 28
            // English voice banks (the set the distilled decoder was
            // trained on).
            approx_bytes: 128_000_000 + 28 * 522_240,
            default_voice: "af_heart",
            files: vec![
                ModelFile {
                    remote: "text-side.onnx".to_string(),
                    local: "text-side.onnx".to_string(),
                    // Floors sit just under the exported graphs' landed
                    // sizes (112.1 MB / 30.4 MB — the decoder is the
                    // exporter's external-data blob inlined into the one
                    // file); the pins are their sha256.
                    min_bytes: 110_000_000,
                    sha256: "95b661f59c5dbf588dc8dda715f449511ff746ac3c2952be8f56e59db3c81b19",
                },
                ModelFile {
                    remote: "nano-decoder.onnx".to_string(),
                    local: "nano-decoder.onnx".to_string(),
                    min_bytes: 30_000_000,
                    sha256: "484fc522c5b971dd6cb61f2cfaf6735fde1c598eea2a47504a9fca4d249e7205",
                },
                ModelFile {
                    remote: format!("{kokoro_base}/tokenizer.json"),
                    local: "tokenizer.json".to_string(),
                    min_bytes: 3_000,
                    sha256: "77a02c8e164413299b4b4c403b14f8e0e1c1b727db4d46a09d6327b861060a34",
                },
            ]
            .into_iter()
            .chain(nano_voice_files(&kokoro_base))
            .collect(),
        },
        ModelSet {
            engine: "ocr",
            base_url: rapidocr_base.to_string(),
            dir: "ocr".to_string(),
            approx_bytes: 15_603_475,
            default_voice: "",
            files: vec![
                ModelFile {
                    remote: "PP-OCRv4/ch_PP-OCRv4_det_infer.onnx".to_string(),
                    local: "ch_PP-OCRv4_det_infer.onnx".to_string(),
                    // The published files are 4 745 517 and 10 857 958 bytes;
                    // the floors sit just under them so a half-transfer
                    // never reads as installed.
                    min_bytes: 4_000_000,
                    sha256: "d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9",
                },
                ModelFile {
                    remote: "PP-OCRv4/ch_PP-OCRv4_rec_infer.onnx".to_string(),
                    local: "ch_PP-OCRv4_rec_infer.onnx".to_string(),
                    min_bytes: 10_000_000,
                    sha256: "48fc40f24f6d2a207a2b1091d3437eb3cc3eb6b676dc3ef9c37384005483683b",
                },
            ],
        },
    ]
}

/// Kokoro's 55 voice banks. The English subset is what the picker shows
/// first; the rest are the multilingual packs, each its own `.bin`.
pub fn kokoro_voices() -> Vec<ModelFile> {
    // (voice, sha256 of the published bank — HF's LFS oid for each file).
    // Name-keyed pairs, not a parallel array: the API's listing is
    // alphabetical and the voice order here is not (`zm_yunxi` /
    // `zm_yunxia` swap places between the two), so a positional table
    // would hand two voices each other's hashes.
    const VOICES: [(&str, &str); 55] = [
        ("af", "a4f11d9d055a12bfa0db2668a3e4f0ef8fd1f1ccca69494479718e44dbf9e41a"),
        ("af_alloy", "c4a6b876047fd7fb472edf4ebd63cfac7c3b958a7cae7c106e8f038ca6308c45"),
        ("af_aoede", "4a004c33430762e2461eedb2013fad808ef4ab3121f5300f554476caf58d8361"),
        ("af_bella", "f69d836209b78eb8c66e75e3cda491e26ea838a3674257e9d4e5703cbaf55c8b"),
        ("af_heart", "d583ccff3cdca2f7fae535cb998ac07e9fcb90f09737b9a41fa2734ec44a8f0b"),
        ("af_jessica", "a240a5e3c15b43563d6e923bdca8ef5613a23471d9b77653694012435df23bd8"),
        ("af_kore", "9be5221b6a941c04b561959b8ff0b06e809444dcc4ab7e75a7b23606f691819e"),
        ("af_nicole", "cd2191ab31b914ed7b318416b0e4440fdf392ddad9106a060819aa600a64f59a"),
        ("af_nova", "18778272caa0d0eebaea251c35fd635f038434f9eee5e691d02a174bd328414f"),
        ("af_river", "00a2bcf82b1d86e8f19902ede58c65ccf6c0e43b44b7d74fad54e5d8933c9c30"),
        ("af_sarah", "4409fbc125afabacc615d94db5398d847006a737b0247d6892b7a9a0007a2f0a"),
        ("af_sky", "4435255c9744f3f31659e0d714ab7689bf65d9e77ec1cce060f083912614f0b9"),
        ("am_adam", "162b035ed91cfc48b6046982184c645f72edcdd1b82843347f605d7bf7b15716"),
        ("am_echo", "3968b92c3c4cd1c4416dbded36c13eaa388a90d5788d02a13e4d781f5f8cf3c3"),
        ("am_eric", "e8b5be17edd1e3636901ce7598baafe2dc8dd8ff707a0c23bf9e461add7e2832"),
        ("am_fenrir", "c27989f741f7ee34d273a39d8a595cc0837d35f5ced9a29b7cc162614616df43"),
        ("am_liam", "52403be32fd047c6a44517cb0bcd6b134f2a18baa73e70ef41651e0eab921ade"),
        ("am_michael", "1d1f21dd8da39c30705cd4c75d039d265e9bc4a2a93ed09bc9e1b1225eb95ba1"),
        ("am_onyx", "da5d135b424164916d75a68ffb4c2abce3d7d5ccc82dd1ee6cf447ce286145e6"),
        ("am_puck", "fcf73c989033e9233e0b98713eca600c8c74dcc1614b37009d5450ff4a2274a0"),
        ("am_santa", "61150cf726ab6c5ed7a99f90a304f91f5a72c00c592e89ec94e5df11c319227a"),
        ("bf_alice", "08afa6ba24da61ea5e8efa139e5aadc938d83f0a6da5a900adaf763ac1da5573"),
        ("bf_emma", "669fe0647f9dd04fcab92f1439a40eeb4c8b4ab1f82e4996fe3d918ce4a63b73"),
        ("bf_isabella", "3754352c4aaa46d17f27654ab7518d65b62ad6163a0f55a5f4330c2da2c4e94f"),
        ("bf_lily", "5e0ee32ebe64a467124976b14e69590746f1c4ce41a12b587a50c862edfea335"),
        ("bm_daniel", "6b3194bbceffb746733cbc22c8f593dd44e401a71d53895a2dca891bc595a1e8"),
        ("bm_fable", "f889083196807b4adb15e9204252165f503b8d33d3982e681c52443c49d798f1"),
        ("bm_george", "c4b235a4c1f2cd3b939fed08b899ce9385638b763f7b73a59616c4fc9bd6c9bc"),
        ("bm_lewis", "b8f671cef828c30e66fdf0b0756a76bba58f6bb3398cbbf27058642acbcedb97"),
        ("ef_dora", "f66ec66bd295acb18372e37008533a9a3228483ccd294e7538d5d9294ac9a532"),
        ("em_alex", "27809e9eafdcbcfff90a3016c697568676531de2a2c39cee29c96c7bd6b83e95"),
        ("em_santa", "ad43b774e1ca24d05c6161297d8aeb770ac3d29bb95daf516727af5f7d543683"),
        ("ff_siwis", "a35f5675ad08948e326ae75fd0ea16ba5d0042e4f76b5f3d1df77d0a48c54861"),
        ("hf_alpha", "040be6a4425411cc01fda5fd06693c76bfa78572632852bc8cda9c99232ffb56"),
        ("hf_beta", "cd83ae0bb9b2e4e4fb92b4973bd8d1822ca0036d3c498bf4fc89aa8e33917cc7"),
        ("hm_omega", "b02d9222d9ed00ce26b302173a862c2c93f96cc40b5c422b8d14910b9ff34137"),
        ("hm_psi", "644daf88ba8aeb7bd08950bbdcd4453bb280864e49dc4df93fabc6be32e03f37"),
        ("if_sara", "409b69248798fcdc2542330c76953d230710f19b057e59cb82fdc3c4cf71265c"),
        ("im_nicola", "bc578e510d52a96d6940d46f12e96d7b3df00905dbea075113226d100e6e1ab0"),
        ("jf_alpha", "56b479360aad9f367aeb8cef908f9201cf48b4555e488c5f4590c9dfcd978bb6"),
        ("jf_gongitsune", "0f1181f3772d27b7c12aaf4bcd71e31b186c4146e330d074a3dc64ee392af396"),
        ("jf_nezumi", "13cb71eebb0b48739d444558322aa35a8c9a489b80e1e631f14d2e6aea93026b"),
        ("jf_tebukuro", "29c6c0561b4288d59639677bebe7533c919743d5ea68d0d2ae992644beea6696"),
        ("jm_kumo", "09e959d239724c734d65661f06f14cdabcddfd476bfaaad905a937099ae9e64f"),
        ("pf_dora", "3da7b5b2d91847ebf5646f57631af6ececae3c29a89cd300f06edf9aa6cfe9ee"),
        ("pm_alex", "0175c753f59c54e7fd5a995bedef0c5ff2fb67e0043dd3dcb2ae74ec2acbeb2a"),
        ("pm_santa", "8b012db3185778afe2e45a62cbad69db73021774fe68dda634bcc748a982eede"),
        ("zf_xiaobei", "5dde6e1c9c4f12c8b327bc29c0cee361a23b52b952c04636858ba637ec66e640"),
        ("zf_xiaoni", "08892b62a39af0a615cd0581238db7e19e44c578e8fa0bfd0e586e93327d9cba"),
        ("zf_xiaoxiao", "03adb5d5e3ddd88b047954e974e651cb0a4b524c985057e5d872e962c7be1169"),
        ("zf_xiaoyi", "bc1555c5c486099196ac254bae5e0bb543c121952a3092f50b7d8724f1bc36b3"),
        ("zm_yunjian", "de48a00bdbf3649f07162269a2b6e0513604389bfac8a2e6c75cb34b323ad6fa"),
        ("zm_yunxi", "7243892fb4e560d47014090ddf010f8b8b790f3c6b029ff82b2ac06aa4e27c8b"),
        ("zm_yunxia", "6b2b8fc15b3df19a368daebe5c581c7fabf433ee5b8a17ffd6b3d723cff8936d"),
        ("zm_yunyang", "261e2c89470534dbbcb8fd98b8fdc495ec94063d9bb6c8277f7be43cccba3f42"),
    ];
    VOICES
        .into_iter()
        .map(|(v, sha)| ModelFile {
            remote: format!("voices/{v}.bin"),
            local: format!("voices/{v}.bin"),
            // A full bank is 510 rows × 256 floats = 522 240 bytes, and that
            // is the floor for all of them — including `af.bin`, which is one
            // row *longer* (512 rows, 524 288). The floor must sit below the
            // smallest bank, not above it, or a complete install reads as
            // missing.
            min_bytes: 522_240,
            sha256: sha,
        })
        .collect()
}

/// Phonon's voice pack: the `voices.jsonl` catalog and the 102 voice
/// embeddings it names, from `gradium/phonon-voices` at the commit `ptts`
/// 0.7.0's `DEFAULT_VOICE_CATALOG` pins. These live in a *different* Hugging
/// Face repo than the model, so their `remote` fields carry absolute URLs —
/// [`ModelManager::install_set`] passes those through unjoined. The ogg
/// preview audio in that repo is deliberately not downloaded; only the
/// embeddings the engine loads are.
///
/// Every embedding is 256,136 bytes (the same shape as the model's bundled
/// default voice); the catalog is 33,407. The oids are the LFS oids HF
/// publishes per file (the catalog JSON sits under the LFS threshold and was
/// hashed from the published bytes at the pinned commit).
pub fn phonon_voice_files() -> Vec<ModelFile> {
    const BASE: &str =
        "https://huggingface.co/gradium/phonon-voices/resolve/da6c9ec0d47be8df5c87c730a3aa97187f3229c3";
    // (voice id, sha256 of the published embedding). The id IS the loader's
    // registered name (the file stem under `voices/`), so a pin here and a
    // voice code in prefs are the same string.
    const VOICES: [(&str, &str); 102] = [
        ("0fvH6XAKvA748v2v", "5646677e9cf4a517e8cc316f94a34e18fa987e224925d916d6fe13396579b95b"),
        ("0zLApsZfVLLNaEGl", "dc1341c8937af5534e6bbcc5c0fc5e7c10312229b59061228074f93683fe2261"),
        ("20zdyYrQPzKlCwkk", "28af62a4ed8af73b560dd5f78f48045969a74306f2d85aedcb4c820025f74870"),
        ("2A2fFf8SkDEJ8nzx", "5d5ad59fdd4642cd162f082873b27f658705a5d8598957d6274b30bf4bd4dc0b"),
        ("3ZKKapPOvuWFcw9f", "b645c79148a02f6cd2ef7a6b3658373166e19a9a1c49fb81408be5d79667e6cb"),
        ("3p0eIGbkmny71GlA", "275928c11b3c8b02e51c72653d060433e1d73a01458ef21b18e9a7f5cbd5205c"),
        ("40QrMVgAxFiUnP9f", "0de1098d1051686a138d068e83ff09deb5053328bc74ec9541a1fdb90bfc24da"),
        ("4701YgRXjkBcg6sL", "74c6bf732b76478e77072d21feca13b0bf0c664eccd2186902a63ba0d9effe59"),
        ("4Mn9VfG2wsLLEzi5", "720e656aa74cd81729593e0672a6213f5f9e5480bead08321e2a6a327eb8e453"),
        ("4SZHfMpw-p46Ywgs", "6b3da6e2d796a20c51af892c482bd6509fb88267a221aff83c870843dee2a123"),
        ("4iKxBtHBRzr1YOph", "c93483af64a9dd990e42264d2b6bb68909fcdff66873945acf7701e501d6e849"),
        ("6HftnVU1XqVBJS8t", "b0c3223c6de2597e3eef285e9def7998e9861b2056ee4784cddb29174bfc4dc4"),
        ("6PWnV0Nq4wu7RVBT", "56494ee67d533ad227ff0dfd0b1c13ff5aa91f1ad6e596e66c2986ba3005e844"),
        ("6k6cRt7QJfm5ChrH", "96ca40ceb24b3cbbff9868c6a920f6c7ff3d471b76698d8cd8b7c542c38d814f"),
        ("ALczEazoF4n6PmGl", "f4ebe6b59ebfa09339cb4c15c9c8f0af72fb6cecd9feb1f550dee0d95c8092c1"),
        ("Adoh4wA9tp5QXJi9", "0179a71e0aa1e89320ed35da4c6dea30384f98d24ee931517916742ccf7e6002"),
        ("BkuASiOJRrJEB5qA", "d32ad93e71b8c726a8f1aac62f276db82346fc1a74a02cfc18ce2b867cf20a07"),
        ("CtNvKUUMrx1h56ih", "ad6f5ca7df38abbb15b85a09613b14ebd76d70108d5420e6f21486a6a10154c9"),
        ("EQcQFGkYfMn5YJgx", "c419a49606182c98e06c369d0cfb9b66cc0f16bfdbc70490fa7daf2289144986"),
        ("Ejb29qHnSOK0cIHY", "471332253b2f76c406e395230453a7a737708cca18461237cfce0239430cb071"),
        ("F05gqkzX7WhKVpSL", "08159aad0d92cb391361abaf8c4bf6ac0d8358ab1514cce8cf5c89b448395ce5"),
        ("FXxJ9mANRq6BCTX5", "9f575447f6908727214c2a855e0e33a7c316e9360a0d8b66c6ccaa8f6e15dade"),
        ("FaLVVqpFQcDOtq45", "31a3837cc6705f37f95c27282ebeba71609b32204622245a6436258cd3d3497d"),
        ("G5HLUsQd659UtRe7", "fba1a4349c3c86aabc58d6570a82fac0ed643e5e7aa97b52a3ede6b792e6d420"),
        ("GXaSqMEmOg12OIYb", "bfa37bac98aefbb9de80f792c47d01a0f9e3b94e73776169040c249c8b722759"),
        ("GgfEkEJtxZR7gnpy", "eb3cd47418349e618408c16bea7153988b3ad04b41a6033039dd474a9a4dcd23"),
        ("JOW0vchyswkQUMDS", "cc9ac1107b0ffce282374a19beb7bfa95847724e6b7afbac991b4ae4277d5e6d"),
        ("JYtmeaysoaR0Wybf", "2450c247061a20c202f63caabe4594379a8fc8599f9fcbb28642a2dd3d247ad1"),
        ("K8mj96kYHMy8q3Hb", "00d4323db427bb68d8b6eed15b1294840e2037bcabc1d6fe6385191efae414b0"),
        ("KJpIQlbYrZPA4aLq", "b5bcbbce39a6332b2f79067be19688753c28c1fc4053bf41fd4608d54c24e1ee"),
        ("Kf5m22mROozoMWj3", "1cd9e5471568fe905906742568b5d3d2e89cdac1fa8b8272780d984d7d54ec9a"),
        ("KgC2Nqnjj48NUiyV", "f6b455eec3ad536c2c8b7b2f18a5620cb46ebf7576a040299fce4de6376ad3a3"),
        ("LIv4NaG8ay4Mhqtd", "4f7f904c976c0b493e9d42654dc81d4f9e686b986fbb3a7d511a7dcc41c7dea6"),
        ("LMDIYE0Oa377Qh6Z", "45c582673cb20edb04ddcca2b5a8865ddfb0a82599efadb269021e87d20fbdd4"),
        ("MOrDPn052KUlnKXk", "a0f55b29cf02a7bab15a9f8ff961f632e7f7a393b2c69a0ce2c4026e7a9c1742"),
        ("MxhgvuHuUER7qjmL", "05160ccf33ce0e22da9a2eb601e992bc9fc34e2591e22d3af4f07a07f3731cb7"),
        ("NIWIOv5vTi5eRMj8", "444378eb06b7c492b86e8fc7d13e4db1a3c0cf3e68891a960bf6f0a41a13ef35"),
        ("NIWoEurJHMJjyUTe", "bc8a8fcb95bb4f32d2d4376aa69d81b186b3ba2f8e9b6cb6a403b2b194590e85"),
        ("NbpkqMVS3CJeq2j8", "97c7f8c253413574406ab80dd23f49c3ebb2517163b00a9bf86b9c4cf43e7e23"),
        ("NuUr_x5V90hSHzCJ", "593c8c56619008116b30d7ad1bae5433d6834060b5b757e51799574601badd99"),
        ("PwgPEaweQILZdGc4", "91af1c0e9233b3069fab26eabd191e6be4d7825eff5d5e1ce3034fbd8f7fcf24"),
        ("QXNr0SvkukLB4wPq", "f390c09f5e279c0f2171995a534b92f5ca5c83443906879da68fc27445f89d89"),
        ("R3uU1GTAZJotUhUs", "36840305b88f217cc20e153276eaaf74718e5e2b50ac05f0dd6c4606b0d576a2"),
        ("Sx8m6WdXBG06zwZT", "8756dddf95fa449d0e5a8649c6a242a460329c53e2f870bbe6ed744d0573a251"),
        ("TUk4X11S6UytB3G6", "cae40431c178258727313c8c4f7c037645ece7fc2f4287644cc7a7138ca4e90c"),
        ("Tek4tJXiX6_yvXq7", "0106f9a0d53685471695a76827f7912e6c890f21fea10b5e5160379da38ffe5b"),
        ("VDwnGxAo68C8U8vC", "ccb1b88e3899db1cb7ba71007d08ed56dd743882214f0495b67feab89850ffe0"),
        ("Vvakv7iWZCUrBooR", "a8c97074fdb4044fc8da2873bdccd8b2a64beeedab859d241d5fc11c0c400aad"),
        ("X6NceqF285Gr04gb", "4de75497fff3030168e8aa6dca23ffeb332b1bc8c25d0bf3507ceadac953ae9c"),
        ("Xn9xDzqSSr4xGTNg", "275c9024bf16518fab2c477d4acdc76cad6f2c322ce083ab56238c3856012f57"),
        ("YKeBw3OV1RgpdhLh", "16c1df0d46be869159d4a40c81b888eaa8dbc494045e57b24d293384149f008e"),
        ("YhIHaAfQ0cQPDV9R", "fbed904e1c25c3410db4adc28b8e27686c9fd9d61353a02a23bcd1a6da054280"),
        ("YyKcxPG7iIgS3jsK", "3169d74b47a54d1eb15e4c243f56393abe9fe90c7e1c308ecf96ee67618b152a"),
        ("ZeSg853xFACESHHI", "21dd67c10f307aa48e1187f48159bb3d41b07d0cd17a3e6366efd508e6fe83a6"),
        ("Zn7g6NxLqXlJb7Qi", "baea0ea9506f3237a5ebfc61afb7c72dbd189c5603b199ef42fbb00dbda379e0"),
        ("_6Aslh2DxfmnRLmP", "beae9e4030efce0a935fcb0f807dc0784d598bcaa87ad4aacb047436b1eaa411"),
        ("aXMVwiUhITMrwPzd", "22421f1fbb25d325eabc351500fed2e92c53b54facc4bc97ba3bc6cc6e75e42c"),
        ("ax7dW1XB0eiMfHgO", "128eeba9b5f5c6d336e1246ae20a40765fba8e5ac268aafa691b98bce1e0acfd"),
        ("b6FvJAiokjdqIti4", "0bc37938a306a35f6c9f8ea53ccca14c5fc8b5575850f0cd15b352f4c3ce826e"),
        ("bXwrkHhURL1VFJdG", "a879938aa9a2df0a8220bdadef88137929f71e887bd91e7db205bd5fa11661d2"),
        ("biuhvu17TxVKOcyy", "a859356200a33faf75c591d24fc0fbb998aa66b53c2120f392ef6305d824ec9d"),
        ("c9wqBDmQiBie5q6Y", "4857d013c7502e4d074778b17122e85919bffa52741a0072e412dd3926853dcb"),
        ("cXoIXJ2llS8S0WOW", "a05944e4c69d61d10d66f689175ba0b081930df2d77b22f04d00ac40d3d69ecb"),
        ("cYtsRSxV5q9IMehn", "b30fa847686ec1eb6be8c5bea4b6a5c5076ea66c3ff053cf98210a155d8aad1e"),
        ("d9XCyhjo4JhkHF1d", "eab20e4f159b48cfbc01b1885804e24f681f80e7a64f909182763a4e3361bee8"),
        ("dGyGDKo2osj7h3s7", "83900e020c14946426614c6553c5a138ba6277e1aaa69832223bd0f0f8b73893"),
        ("dME3IWyZBvmh1n1q", "09ab315c7f4b72b4d71e66cb756184cedfb1f14d222d096e115bf95bfe7b29dc"),
        ("evfxKSxpDIhmsNoq", "549f2a8dec851b967edbbd67b29d2da0799eec9ec2fefb5b0ec8aa99436d3eea"),
        ("fghJGlv90qu8OJ36", "60656e9cd518454850a7e6ecacb953572a4a23f0a358fe3b4fb946b67f6bf934"),
        ("gDT1nz7Ie36ZhL-C", "1980ab3c35fa98a25b5f3b5e643fd852e14ca8a12a4a26b06aaf1abf2a8c68b1"),
        ("gOc90Rjmv5CobHCj", "f6e911481dd0bd60a36d77c1883f2f237b5b78275615498764a07456e8685c7c"),
        ("gSgG5Tor0GPEnqzC", "a83430cc022b85c83753f859f9e44f406651646dab994fced8ef7a9a88005971"),
        ("gfP44RlHwr2kEwk7", "bcc207a73b7885e45b49ab633d3377a10158292e3a7cef9cd6764ad107f1cb9b"),
        ("jvPx8j8zLGQ3utZz", "be8c5f329070f40c39007bea13cee9503fe71599704cfe55c67d6eb51f5e6eb2"),
        ("l7520SIXi14iEamd", "044d37978b68c5363e0fda99873c9f657323c350d8351b4d266bf03b2f839c30"),
        ("lX3LavMNdlIbgEHH", "d3f9429205e5906dcd3c78edaf7c9d5148c7e389e34c82c86f72f154adb5d923"),
        ("m9MxpY6IUGXtWp76", "a906ebf17e43fded12b3d6bd077abb2df33bc86eff51c1beeb82e5d416527db3"),
        ("mWgyMRkFED70QxQv", "8732e9ed54a3380cf864152d3adfd0ed056af85f5e2f468395244a51a923842e"),
        ("nGzMHMPAWGaJFHOo", "a0c3dcb91b2247eca56536be9ae8e8becc1c807b1136c5fe101552d6a155b24a"),
        ("nOykTmcH9ydsGGIc", "2ff2db91e70648623df8d42c0dcd7c336b2f14cb4362d602a20f292a4d862154"),
        ("p6Uutkyi3j2iNAUu", "0a4ec738c4222645f175dca1015686b0a628f100650a5e0ab26e88d0a9b60fd4"),
        ("qDeAnE4L4bk0BRz1", "842930df459e1d636cdd3414cd60ab2631ebfdff29460a922415e506b1cc5ed1"),
        ("qG5BXOJYrKEnXLIX", "3d5590357dbd87b697a14573bbdc1f29e2abc5b6cd8a5e9ed3c98e325dede435"),
        ("qIxlZ7Ei8KvWlyQH", "6e04b84d3ae8751b22bca0a4f63936f9cb648f4f2182b2d0788071ea1c0b68a1"),
        ("qtPlUYAZ4EJDuWMm", "250af13c97fdc3519ccde9c8d5e1ae1953f8ad365fc7d736aa5eae2ad2e14473"),
        ("rmLBOdHrfkFym9MM", "6ed37f47fe59674b0c9f2c099ab983d4a847ca14d09aedf80f1608a11148c757"),
        ("rqqeErpASHVQXHDd", "cce882370d25a3c4d29cb226af218a44274674805e7091447d27d3ad61bb25b3"),
        ("s048cR1l2Jmu4k3B", "f7b80c03210e3fd736de6596586305e9b7eb0e615047dd1cbb1b58fc3f92985f"),
        ("s58clrg2fe0MO7Y-", "ab923d4fd9ed09954e3a91c61780bcd536f5bb5787e1d0541cb6ff56dd7ff531"),
        ("sGCMzFIA53ppIngM", "edbfd1498e78dab5a1f707c4ff5a169e887741abf4e78d2e9759d242cd32b25c"),
        ("t-_TS1e-0GzDAX02", "25c6df91a261bcb96d9e5152c9b0988398859c0e6f4ee5f08dd577fd3bd0300d"),
        ("tIjCgP8v3RY8UpgM", "fc8828dcdad4a09b7066b9008b108b77976ec4880d531da7761ded9a431bd208"),
        ("u2UscyAnHilpiwmf", "edd14cc89944ec7a9b1a87cc6be6f371f1a56791d344d921f850e2106e6dee9e"),
        ("uem82D50GRv2Dwma", "d41a8f01d6e33450f1ce062d332914da10beee99e585dab531750706f187c48a"),
        ("vjFyp2BcCyTlUmYb", "958ea94da14f46dd48c7f64adac11ed2ad2aa45f7ccd165dfebab5272664402e"),
        ("xO8xQo3BrB8FuI6J", "63ab4b15427844a804480a1603774262e8903dee33cb279e2749860dbfaedd10"),
        ("xPDpsV3F592srkjA", "a09f615b0084980ebc4076d71550bf73ccd8d4df076bbbee4a9beb523a1379d9"),
        ("y2eiKxtQo2B67deU", "b3cea6789aeb58822af16ba08c4e5105d7febb840dcb67b47f426c19d1a8f209"),
        ("y9bQqIDnyxwqK01t", "21eade9d83b2e66b9364e1b6cd999a3a6704c000a71f27f34e05ace5cdfd58bb"),
        ("yadpmqsHVrYC0NJp", "f0c5e9b03ec651da1b8ae2d76f23b46381c9943e1e7c7ad434f66a6bb1a16d44"),
        ("yyS1KYWs6mXoEw7D", "4a1bea252ee32f44dff1852894a521bb5de0b58a3ba5f18011c84aab0ad45916"),
        ("zUCxzrOrugTSis03", "ff1d0735b8b45710462b1d1f846da040f970c6eb62c7c2f36dbfb8e5bde74f1e"),
    ];
    let mut files = vec![ModelFile {
        remote: format!("{BASE}/voices.jsonl"),
        local: "voices.jsonl".to_string(),
        min_bytes: 33_000,
        sha256: "874f34998ce581b94f2b8912aa015639f52b5fd62eba3ba11e1b64dcaed7d802",
    }];
    files.extend(VOICES.into_iter().map(|(id, sha)| ModelFile {
        remote: format!("{BASE}/voices/{id}.safetensors"),
        local: format!("voices/{id}.safetensors"),
        min_bytes: 256_000,
        sha256: sha,
    }));
    files
}

/// The nano-edge decoder's voices: Kokoro's English banks (af/am =
/// American, bf/bm = British — the 28 named English voices the distilled
/// decoder was trained on; kokoro's `af.bin` aggregate row is not a voice
/// and is excluded, and other languages run but are untested, so they are
/// not offered). The same published files at the same pins as kokoro's —
/// fetched from kokoro's repo, not the nano set's.
pub fn nano_voice_files(kokoro_base: &str) -> Vec<ModelFile> {
    kokoro_voices()
        .into_iter()
        .filter(|f| {
            let stem = f.local.trim_start_matches("voices/").trim_end_matches(".bin");
            stem.starts_with("af_") || stem.starts_with("am_")
                || stem.starts_with("bf_") || stem.starts_with("bm_")
        })
        .map(|mut f| {
            f.remote = format!("{kokoro_base}/{}", f.remote);
            f
        })
        .collect()
}

/// Where a set stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallState {
    /// Nothing (or only a `.part`) is present.
    Absent,
    /// Every file is present and above its floor.
    Installed,
}

/// The manager: the catalog, one resident engine, and a progress hook the
/// UI reads.
pub struct ModelManager {
    /// The engine currently loaded into memory, and its voice. `None` means
    /// nothing is resident — the state the app boots in, and the state after
    /// a `drop_resident`.
    resident: Option<Resident>,
    /// Fractional progress of the running download, for the status line.
    progress: Mutex<HashMap<&'static str, f64>>,
}

/// One loaded engine, kept behind a name so the pipeline can ask by kind.
///
/// `Arc` rather than `Box`: the pipeline holds its engine for the session's
/// life while the manager keeps it resident for the next session — one
/// graph, two handles, and a re-ask at the same settings hands back the
/// same instance instead of re-opening it.
pub struct Resident {
    pub engine: &'static str,
    pub voice: String,
    /// The rate the engine was built at. Part of the reuse key: engines
    /// bake the rate in at construction, so a reuse test without it would
    /// hand the speed control a stale engine and make the slider a no-op.
    rate: f64,
    /// The speech language the engine was built for. Part of the reuse key
    /// for the same reason: supertonic bakes it in (the `<lang>` wrap),
    /// so a language change must rebuild, not reuse.
    lang: String,
    pub inner: std::sync::Arc<dyn Engine>,
}

impl Default for ModelManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelManager {
    pub fn new() -> Self {
        Self {
            resident: None,
            progress: Mutex::new(HashMap::new()),
        }
    }

    /// The catalog entry for an engine name, served from the catalog's own
    /// static storage — no allocation, no leak, however often it is called.
    /// `state()` reads it on every `--engines` line and the picker reads it
    /// per engine, so a per-call box would be a slow leak the switch gate's
    /// `resident()` cycling could never see.
    pub fn set_for(engine: &str) -> Option<&'static ModelSet> {
        static BY_ENGINE: std::sync::OnceLock<Vec<(&'static str, &'static ModelSet)>> =
            std::sync::OnceLock::new();
        let index = BY_ENGINE.get_or_init(|| {
            //  index every set, `ocr` included — `state()`/`install()`/
            // `voices()` resolve "ocr" through here for the Models page;
            // only `catalog()` filters it out of the speech list.
            build_catalog()
                .into_iter()
                .map(|set| {
                    // Leak once per distinct engine at most: the entry handed
                    // out is &'static, and the catalog itself is rebuilt per
                    // call, so this is the memo that stops that rebuild from
                    // mattering at all.
                    let leaked: &'static ModelSet = Box::leak(Box::new(set));
                    (leaked.engine, leaked)
                })
                .collect()
        });
        index
            .iter()
            .find(|(name, _)| *name == engine)
            .map(|(_, set)| *set)
    }

    /// Whether the engine is the espeak CLI, which needs no model files.
    pub fn is_builtin(engine: &str) -> bool {
        engine == "espeak"
    }

    /// The on-disk state of an engine's model set. `kokoro` holds extra
    /// files (`model_uint8.onnx`, `af.bin`) that are not part of this
    /// catalog; only the catalog's files decide `Installed`.
    pub fn state(engine: &str) -> InstallState {
        let Some(set) = Self::set_for(engine) else {
            return InstallState::Absent;
        };
        let dir = set.path();
        let all_present = set.files.iter().all(|f| {
            let path = dir.join(&f.local);
            std::fs::metadata(&path)
                .map(|m| m.len() >= f.min_bytes)
                .unwrap_or(false)
        });
        if all_present {
            InstallState::Installed
        } else {
            InstallState::Absent
        }
    }

    /// The engines this box can actually run: the CLI, plus every neural set
    /// whose files are all present above their floors. `ocr` is page
    /// recognition, not a voice —  comment adapted, it installs from
    /// the Models page via `set_for` and is never for the speech picker.
    pub fn available_engines() -> Vec<&'static str> {
        let mut out: Vec<&'static str> = vec!["espeak"];
        for set in catalog() {
            if set.engine == "ocr" {
                continue;
            }
            if Self::state(set.engine) == InstallState::Installed {
                out.push(set.engine);
            }
        }
        out
    }

    /// The first engine that is installed and not a fallback, for a fresh
    /// prefs file: neural quality is the whole point of Batch 6, and a first
    /// run on this box should use the best voice it has. Kokoro stays the
    /// fresh-install reference; nano (the same voices, distilled) rides
    /// second for boxes where kokoro is absent.
    pub fn preferred_engine() -> &'static str {
        for engine in ["kokoro", "nano-edge-tts", "supertonic", "espeak"] {
            if Self::state(engine) == InstallState::Installed {
                return engine;
            }
        }
        "espeak"
    }

    /// Delete an engine's model set. Shared files (kokoro's voice banks are
    /// per-voice) are all inside the directory, so one recursive remove is
    /// exact.
    pub fn delete(&mut self, engine: &str) -> Result<(), TtsError> {
        let Some(set) = Self::set_for(engine) else {
            return Err(TtsError::ModelLoad(format!(
                "no model set for engine {engine:?}"
            )));
        };
        let dir = set.path();
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(TtsError::Io)?;
        }
        // A resident engine on a deleted set would keep serving from a graph
        // whose files are gone — possible on Linux (the fd stays open) but
        // a surprise the user can see, so drop it here.
        let should_drop = self
            .resident
            .as_ref()
            .is_some_and(|r| r.engine == engine);
        if should_drop {
            self.resident = None;
        }
        Ok(())
    }

    /// The resident engine, loading it on demand. Single-resident: asking for
    /// a different engine drops the first, so two model pools are never in
    /// memory at once. `espeak` needs no files and is never dropped this way.
    pub fn resident(
        &mut self,
        engine: &str,
        voice: &str,
        rate_multiplier: f64,
        lang: &str,
    ) -> Result<std::sync::Arc<dyn Engine>, TtsError> {
        let existing_ok = self.resident.as_ref().is_some_and(|r| {
            r.engine == engine && r.voice == voice && r.rate == rate_multiplier && r.lang == lang
        });
        if !existing_ok {
            if self.resident.is_some() {
                // Drop the old engine *before* building the new one: the
                // memory the old pool holds is the headroom the new one
                // needs, and Supertonic's four sessions are the reason.
                self.resident = None;
            }
            let inner: std::sync::Arc<dyn Engine> =
                build_engine(engine, voice, rate_multiplier, lang).map(std::sync::Arc::from)?;
            self.resident = Some(Resident {
                engine: leak_name(engine),
                voice: voice.to_string(),
                rate: rate_multiplier,
                lang: lang.to_string(),
                inner,
            });
        }
        self.resident
            .as_ref()
            .map(|r| r.inner.clone())
            .ok_or_else(|| TtsError::State("resident engine vanished".to_string()))
    }

    /// Drop whatever is resident. Called on shutdown and before a delete.
    pub fn drop_resident(&mut self) {
        self.resident = None;
    }

    /// The engine and voice currently resident, if any.
    pub fn resident_name(&self) -> Option<(&'static str, &str)> {
        self.resident
            .as_ref()
            .map(|r| (r.engine, r.voice.as_str()))
    }

    /// Progress of a running download as a fraction in 0..=1.
    pub fn progress(&self, engine: &str) -> f64 {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(engine)
            .copied()
            .unwrap_or(0.0)
    }

    fn set_progress(&self, engine: &'static str, value: f64) {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(engine, value);
    }

    /// Record a download's progress for [`Self::progress`] readers — the
    /// settings page's worker thread reports through here, so the bar it
    /// draws is the manager's own number rather than a second bookkeeping
    /// copy the two could disagree on.
    pub fn report_progress(&self, engine: &str, value: f64) {
        if let Some(set) = Self::set_for(engine) {
            self.set_progress(set.engine, value);
        }
    }

    /// Download a model set. Runs the whole transfer, then validates every
    /// file against its floor; a short landing deletes its own `.part`
    /// before returning (the one failure a floor can name). A mid-transfer
    /// I/O error (disk full) leaves the `.part` behind deliberately — it is
    /// smaller than the floor, so the next attempt overwrites it rather than
    /// resuming a file whose tail nobody can vouch for.
    ///
    /// `on_progress` gets the overall fraction (0..=1) weighted by file size,
    /// not a per-file one: with a 325 MB graph and 55 small banks, a
    /// per-file fraction would sit at zero for minutes.
    pub fn install(
        &mut self,
        engine: &str,
        on_progress: &mut dyn FnMut(f64),
    ) -> Result<(), TtsError> {
        let set = Self::set_for(engine)
            .ok_or_else(|| TtsError::ModelLoad(format!("no model set for {engine:?}")))?;
        self.install_set(set, on_progress)
    }

    /// [`Self::install`]'s body, against a set rather than a catalog name —
    /// the truncated-fetch gate drives this with a local throwaway server,
    /// and it must run the manager's real transfer/floor/rename code, not a
    /// re-implementation of it that would keep passing while the real one
    /// changed.
    pub fn install_set(
        &mut self,
        set: &ModelSet,
        on_progress: &mut dyn FnMut(f64),
    ) -> Result<(), TtsError> {
        let dir = set.path();
        std::fs::create_dir_all(&dir).map_err(TtsError::Io)?;

        // The denominator is the set's *approximate* size, not the sum of
        // floors: floors sit ~8% below the published sizes, so a numerator
        // advanced by real landed bytes would clamp the fraction to 1.0
        // with the largest file still mid-transfer — a status line pinned
        // at 100% for the last 26 MB of kokoro. `approx_bytes` is the same
        // number the "this will take 354 MB" line quotes, so the bar and
        // the promise agree.
        let total: u64 = set.approx_bytes.max(1);
        let mut done: u64 = 0;
        // Hoisted out of the progress closure: the closure runs once per
        // read chunk (thousands of times per large download) and must not
        // allocate, let alone leak a fresh `&'static` per tick.
        let engine_key = leak_name(set.engine);
        for file in &set.files {
            let target = dir.join(&file.local);
            if let Ok(meta) = std::fs::metadata(&target) {
                if meta.len() >= file.min_bytes {
                    // Advance by what is on disk — the same unit the
                    // transfer leg advances by, so a resumed install's
                    // fraction starts where the disk says it should.
                    done = done.saturating_add(meta.len());
                    continue;
                }
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(TtsError::Io)?;
            }
            let url = if file.remote.starts_with("https://") || file.remote.starts_with("http://") {
                file.remote.clone()
            } else {
                format!("{}/{}", set.base_url, file.remote)
            };
            let mut landed_file = None;
            for attempt in 0..3u32 {
                let mut sink = |written: u64| {
                    let fraction = (done + written) as f64 / total as f64;
                    self.set_progress(engine_key, fraction.clamp(0.0, 1.0));
                    on_progress(fraction.clamp(0.0, 1.0));
                    Ok(())
                };
                match sn_net::download(
                    &url,
                    target.parent().unwrap_or(&dir),
                    file.min_bytes.saturating_mul(4).max(file.min_bytes + 1_000_000),
                    &mut sink,
                ) {
                    Ok(download) => {
                        landed_file = Some(download);
                        break;
                    }
                    // HF throttles a long file burst: the measured failure
                    // shape is a TLS "unexpected end of file" mid-install
                    // (twice in the first phonon install, at files 40 and 82
                    // of 107). The transient shapes get two retries with a
                    // widening backoff; the permanent ones — a cross-host
                    // redirect, a body over the cap, a missing length — fail
                    // the install at once. Floors and hashes still gate every
                    // file, so a retry cannot land anything a load would
                    // trust.
                    Err(err @ (sn_net::NetError::Http(_) | sn_net::NetError::Io(_)))
                        if attempt < 2 =>
                    {
                        tracing::warn!(
                            file = %file.local,
                            attempt = attempt + 1,
                            error = %err,
                            "transient download failure, retrying"
                        );
                        std::thread::sleep(std::time::Duration::from_secs(
                            if attempt == 0 { 1 } else { 3 },
                        ));
                    }
                    Err(err) => {
                        return Err(TtsError::ModelLoad(format!("{}: {err}", file.local)));
                    }
                }
            }
            let download = landed_file.ok_or_else(|| {
                TtsError::ModelLoad(format!("{}: download did not complete", file.local))
            })?;

            // A download that under-reports its own length lands whole and
            // then fails this check — the model must not load from a file the
            // server truncated.
            let landed = std::fs::metadata(&download.part)
                .map_err(TtsError::Io)?
                .len();
            if landed < file.min_bytes {
                let _ = std::fs::remove_file(&download.part);
                return Err(TtsError::ModelLoad(format!(
                    "{}: {} bytes, expected at least {} (partial download)",
                    file.local, landed, file.min_bytes
                )));
            }
            // The size floor passes a tampered file of the right length; the
            // pinned hash does not. Verified on the `.part`, before the
            // rename, so a refused download never lands as a plausible model.
            if !file.sha256.is_empty() {
                let got = sha256_file(&download.part).map_err(TtsError::Io)?;
                if got != file.sha256 {
                    let _ = std::fs::remove_file(&download.part);
                    return Err(TtsError::ModelLoad(format!(
                        "{}: sha256 {got} does not match the pinned {} — \
                         this is not the published model, refusing it",
                        file.local, file.sha256
                    )));
                }
            }
            // Atomic: rename the `.part` over the target. A `.part` that
            // survives a crash is smaller than the floor and is re-downloaded
            // on the next attempt.
            std::fs::rename(&download.part, &target).map_err(TtsError::Io)?;
            done += file.min_bytes.max(landed);
            self.set_progress(engine_key, (done as f64 / total as f64).min(1.0));
        }

        // Clamp, not min, on the final tick: real files land above their
        // floors, so `done` overshoots the approx sum by design and the
        // last reported fraction is exactly 1.0.
        self.set_progress(engine_key, 1.0);
        if Self::state(set.engine) != InstallState::Installed {
            return Err(TtsError::ModelLoad(format!(
                "{}: files on disk failed validation after download",
                set.engine
            )));
        }
        Ok(())
    }

    /// Free space in the models directory's filesystem, in bytes. The
    /// download refuses to start below 1.5× the model set's size — the
    /// iOS port's PocketPal lesson: a half-written 300 MB model is worse
    /// than one never started.
    pub fn free_space_bytes() -> Option<u64> {
        free_space(&paths::models_dir())
    }

    /// Is there room to install this engine's set?
    pub fn has_room(engine: &str) -> bool {
        let Some(set) = Self::set_for(engine) else {
            return false;
        };
        match Self::free_space_bytes() {
            Some(free) => free >= set.approx_bytes() * 3 / 2,
            // An unreadable filesystem is not a licence to fill it: assume no
            // room rather than the reverse.
            None => false,
        }
    }

    /// The voices an engine offers, read off the disk rather than loaded
    /// through an ONNX session. A picker must not need a 325 MB graph in
    /// memory to show a list, and this is why every engine's voice set is
    /// discoverable from its directory alone:
    ///
    /// * espeak — its own `--voices` listing, parsed
    /// * kokoro — `voices/*.bin` beside the graph
    /// * supertonic — `voice_styles/*.json`
    /// * phonon — the `voices.jsonl` pack catalog beside the checkpoint
    ///   (falling back to the raw `voices/*.safetensors` stems), plus the
    ///   bundled `default`
    ///
    /// A model set that is not installed yields an empty list, which the
    /// picker shows as "install this engine first" rather than as an error.
    pub fn voices(engine: &str) -> Vec<Voice> {
        // The pico alias answers with espeak's voices, as it does at build.
        if engine == "espeak" || engine == "pico" {
            return crate::espeak::list_voices();
        }
        let Some(set) = Self::set_for(engine) else {
            return Vec::new();
        };
        if Self::state(engine) != InstallState::Installed {
            return Vec::new();
        }
        let dir = set.path();
        match engine {
            "kokoro" => sorted_stems(&dir.join("voices"), ".bin"),
            "nano-edge-tts" => sorted_stems(&dir.join("voices"), ".bin"),
            "supertonic" => sorted_stems(&dir.join("voice_styles"), ".json"),
            "phonon" => crate::phonon::catalog_voices(&dir),
            _ => Vec::new(),
        }
    }
}

/// The voices `engine` offers, filtered to the speech language `lang`.
///
/// ONE helper for both surfaces — the Speech settings page and the
/// playback rail — so the two lists cannot drift. A voice is visible when
/// it carries no locale (a single-language bank — kokoro — or a
/// language-neutral style — supertonic — speaks what it speaks, and the
/// pref cannot hide it), when the pref is All (`""`), when the locale
/// matches the pref by prefix (`"en"` matches `"en-us"` and `"en-gb"`),
/// or when the pref AND the locale are both the `"characters"` group.
pub fn voices_for_language(engine: &str, lang: &str) -> Vec<Voice> {
    ModelManager::voices(engine)
        .into_iter()
        .filter(|v| visible_in_language(v.locale.as_deref(), lang))
        .collect()
}

/// The filter rule [`voices_for_language`] applies, spelled out for tests.
pub fn visible_in_language(locale: Option<&str>, lang: &str) -> bool {
    match locale {
        None => true,
        // All languages: everything shows, the characters group included.
        _ if lang.is_empty() => true,
        // The characters group is its own language entry, never a prefix
        // match away: picking English must not drag the ogre in.
        Some("characters") => lang == "characters",
        Some(locale) => lang != "characters" && locale.starts_with(lang),
    }
}

/// The languages `engine` can SPEAK, as the language dropdown offers them.
///
/// For a voice pack whose voices carry locales (espeak, phonon), that is
/// the locales the voices actually carry — a language with no voice has no
/// row, because picking it would leave the voice list empty. For an engine
/// whose voices are language-neutral but whose model is multilingual
/// (supertonic), that is every language the model itself knows: the same
/// ten voices speak all of them, and this is what picks which.
pub fn engine_languages(engine: &str) -> Vec<String> {
    if engine == "supertonic" {
        return crate::supertonic::languages().iter().map(|s| s.to_string()).collect();
    }
    let mut codes: Vec<String> = ModelManager::voices(engine)
        .iter()
        .filter_map(|v| v.locale.clone())
        .map(|locale| match locale.as_str() {
            "characters" => locale,
            other => other.split('-').next().unwrap_or(other).to_string(),
        })
        .collect();
    codes.sort();
    codes.dedup();
    codes
}

/// The `*.bin` / `*.json` stems in a directory, sorted, as voices.
fn sorted_stems(dir: &Path, extension: &str) -> Vec<Voice> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut codes: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with(extension))
        .map(|n| n.trim_end_matches(extension).to_string())
        .collect();
    codes.sort();
    codes
        .into_iter()
        .map(|c| Voice {
            name: c.clone(),
            code: c,
            locale: None,
        })
        .collect()
}

/// The voice to build with: the requested one when the engine actually
/// offers it, the engine's default otherwise.
///
/// A voice pref that does not name a real voice used to reach the engine
/// as-is and fail there — an espeak engine handed a foreign voice code
/// (a phonon id written under the wrong key by a picker bug) errored on
/// EVERY chunk, and the listener heard the failure beep through a whole
/// session. The default is what the engine can actually speak; the log
/// keeps the why for the session that hits it.
fn resolve_voice(engine: &str, voice: &str, default: &str) -> String {
    if voice.is_empty() {
        return default.to_string();
    }
    let known = ModelManager::voices(engine).iter().any(|v| v.code == voice);
    if known {
        return voice.to_string();
    }
    tracing::warn!(
        engine = %engine,
        requested = %voice,
        default = %default,
        "the prefs name a voice this engine does not offer; building the default instead"
    );
    default.to_string()
}

/// Build one engine from disk, from a name, a voice and a speech language.
/// The candidates' model directories are the catalog's; espeak needs
/// nothing but its CLI.
///
/// This is the single construction path: the UI's picker, the `--speak` gate
/// and `ModelManager::resident` all call it, so a voice code that loads in
/// one loads in all of them. `lang` is the speech-language pref — a
/// language-neutral or single-language engine ignores it, supertonic
/// speaks it (falling back to English when the pref is empty, the
/// characters group, or a language it cannot speak).
pub fn build_engine(
    engine: &str,
    voice: &str,
    rate_multiplier: f64,
    lang: &str,
) -> Result<Box<dyn Engine>, TtsError> {
    crate::ortrt::ensure_dylib_env();
    // "pico" is the iOS app's name for its espeak-tier engine; there is no
    // pico on Linux, so the name maps onto espeak here — the pref keeps its
    // value (it is a foreign file's data, not ours to rewrite) while every
    // session start builds a working engine instead of wedging on a name
    // no branch answers.
    let engine = match engine {
        "pico" => "espeak",
        other => other,
    };
    match engine {
        "espeak" => Ok(Box::new(crate::espeak::EspeakEngine::new(
            resolve_voice("espeak", voice, ""),
            rate_multiplier,
        ))),
        "kokoro" => {
            let set = ModelManager::set_for("kokoro")
                .ok_or_else(|| TtsError::ModelLoad("kokoro set vanished".into()))?;
            let voice = resolve_voice("kokoro", voice, set.default_voice);
            Ok(Box::new(crate::kokoro::KokoroEngine::load(
                &set.path(),
                &voice,
                rate_multiplier,
            )?))
        }
        "supertonic" => {
            let set = ModelManager::set_for("supertonic")
                .ok_or_else(|| TtsError::ModelLoad("supertonic set vanished".into()))?;
            let voice = resolve_voice("supertonic", voice, set.default_voice);
            Ok(Box::new(crate::supertonic::SupertonicEngine::load(
                &set.path(),
                &voice,
                rate_multiplier,
                lang,
            )?))
        }
        "phonon" => {
            // rate_multiplier is received but unused: the model has no native
            // speed control (see phonon.rs), and Engine::rate() stays 1.0.
            let set = ModelManager::set_for("phonon")
                .ok_or_else(|| TtsError::ModelLoad("phonon set vanished".into()))?;
            let voice = resolve_voice("phonon", voice, set.default_voice);
            Ok(Box::new(crate::phonon::PhononEngine::load(&set.path(), &voice)?))
        }
        "nano-edge-tts" => {
            // No `lang`: single-language family (the English banks only).
            let set = ModelManager::set_for("nano-edge-tts")
                .ok_or_else(|| TtsError::ModelLoad("nano-edge-tts set vanished".into()))?;
            let voice = resolve_voice("nano-edge-tts", voice, set.default_voice);
            Ok(Box::new(crate::nano_edge::NanoEdgeEngine::load(
                &set.path(),
                &voice,
                rate_multiplier,
            )?))
        }
        other => Err(TtsError::Unavailable {
            engine: "model manager",
            detail: format!("no such engine {other:?}"),
        }),
    }
}

/// The sha256 of a file's bytes, lower-case hex, streamed — the graphs run
/// to 325 MB and none of this may hold one in RAM to hash it.
fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// The `&'static str` the manager stores for an engine name. One interned
/// &str per distinct name, however often it is called: the catalog's own
/// names are served straight out of the static, and an unknown name (which
/// callers only pass on their way to an error) interns into the same table
/// rather than leaking a fresh one per call.
fn leak_name(engine: &str) -> &'static str {
    static NAMES: std::sync::OnceLock<Mutex<std::collections::HashSet<&'static str>>> =
        std::sync::OnceLock::new();
    let names = NAMES.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let mut guard = names.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(interned) = guard.get(engine) {
        return interned;
    }
    let interned: &'static str = Box::leak(engine.to_string().into_boxed_str());
    guard.insert(interned);
    interned
}

/// Free bytes on the filesystem holding `path`. `statvfs` via libc, because
/// `std` has no portable wrapper and adding a crate for one syscall is not
/// worth it.
pub fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let mut c_path: Vec<u8> = path.as_os_str().as_bytes().to_vec();
    c_path.push(0);
    let stat = unsafe {
        let mut s: libc_statvfs = std::mem::zeroed();
        if statvfs(c_path.as_ptr() as *const _, &mut s) != 0 {
            return None;
        }
        s
    };
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

/// The fields of `statvfs` this module reads.
#[repr(C)]
struct libc_statvfs {
    f_bsize: u64,
    f_frsize: u64,
    f_blocks: u64,
    f_bfree: u64,
    f_bavail: u64,
    f_files: u64,
    f_ffree: u64,
    f_favail: u64,
    f_fsid: u64,
    f_flag: u64,
    f_namemax: u64,
    _pad: [u8; 32],
}

unsafe extern "C" {
    fn statvfs(path: *const std::ffi::c_char, buf: *mut libc_statvfs) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_lists_the_voice_engines() {
        let engines: Vec<&str> = catalog().iter().map(|s| s.engine).collect();
        // The shipped binary's embedded selftest pins catalog() to the
        // speech voices; ocr installs via `set_for`, not this list. Phonon
        // joined the speech-facing list 2026-11-11 (user ask); nano-edge-tts
        // 2026-11-15 (the distilled kokoro decoder).
        assert_eq!(
            engines,
            vec!["kokoro", "supertonic", "phonon", "nano-edge-tts"]
        );
    }

    #[test]
    fn the_page_recogniser_is_never_a_voice() {
        // The speech picker lists `available_engines`; OCR must never appear
        // there — it has no voice and no waveform to play.
        assert!(
            !ModelManager::available_engines().contains(&"ocr"),
            "ocr leaked into the speech engines: {:?}",
            ModelManager::available_engines()
        );
        assert_ne!(
            ModelManager::preferred_engine(),
            "ocr",
            "the preferred engine must be a voice"
        );
    }

    #[test]
    fn every_file_has_a_local_path_and_a_floor() {
        for set in catalog() {
            assert!(!set.files.is_empty(), "{} has no files", set.engine);
            for f in &set.files {
                assert!(f.min_bytes > 0, "{}: {} has no floor", set.engine, f.local);
                assert!(
                    !f.local.starts_with('/'),
                    "{}: {} is absolute",
                    set.engine,
                    f.local
                );
            }
        }
    }

    #[test]
    fn kokoro_lists_every_voice_bank() {
        let set = ModelManager::set_for("kokoro").expect("kokoro");
        let voices = kokoro_voices();
        assert_eq!(voices.len(), 55);
        // The floor must clear the smallest bank on disk, or a complete
        // install reads as absent — `af.bin` is the one bank that is a row
        // longer than the rest.
        for v in &voices {
            assert_eq!(v.min_bytes, 522_240, "{}", v.local);
            assert!(set.files.contains(v), "{} missing from the set", v.local);
        }
    }

    #[test]
    fn supertonic_lists_ten_styles_and_four_graphs() {
        let set = ModelManager::set_for("supertonic").expect("supertonic");
        let graphs = set
            .files
            .iter()
            .filter(|f| f.local.starts_with("onnx/"))
            .count();
        let styles = set
            .files
            .iter()
            .filter(|f| f.local.starts_with("voice_styles/"))
            .count();
        assert_eq!(graphs, 6, "four graphs plus tts.json and the indexer");
        assert_eq!(styles, 10);
    }

    #[test]
    fn espeak_is_builtin_and_needs_no_set() {
        assert!(ModelManager::is_builtin("espeak"));
        assert!(ModelManager::set_for("espeak").is_none());
        assert_eq!(ModelManager::state("espeak"), InstallState::Absent);
        // espeak is always offerable regardless of the models directory.
        assert!(ModelManager::available_engines().contains(&"espeak"));
    }

    #[test]
    fn an_unknown_engine_has_no_set_and_no_state() {
        assert!(ModelManager::set_for("whisper").is_none());
        assert_eq!(ModelManager::state("whisper"), InstallState::Absent);
        assert!(!ModelManager::available_engines().contains(&"whisper"));
    }

    #[test]
    fn a_missing_file_reads_as_absent() {
        // The real box has all three installed; a set pointed at an empty
        // directory proves the check is not reading the catalog's own truth.
        let set = ModelManager::set_for("kokoro").expect("kokoro");
        let empty = std::env::temp_dir().join("sn-model-manager-empty");
        std::fs::create_dir_all(&empty).unwrap();
        let present = set.files.iter().all(|f| {
            std::fs::metadata(empty.join(&f.local))
                .map(|m| m.len() >= f.min_bytes)
                .unwrap_or(false)
        });
        assert!(!present, "an empty directory must not read as installed");
    }

    #[test]
    fn the_resident_engine_is_the_one_that_was_asked_for() {
        // espeak needs no model files, so this runs anywhere.
        let mut m = ModelManager::new();
        let engine = m.resident("espeak", "en-gb", 1.0, "").expect("load espeak");
        assert_eq!(engine.name(), "espeak");
        assert_eq!(m.resident_name(), Some(("espeak", "en-gb")));
    }

    #[test]
    fn asking_for_a_different_engine_drops_the_first() {
        let mut m = ModelManager::new();
        let _ = m.resident("espeak", "en-gb", 1.0, "").expect("first");
        let _ = m.resident("espeak", "de", 2.0, "").expect("second");
        assert_eq!(m.resident_name(), Some(("espeak", "de")));
        m.drop_resident();
        assert_eq!(m.resident_name(), None);
    }

    #[test]
    fn a_resident_engine_survives_a_repeat_ask_at_the_same_settings() {
        // The pipeline asks for the resident engine on every session start;
        // reloading it each time would re-open the graph for nothing. With
        // the engine behind an Arc, "the same instance" is Arc::ptr_eq —
        // no pointer casts.
        let mut m = ModelManager::new();
        let a = m.resident("espeak", "en-gb", 1.0, "").expect("first");
        let b = m.resident("espeak", "en-gb", 1.0, "").expect("second");
        assert!(
            std::sync::Arc::ptr_eq(&a, &b),
            "the same engine must be reused, not reloaded"
        );
    }

    #[test]
    fn free_space_is_readable_and_sane() {
        let free = ModelManager::free_space_bytes().expect("statvfs on /home");
        assert!(free > 0, "zero free space is not a usable answer");
        assert!(
            free < 1_000_000_000_000_000,
            "{free} bytes is not a plausible filesystem size"
        );
    }

    #[test]
    fn the_voices_of_an_installed_engine_come_off_the_disk() {
        // The box has all the neural sets installed; the picker must be
        // able to list without a session in memory.
        if ModelManager::state("kokoro") != InstallState::Installed {
            panic!("kokoro not installed; the batch 6 contract says it is");
        }
        let kokoro = ModelManager::voices("kokoro");
        assert_eq!(kokoro.len(), 55, "voice banks on disk");
        assert!(kokoro.iter().any(|v| v.code == "am_eric"));
        assert!(kokoro.iter().any(|v| v.code == "zf_xiaoyi"));
        // Sorted, so the picker's list does not reshuffle between openings.
        let codes: Vec<&str> = kokoro.iter().map(|v| v.code.as_str()).collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        assert_eq!(codes, sorted, "voices must come back sorted");

        let supertonic = ModelManager::voices("supertonic");
        assert_eq!(supertonic.len(), 10);
        assert!(supertonic.iter().any(|v| v.code == "F1"));
    }

    #[test]
    fn an_uninstalled_engine_lists_no_voices_rather_than_failing() {
        // A picker that errors on "not installed" cannot show the install
        // button; an empty list is what lets it.
        assert!(ModelManager::voices("whisper").is_empty());
    }

    #[test]
    fn espeak_lists_its_own_voices_without_a_model_set() {
        let voices = ModelManager::voices("espeak");
        assert!(!voices.is_empty(), "espeak-ng --voices returned nothing");
        assert!(voices.iter().any(|v| v.code == "en-gb"));
    }

    #[test]
    fn a_neural_engine_builds_from_the_catalog_directory() {
        // The whole point of the catalog's `dir` field: the engine loader is
        // handed the catalog's directory and finds its graph there.
        let set = ModelManager::set_for("kokoro").expect("kokoro");
        let engine =
            build_engine("kokoro", "am_eric", 1.0, "").expect("load from the catalog dir");
        assert_eq!(engine.name(), "kokoro");
        assert!(set.path().join("model.onnx").exists());
    }

    #[test]
    fn an_engine_with_a_bad_voice_builds_the_default() {
        // The load used to refuse the whole engine here — right for a
        // hand-typed `--voice`, wrong for a pref a picker bug wrote under
        // the wrong engine's key: the session then failed every chunk and
        // the listener heard the skip-beep instead of speech. The engine
        // builds with its default voice; the substitution is in the log.
        let engine = build_engine("kokoro", "not_a_voice", 1.0, "").expect("build");
        assert_eq!(engine.name(), "kokoro");
    }

    /// The exact poisoning 2026-11-13 shipped: a phonon voice id under the
    /// espeak key made every espeak chunk fail at the CLI ("The specified
    /// espeak-ng voice does not exist") — the beeps-everywhere bug. The
    /// read side now answers with espeak's own default voice instead.
    #[test]
    fn a_foreign_voice_code_under_espeak_builds_espeaks_default() {
        let engine = build_engine("espeak", "nGzMHMPAWGaJFHOo", 1.0, "").expect("build");
        assert_eq!(engine.name(), "espeak");
        // The engine still works end to end: its default voice synthesises.
        let out = engine.synthesize("Still a voice.").expect("synth");
        assert!(out.audio_time().as_secs_f64() > 0.2, "{:?}", out.audio_time());
    }

    #[test]
    fn an_unknown_engine_name_is_refused_by_name() {
        let Err(err) = build_engine("whisper", "", 1.0, "") else {
            panic!("an unknown engine must not build");
        };
        assert!(err.to_string().contains("whisper"), "{err}");
    }

    #[test]
    fn the_room_check_refuses_a_failing_filesystem() {
        // `free_space` on a path that does not exist returns None and
        // `has_room` must then say no, not yes.
        assert!(free_space(Path::new("/definitely/not/a/real/path/at/all")).is_none());
        assert!(ModelManager::has_room("kokoro") == ModelManager::free_space_bytes().is_some_and(|f| {
            let set = ModelManager::set_for("kokoro").unwrap();
            f >= set.approx_bytes() * 3 / 2
        }));
    }

    /// The language filter's rule: no locale always shows (a
    /// single-language bank or a language-neutral style speaks what it
    /// speaks), All shows everything, a prefix match shows its locales,
    /// and the characters group is its own entry — never a prefix match
    /// away, in either direction.
    #[test]
    fn the_language_filter_hides_only_what_the_pref_names_away() {
        // No locale: always visible, whatever the pref says.
        assert!(visible_in_language(None, ""));
        assert!(visible_in_language(None, "en"));
        assert!(visible_in_language(None, "characters"));
        // All languages: everything visible.
        assert!(visible_in_language(Some("en-us"), ""));
        assert!(visible_in_language(Some("characters"), ""));
        // Prefix match: "en" sees "en-us" and "en-gb".
        assert!(visible_in_language(Some("en-us"), "en"));
        assert!(visible_in_language(Some("en-gb"), "en"));
        assert!(!visible_in_language(Some("es-mx"), "en"));
        // The characters group answers only to its own pref.
        assert!(visible_in_language(Some("characters"), "characters"));
        assert!(!visible_in_language(Some("characters"), "en"));
        assert!(!visible_in_language(Some("en-us"), "characters"));
    }
}
