//! The ONNX Runtime wrapper: dylib discovery, session loading, tensor
//! plumbing.
//!
//! The `ort` crate is compiled with `load-dynamic`, so the runtime is found
//! at start-up from `ORT_DYLIB_PATH` or this box's known install. Everything
//! the engines need sits here so the engine files read as graphs, not as
//! FFI. Runs take `&self` (an ORT session is thread-safe in the C API; the
//! guard inside is uncontended — one synth thread feeds it) and extract the
//! wanted outputs *inside* the lock, because an output that borrows the
//! session would tie the engines' hands for nothing: every consumer here
//! wants a flat f32 vector and nothing else.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once};

use ort::value::{DynValue, Tensor};

use crate::TtsError;

/// The runtime install. `ORT_DYLIB_PATH` overrides, for a machine that
/// keeps it elsewhere. Otherwise the first existing file among the package
/// layouts wins: the system package (`/usr/lib/speechnotes`), the
/// user-local install (`~/.local/lib/speechnotes`), and the bare
/// onnxruntime release layout older installs used.
pub fn dylib_path() -> PathBuf {
    if let Some(path) = std::env::var_os("ORT_DYLIB_PATH") {
        return PathBuf::from(path);
    }
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("/usr/lib/speechnotes/libonnxruntime.so"),
    ];
    if let Some(appdir) = std::env::var_os("APPDIR") {
        let mut p = PathBuf::from(appdir);
        p.push("usr/lib/speechnotes/libonnxruntime.so");
        candidates.push(p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut home = PathBuf::from(home);
        home.push(".local/lib/speechnotes/libonnxruntime.so");
        candidates.push(home);
        // The bare onnxruntime release layout older installs used.
        let mut legacy = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        legacy.push(".local/share/onnxruntime-linux-x64-1.30.0/lib/libonnxruntime.so");
        candidates.push(legacy);
    }
    candidates
        .into_iter()
        .find(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("/usr/lib/speechnotes/libonnxruntime.so"))
}

/// `ort` reads the dylib path once per process from this env var at the
/// first use of the API. Set it if it is not already set.
pub fn ensure_dylib_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if std::env::var_os("ORT_DYLIB_PATH").is_none() {
            std::env::set_var("ORT_DYLIB_PATH", dylib_path());
        }
    });
}

/// A loaded ONNX session. Owns the graph; drop it and the memory goes back —
/// that drop is the single-resident-model discipline's whole mechanism.
pub struct Session {
    inner: Mutex<ort::session::Session>,
}

impl Session {
    /// Load a graph. Intra-op threading stays at the runtime's default: the
    /// graphs here are small, and the pipeline's parallelism already lives
    /// in sn-tts's own threads.
    pub fn load(path: &Path) -> Result<Self, TtsError> {
        ensure_dylib_env();
        let inner = ort::session::Session::builder()
            .map_err(ort_err)?
            .commit_from_file(path)
            .map_err(|e| TtsError::ModelLoad(format!("{}: {e}", path.display())))?;
        Ok(Self {
            inner: Mutex::new(inner),
        })
    }

    /// Does this graph have an output called `name`? Kokoro exports differ
    /// on the output name (`waveform` vs `audio`); resolve instead of assume.
    pub fn has_output(&self, name: &str) -> bool {
        self.lock().outputs().iter().any(|o| o.name() == name)
    }

    /// The first output's name — the fallback when a known one is absent.
    pub fn first_output(&self) -> String {
        self.lock()
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .unwrap_or_default()
    }

    /// Run with named tensors and extract the named outputs as flat f32
    /// vectors, in `wanted` order. Anything the graph returns that wasn't
    /// asked for is dropped by the runtime.
    pub fn run_f32(
        &self,
        inputs: Vec<(&str, DynValue)>,
        wanted: &[&str],
    ) -> Result<Vec<Vec<f32>>, TtsError> {
        let mut guard = self.lock();
        let mut outputs = guard
            .run(inputs)
            .map_err(|e| TtsError::ModelLoad(format!("inference: {e}")))?;
        let mut taken = Vec::with_capacity(wanted.len());
        for name in wanted {
            let value = outputs
                .remove(name)
                .ok_or_else(|| TtsError::Synthesis(format!("output {name} missing")))?;
            let (_shape, data) = value.try_extract_tensor::<f32>().map_err(ort_err)?;
            taken.push(data.to_vec());
        }
        Ok(taken)
    }

    /// The same run with each wanted tensor's shape beside its data — the
    /// two-graph engines reshape one session's outputs into the next one's
    /// inputs, and a flat vector cannot say what rank it had.
    pub fn run_f32_shaped(
        &self,
        inputs: Vec<(&str, DynValue)>,
        wanted: &[&str],
    ) -> Result<Vec<(Vec<i64>, Vec<f32>)>, TtsError> {
        let mut guard = self.lock();
        let mut outputs = guard
            .run(inputs)
            .map_err(|e| TtsError::ModelLoad(format!("inference: {e}")))?;
        let mut taken = Vec::with_capacity(wanted.len());
        for name in wanted {
            let value = outputs
                .remove(name)
                .ok_or_else(|| TtsError::Synthesis(format!("output {name} missing")))?;
            let (shape, data) = value.try_extract_tensor::<f32>().map_err(ort_err)?;
            taken.push((shape.to_vec(), data.to_vec()));
        }
        Ok(taken)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ort::session::Session> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// An i64 tensor, rank per `dims`. Row-major, like everything here.
pub fn tensor_i64(dims: &[i64], data: Vec<i64>) -> Result<DynValue, TtsError> {
    Tensor::from_array((dims.to_vec(), data))
        .map(|t| t.into_dyn())
        .map_err(ort_err)
}

/// An f32 tensor, rank per `dims`. Row-major, like everything here.
pub fn tensor_f32(dims: &[i64], data: Vec<f32>) -> Result<DynValue, TtsError> {
    Tensor::from_array((dims.to_vec(), data))
        .map(|t| t.into_dyn())
        .map_err(ort_err)
}

fn ort_err(e: ort::Error) -> TtsError {
    TtsError::ModelLoad(format!("onnxruntime: {e}"))
}
