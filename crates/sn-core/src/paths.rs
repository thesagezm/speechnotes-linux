//! XDG paths and logging setup.
//!
//! The Swift port re-read `XDG_*_HOME` on every single access, which is what
//! let its Linux tests set `XDG_DATA_HOME` per test. We do the same: reading
//! the environment per call costs nothing and it is the difference between a
//! testable store and one that needs a real home directory.

use std::path::{Path, PathBuf};

/// Env overrides are only honoured when absolute. A relative `XDG_DATA_HOME`
/// silently resolving against the cwd is a classic footgun and the Swift port
/// documented the rule but implemented the check only on some paths.
fn env_dir(key: &str) -> Option<PathBuf> {
    let raw = std::env::var_os(key)?;
    if raw.is_empty() {
        return None;
    }
    let p = PathBuf::from(raw);
    p.is_absolute().then_some(p)
}

/// `~/.local/share/speechnotes` (or `$XDG_DATA_HOME/speechnotes`)
pub fn data_dir() -> PathBuf {
    env_dir("XDG_DATA_HOME")
        .unwrap_or_else(|| home().join(".local/share"))
        .join("speechnotes")
}

/// `~/.cache/speechnotes` (or `$XDG_CACHE_HOME/speechnotes`)
pub fn cache_dir() -> PathBuf {
    env_dir("XDG_CACHE_HOME")
        .unwrap_or_else(|| home().join(".cache"))
        .join("speechnotes")
}

/// `~/.config/speechnotes` (or `$XDG_CONFIG_HOME/speechnotes`)
pub fn config_dir() -> PathBuf {
    env_dir("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home().join(".config"))
        .join("speechnotes")
}

/// `$HOME`, or `/` when it cannot be read at all.
pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `data_dir()/Books`
pub fn books_dir() -> PathBuf {
    data_dir().join("Books")
}

/// `data_dir()/models`
pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

/// `data_dir()/Exports`
pub fn exports_dir() -> PathBuf {
    data_dir().join("Exports")
}

/// `data_dir()/note-images`
pub fn note_images_dir() -> PathBuf {
    data_dir().join("note-images")
}

/// `cache_dir()/tts` — per-run synthesised chunk WAVs. Pure scratch: safe to
/// delete at any time, and always cleared at the start of a run because resume
/// renumbers chunks.
pub fn tts_cache_dir() -> PathBuf {
    cache_dir().join("tts")
}

/// `data_dir()/speechnotes.log`
pub fn log_file() -> PathBuf {
    data_dir().join("speechnotes.log")
}

/// `data_dir()/notes.json`
pub fn notes_file() -> PathBuf {
    // A test-flood guard: one session leaked a 5000-note test fixture into
    // the user's live library, and the app's startup persist then rewrote
    // the flooded content. In test builds, a caller that reaches the real
    // home — no XDG_DATA_HOME set by a fixture — is a test that forgot its
    // fixture, and this names the mistake at the first touch instead of
    // after the file is written. The shipped binary is unaffected: cfg(test)
    // does not exist outside `cargo test`.
    #[cfg(test)]
    if std::env::var_os("XDG_DATA_HOME").is_none() {
        panic!(
            "notes_file() reached the real home from a test — every store \
             test must hold the XdgFixture (see sn-store::notes::test_support)"
        );
    }
    data_dir().join("notes.json")
}

/// `data_dir()/notebooks.json`
pub fn notebooks_file() -> PathBuf {
    data_dir().join("notebooks.json")
}

/// `data_dir()/bookmarks.json`
pub fn bookmarks_file() -> PathBuf {
    data_dir().join("bookmarks.json")
}

/// `data_dir()/stats.json`
pub fn stats_file() -> PathBuf {
    data_dir().join("stats.json")
}

/// `config_dir()/prefs.json`
pub fn prefs_file() -> PathBuf {
    config_dir().join("prefs.json")
}

/// Create every directory the app writes into. Idempotent. Called at launch and
/// at the top of each test so a store never has to think about it.
pub fn ensure_directories() -> std::io::Result<()> {
    for dir in [
        data_dir(),
        cache_dir(),
        config_dir(),
        books_dir(),
        models_dir(),
        exports_dir(),
        note_images_dir(),
        tts_cache_dir(),
    ] {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(())
}

/// Create `dir`, treating an existing *file* at that path as an error rather
/// than silently succeeding — the Swift port's `ensureRunDir` bug was exactly
/// this: the run directory was never created, every WAV write failed, and the
/// error surfaced as a read failure with no mention of the missing folder.
pub fn ensure_dir_or_report(dir: &Path) -> Result<(), std::io::Error> {
    match std::fs::metadata(dir) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} exists but is not a directory", dir.display()),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir_all(dir),
        Err(e) => Err(e),
    }
}

/// A guard that serialises the process-global XDG variables.
///
/// `XDG_DATA_HOME` and its siblings are process-global, and `cargo test` runs a
/// crate's tests as parallel threads of one process. Two fixtures repointing
/// each other's roots at once is how a store test ends up asserting on a
/// directory another test just deleted — the failure looks like a store bug and
/// is not one.
///
/// Other crates' test fixtures hold this too, so the two crates take turns
/// rather than each guarding only itself. Production code does not need it:
/// paths are read per access and nothing else writes those variables.
pub fn xdg_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}
