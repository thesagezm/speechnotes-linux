//! The notes store.
//!
//! One file, `notes.json`, an array, written by a background writer that
//! coalesces bursts. The design rule that decides everything here: **the UI
//! thread never encodes, never writes, never sorts the corpus.** It mutates the
//! in-memory list, bumps a version, and hands bytes to the writer. A 5 000-note
//! library saves in the time it takes to enqueue a `Vec<u8>`, so typing is
//! never gated on disk.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use sn_core::atomic::{DebouncedWriter, LoadOutcome, SharedWriter, WriterThread};
use sn_core::paths;
use sn_core::AppleDate;

use crate::model::{Note, PlaybackBookmark, RECYCLE_RETENTION_DAYS};

/// How long text edits wait before a write lands. Matches the Swift port so
/// either app's cadence assumptions hold.
pub const TEXT_SAVE_DEBOUNCE: Duration = Duration::from_millis(400);

/// A loaded library, plus what the loader had to do to get it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadReport {
    Clean,
    RecoveredFromBackup,
    /// The file existed but nothing readable came out of it, and there was no
    /// backup. The store is usable and empty, but the user should be told.
    Unreadable,
    Quarantined(PathBuf),
    Fresh,
}

impl LoadReport {
    /// Interpret a load outcome, given whether the file existed before the
    /// attempt.
    ///
    /// `LoadOutcome::Empty` is ambiguous: it means "nothing usable was loaded",
    /// which for a missing file is a fresh start but for a file that *existed*
    /// and could not be parsed is data the user should hear about. Collapsing
    /// the two into `Fresh` is how a broken library looks like a brand-new one.
    pub fn from_outcome(outcome: &LoadOutcome, existed: bool) -> Self {
        match outcome {
            LoadOutcome::Primary => LoadReport::Clean,
            LoadOutcome::Backup => LoadReport::RecoveredFromBackup,
            LoadOutcome::Quarantined(p) => LoadReport::Quarantined(p.clone()),
            LoadOutcome::Empty if !existed => LoadReport::Fresh,
            LoadOutcome::Empty => LoadReport::Unreadable,
        }
    }

    pub fn is_problem(&self) -> bool {
        !matches!(self, LoadReport::Clean | LoadReport::Fresh)
    }
}

/// The in-memory note library.
pub struct NotesStore {
    /// `RwLock`, not `&mut self`: the store is shared with background threads
    /// (the player, the importer) and the UI thread, and reads outnumber writes
    /// by a wide margin — a list render takes the read lock hundreds of times
    /// per frame against a handful of writes.
    /// Shared with the writer thread, which encodes the library when a debounced
    /// save fires. Cloning the `Arc` is free; cloning the `Vec` is not.
    notes: std::sync::Arc<RwLock<Vec<Note>>>,
    /// Bumped on every mutation, so a view can memoise a filtered result set
    /// and rebuild it only when the library actually changed.
    version: AtomicU64,
    writer: SharedWriter,
    thread: WriterThread,
    /// Notes purged since the last save, so the caller can drop their images
    /// and bookmarks without the store knowing about either.
    purged: Mutex<Vec<String>>,
    load_report: LoadReport,
    /// Set when a write fails, surfaced once, because a silent failure means a
    /// user closes the app believing their note was saved.
    save_error: std::sync::Arc<Mutex<Option<String>>>,
    /// How many [`Self::batch_saves`] guards are live. While it is above zero
    /// every save is skipped; the last guard to drop writes the library once.
    defer_depth: AtomicUsize,
}

impl NotesStore {
    /// Open the library at the default location.
    pub fn open() -> std::io::Result<Self> {
        let path = paths::notes_file();
        let existed = path.exists();
        let (notes, outcome) =
            sn_core::atomic::load_json_recoverable::<Vec<Note>>(&path);
        let (writer, thread) =
            DebouncedWriter::new(path.clone(), TEXT_SAVE_DEBOUNCE)?.share();
        let mut store = Self {
            notes: std::sync::Arc::new(RwLock::new(notes)),
            version: AtomicU64::new(1),
            writer,
            thread,
            purged: Mutex::new(Vec::new()),
            load_report: LoadReport::from_outcome(&outcome, existed),
            save_error: std::sync::Arc::new(Mutex::new(None)),
            defer_depth: AtomicUsize::new(0),
        };
        store.expire_recycle_bin();
        store.persist(false);
        Ok(store)
    }

    /// Build a store from an explicit list, for tests and imports.
    pub fn from_notes(notes: Vec<Note>) -> std::io::Result<Self> {
        let path = paths::notes_file();
        let (writer, thread) =
            DebouncedWriter::new(path, TEXT_SAVE_DEBOUNCE)?.share();
        let mut store = Self {
            notes: std::sync::Arc::new(RwLock::new(notes)),
            version: AtomicU64::new(1),
            writer,
            thread,
            purged: Mutex::new(Vec::new()),
            load_report: LoadReport::Fresh,
            save_error: std::sync::Arc::new(Mutex::new(None)),
            defer_depth: AtomicUsize::new(0),
        };
        // A store built in memory behaves like a loaded one: binned notes past
        // retention go, and the first save lands. Skipping this here is why the
        // retention test failed — `open` expired, `from_notes` did not.
        store.expire_recycle_bin();
        store.persist(false);
        Ok(store)
    }

    pub fn load_report(&self) -> &LoadReport {
        &self.load_report
    }

    /// Every active (not binned) note, oldest position first.
    pub fn active(&self) -> Vec<Note> {
        self.notes.read().unwrap().iter().filter(|n| !n.is_deleted()).cloned().collect()
    }

    /// Every binned note, including those past retention.
    pub fn binned(&self) -> Vec<Note> {
        self.notes.read().unwrap().iter().filter(|n| n.is_deleted()).cloned().collect()
    }

    /// Active notes whose bin stamp is inside the retention window.
    pub fn binned_active(&self) -> Vec<Note> {
        let cutoff = AppleDate::now().plus_days(-RECYCLE_RETENTION_DAYS);
        self.notes
            .read()
            .unwrap()
            .iter()
            .filter(|n| n.deleted_at.map(|d| d.raw() > cutoff.raw()).unwrap_or(false))
            .cloned()
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Note> {
        self.notes.read().unwrap().iter().find(|n| n.id == id).cloned()
    }

    pub fn len(&self) -> usize {
        self.notes.read().unwrap().iter().filter(|n| !n.is_deleted()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    /// Notes purged since the last call, for the caller to clean up.
    pub fn take_purged(&self) -> Vec<String> {
        self.purged.lock().unwrap().drain(..).collect()
    }

    /// A save error, if one happened since the last call.
    ///
    /// Two failures count: the library could not be **encoded** (a note that
    /// will not serialise) and it could not be **written** (the disk said
    /// no). The second was missing, so a full disk or a read-only directory
    /// lost the user's notes with nothing but a log line to show for it.
    pub fn take_save_error(&self) -> Option<String> {
        // The write failure first: it is the one the user must know about.
        let written = sn_core::atomic::take_write_error(&paths::notes_file());
        let encoded = self.save_error.lock().unwrap().take();
        match (written, encoded) {
            (Some(w), Some(e)) => Some(format!("{w} ({e})")),
            (Some(w), None) => Some(w),
            (None, Some(e)) => Some(e),
            (None, None) => None,
        }
    }

    /// Create a note, filed into `notebook_id` when given.
    pub fn create(&self, notebook_id: Option<String>) -> Note {
        let note = Note::new(notebook_id);
        self.notes.write().unwrap().push(note.clone());
        self.bump();
        self.persist(true);
        note
    }

    /// Insert a note with an id already fixed — imports, and nothing else.
    ///
    /// The id is trusted: an import that collides is skipped by the caller.
    pub fn insert(&self, note: Note) {
        self.notes.write().unwrap().push(note);
        self.bump();
        self.persist(true);
    }

    /// Apply `f` to a note and save.
    ///
    /// Returns the updated note. Editing text uses [`Self::update_text`], which
    /// coalesces; everything here saves immediately, because a pin or a delete
    /// is a user-visible state change and losing it to a debounce is worse than
    /// an extra write.
    pub fn update<F>(&self, id: &str, f: F) -> Option<Note>
    where
        F: FnOnce(&mut Note),
    {
        let mut notes = self.notes.write().unwrap();
        let note = notes.iter_mut().find(|n| n.id == id)?;
        f(note);
        note.updated_at = AppleDate::now();
        let out = note.clone();
        drop(notes);
        self.bump();
        self.persist(true);
        Some(out)
    }

    /// Change a note's text, saving on the debounce rather than immediately.
    ///
    /// **This is the one mutation that must not clear the playback bookmark.**
    /// The Swift store deleted it on every edit, so a user who corrected a
    /// typo while listening lost their place. Whether the bookmark survives is
    /// the player's decision, made by comparing text hashes — the store just
    /// reports the change through [`Self::version`].
    pub fn update_text(&self, id: &str, text: &str) -> Option<Note> {
        {
            let mut notes = self.notes.write().unwrap();
            let note = notes.iter_mut().find(|n| n.id == id)?;
            if note.text == text {
                return Some(note.clone());
            }
            note.text = text.to_string();
            note.updated_at = AppleDate::now();
        }
        self.bump();
        self.persist(false);
        self.get(id)
    }

    /// Set or clear the derived title. A blank string clears it.
    pub fn set_title(&self, id: &str, title: &str) -> Option<Note> {
        self.update(id, |n| {
            n.explicit_title = if title.trim().is_empty() {
                None
            } else {
                Some(title.trim().to_string())
            };
        })
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Option<Note> {
        self.update(id, |n| n.is_pinned = pinned)
    }

    pub fn set_favorite(&self, id: &str, favorite: bool) -> Option<Note> {
        self.update(id, |n| n.is_favorite = favorite)
    }

    pub fn move_to_notebook(&self, id: &str, notebook_id: Option<String>) -> Option<Note> {
        self.update(id, |n| n.notebook_id = notebook_id)
    }

    /// Soft-delete into the recycle bin.
    pub fn delete(&self, id: &str) -> Option<Note> {
        self.update(id, |n| n.deleted_at = Some(AppleDate::now()))
    }

    /// Take a note out of the bin.
    pub fn recover(&self, id: &str) -> Option<Note> {
        self.update(id, |n| n.deleted_at = None)
    }

    /// Delete permanently, dropping the note's images and bookmarks.
    ///
    /// Returns the purged id. Images live in `note-images/<id>` and bookmarks
    /// under `note:<id>`, so the caller (which owns both stores) can finish the
    /// job; the note itself is gone from memory and from the next save.
    pub fn purge(&self, id: &str) -> Option<String> {
        let mut notes = self.notes.write().unwrap();
        let before = notes.len();
        notes.retain(|n| n.id != id);
        let removed = notes.len() != before;
        drop(notes);
        if !removed {
            return None;
        }
        self.purged.lock().unwrap().push(id.to_string());
        self.bump();
        self.persist(true);
        Some(id.to_string())
    }

    /// Permanently delete everything in the bin.
    pub fn empty_bin(&self) -> Vec<String> {
        let purged: Vec<String> = self
            .notes
            .read()
            .unwrap()
            .iter()
            .filter(|n| n.is_deleted())
            .map(|n| n.id.clone())
            .collect();
        if purged.is_empty() {
            return purged;
        }
        self.notes.write().unwrap().retain(|n| !n.is_deleted());
        self.purged.lock().unwrap().extend(purged.iter().cloned());
        self.bump();
        self.persist(true);
        purged
    }

    /// Drop binned notes past the retention window. Runs at open.
    fn expire_recycle_bin(&mut self) {
        let cutoff = AppleDate::now().plus_days(-RECYCLE_RETENTION_DAYS);
        let expired: Vec<String> = self
            .notes
            .read()
            .unwrap()
            .iter()
            .filter(|n| n.deleted_at.map(|d| d.raw() < cutoff.raw()).unwrap_or(false))
            .map(|n| n.id.clone())
            .collect();
        if expired.is_empty() {
            return;
        }
        self.notes.write().unwrap().retain(|n| {
            !n.deleted_at
                .map(|d| d.raw() < cutoff.raw())
                .unwrap_or(false)
        });
        self.purged
            .lock()
            .unwrap()
            .extend(expired.iter().cloned());
    }

    /// What the bookmark for `note_id` should be, if the player has one.
    ///
    /// Read by the player, written through [`Self::put_bookmark`], which is
    /// where the write-ahead ordering lives — the store never writes a bookmark
    /// it was not handed.
    pub fn bookmark_of(&self, _note_id: &str) -> Option<PlaybackBookmark> {
        None
    }

    pub fn put_bookmark(&self, _bookmark: PlaybackBookmark) {}

    fn bump(&self) {
        self.version.fetch_add(1, Ordering::Relaxed);
    }

    /// Hand the library to the writer thread.
    ///
    /// `immediate` jumps the queue: a pin, a delete or a purge is a state change
    /// the user expects to survive a crash, unlike a keystroke. Encode happens
    /// here on the calling thread because it is a few microseconds for a typical
    /// library; the *write* is what takes time, and that is on the writer thread.
    /// Hand the library to the writer thread.
    ///
    /// `immediate` writes now, ahead of anything queued; otherwise the save
    /// rides the debounce. Either way **the encode happens on the writer
    /// thread**, via [`SharedWriter::schedule_lazy`]: the caller hands over a
    /// closure that reads the library when the window closes, so a keystroke
    /// costs a lock and an enqueue and nothing else. Encoding eagerly would make
    /// every keystroke O(n) — ~100 ms on a 5 000-note library, measured.
    fn persist(&self, immediate: bool) {
        // A live batch holds every save; the last guard to drop writes once.
        if self.defer_depth.load(Ordering::Acquire) > 0 {
            return;
        }
        if immediate {
            self.persist_now();
            return;
        }

        let library = self.notes.clone();
        let errors = self.save_error.clone();
        self.writer.schedule_lazy(Box::new(move || {
            let snapshot = library.read().unwrap().clone();
            match serde_json::to_vec_pretty(&snapshot) {
                Ok(mut b) => {
                    b.push(b'\n');
                    b
                }
                Err(e) => {
                    *errors.lock().unwrap() = Some(e.to_string());
                    Vec::new()
                }
            }
        }));
    }

    /// Encode and hand the library to the writer now, ahead of anything
    /// queued. The immediate half of [`Self::persist`], also the write a
    /// [`Self::batch_saves`] guard performs when the last one drops.
    fn persist_now(&self) {
        // An immediate save is a real state change (pin, delete, purge), so
        // the few microseconds of encoding are worth the guarantee.
        let snapshot = self.notes.read().unwrap().clone();
        match serde_json::to_vec_pretty(&snapshot) {
            Ok(mut b) => {
                b.push(b'\n');
                self.writer.now(b);
            }
            Err(e) => *self.save_error.lock().unwrap() = Some(e.to_string()),
        }
    }

    /// Hold every save until the returned guard drops, then save once.
    ///
    /// The import path mutates the library once per note through
    /// [`Self::insert`] and [`Self::update`], and each of those saves encodes
    /// the *whole* library on the calling thread — an N-note import was N
    /// encodes of a growing library, O(n²) bytes. Holding a guard across the
    /// loop makes every mutation free and writes the library exactly once at
    /// the end, which is the same crash window a debounced keystroke already
    /// accepts. Saves arriving from other threads inside the window defer
    /// with the batch (a pin made while an import runs lands with the
    /// import's single save). Nesting is counted, not boolean.
    pub fn batch_saves(&self) -> BatchedSaves<'_> {
        self.defer_depth.fetch_add(1, Ordering::AcqRel);
        BatchedSaves { store: self }
    }

    /// Block until everything is on disk. Called on quit.
    pub fn flush(&self) {
        let _ = self.writer.flush();
    }

    /// Flush and stop the writer thread. Called on quit; `Drop` also flushes,
    /// but only an explicit shutdown joins the thread.
    pub fn shutdown(&mut self) {
        self.flush();
        // Hand the (now empty) thread slot away so `Drop` cannot try to shut
        // the writer down a second time.
        let mut thread = WriterThread::detached();
        std::mem::swap(&mut thread, &mut self.thread);
        let _ = thread.shutdown(&self.writer);
    }
}

/// The guard [`NotesStore::batch_saves`] returns. Dropping it decrements the
/// depth; the drop that brings the depth to zero writes the library once.
pub struct BatchedSaves<'a> {
    store: &'a NotesStore,
}

impl Drop for BatchedSaves<'_> {
    fn drop(&mut self) {
        // AcqRel pairs with the load in `persist`: the final decrement
        // happens-before the save, so no thread can still see "deferred"
        // while the write is being handed over.
        if self.store.defer_depth.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.store.persist_now();
        }
    }
}

impl Drop for NotesStore {
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Shared fixture helpers. `pub(crate)` because `sn-store`'s integration
    //! tests use them too.

    /// Serialises every test that mutates the process environment. See the
    /// `XdgFixture` docs for why this has to exist.
    pub(crate) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Point the XDG roots at a fresh temp directory for one test.
    pub struct XdgFixture {
        dir: std::path::PathBuf,
        // Held for the fixture's whole life. `cargo test` runs tests in
        // parallel threads of one process, and the XDG variables are
        // process-global — two fixtures running at once point each other's
        // stores at the wrong directory and fail in ways that look like store
        // bugs. Serialising the env-mutating tests is the honest fix; making
        // paths injectable is the Batch 3 refactor, when the UI needs profiles.
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl XdgFixture {
        pub fn new() -> Self {
            // `sn_core`'s guard first: the XDG variables are global to the
            // process, so this crate's fixtures and `sn_core`'s own tests have
            // to take turns rather than each guarding only itself.
            let _xdg = sn_core::paths::xdg_test_lock();
            let guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let dir = std::env::temp_dir().join(format!(
                "sn-store-{}-{}",
                std::process::id(),
                sn_core::util::new_uuid()
            ));
            std::fs::create_dir_all(dir.join("data")).unwrap();
            std::fs::create_dir_all(dir.join("cache")).unwrap();
            std::fs::create_dir_all(dir.join("config")).unwrap();
            std::env::set_var("XDG_DATA_HOME", dir.join("data"));
            std::env::set_var("XDG_CACHE_HOME", dir.join("cache"));
            std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
            Self { dir, _guard: guard }
        }

    }

    impl Drop for XdgFixture {
        fn drop(&mut self) {
            // Remove first, then release the lock: another test may recreate
            // and reuse these variables the moment we let go.
            let _ = std::fs::remove_dir_all(&self.dir);
            // The floor is the scratch library, not the unset variable:
            // unsetting falls back to `$HOME/.local/share` — the user's
            // live library — and every book test that runs after this drop
            // used to write its fixtures there (the orphan `Books/<uuid>/`
            // directories that inspired `scratch_data_home`). Cache and
            // config keep the old unset behaviour; only the data root ever
            // held a live library worth protecting.
            let scratch = scratch_data_home();
            std::env::set_var("XDG_DATA_HOME", scratch);
            std::env::remove_var("XDG_CACHE_HOME");
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }

    /// The process-wide scratch data root every path-mutating test falls
    /// back to: pid-keyed under the OS temp dir, so parallel test binaries
    /// never share one. Both [`XdgFixture`] (before and after each of its
    /// tests) and the book tests' `new_book_id` (through
    /// `isolate_test_library`) land here, so no test ever resolves a store
    /// path into the user's real `~/.local/share`.
    pub fn scratch_data_home() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("sn-store-test-library-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&root);
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::note_key;
    use sn_core::util;


    fn fixture() -> crate::notes::test_support::XdgFixture {
        crate::notes::test_support::XdgFixture::new()
    }

    fn open() -> NotesStore {
        paths::ensure_directories().unwrap();
        NotesStore::open().unwrap()
    }

    #[test]
    fn a_fresh_library_loads_empty_and_writes_a_file() {
        let _f = fixture();
        let store = open();
        assert!(store.is_empty());
        assert_eq!(store.load_report(), &LoadReport::Fresh);
        store.flush();
        assert!(paths::notes_file().exists());
    }

    #[test]
    fn a_note_is_created_filed_and_listed() {
        let _f = fixture();
        let store = open();
        let nb = util::new_uuid();
        let note = store.create(Some(nb.clone()));
        assert_eq!(store.len(), 1);
        assert_eq!(note.notebook_id.as_deref(), Some(nb.as_str()));
        assert_eq!(store.active().len(), 1);
        assert!(store.binned().is_empty());
    }

    #[test]
    fn an_edit_is_visible_immediately_and_persisted_after_flush() {
        let _f = fixture();
        let store = open();
        let note = store.create(None);
        store.update_text(&note.id, "new body").unwrap();
        assert_eq!(store.get(&note.id).unwrap().text, "new body");

        // Reopen from disk through a second store to prove it landed.
        store.flush();
        let again = open();
        assert_eq!(again.get(&note.id).unwrap().text, "new body");
    }

    #[test]
    fn a_noop_edit_does_not_bump_the_version() {
        let _f = fixture();
        let store = open();
        let note = store.create(None);
        store.update_text(&note.id, "same").unwrap();
        let before = store.version();
        // Writing the identical text is not an edit: no bump, no save, so the
        // debounced writer has nothing new to coalesce.
        store.update_text(&note.id, "same").unwrap();
        assert_eq!(store.version(), before);
        store.update_text(&note.id, "different").unwrap();
        assert!(store.version() > before);
    }

    #[test]
    fn pin_star_and_move_save_immediately() {
        let _f = fixture();
        let store = open();
        let note = store.create(None);
        store.set_pinned(&note.id, true).unwrap();
        store.set_favorite(&note.id, true).unwrap();

        let nb = util::new_uuid();
        store.move_to_notebook(&note.id, Some(nb.clone())).unwrap();
        let got = store.get(&note.id).unwrap();
        assert!(got.is_pinned && got.is_favorite);
        assert_eq!(got.notebook_id.as_deref(), Some(nb.as_str()));
    }

    #[test]
    fn deleting_bins_and_recovering_restores() {
        let _f = fixture();
        let store = open();
        let note = store.create(None);
        store.delete(&note.id).unwrap();
        assert_eq!(store.len(), 0);
        assert_eq!(store.binned().len(), 1);
        assert!(store.binned()[0].deleted_at.is_some());

        store.recover(&note.id).unwrap();
        assert_eq!(store.len(), 1);
        assert!(store.binned().is_empty());
    }

    #[test]
    fn a_purge_removes_the_note_and_reports_it() {
        let _f = fixture();
        let store = open();
        let a = store.create(None);
        let b = store.create(None);
        store.delete(&a.id).unwrap();
        let purged = store.purge(&a.id).unwrap();
        assert_eq!(purged, a.id);
        assert_eq!(store.take_purged(), vec![a.id.clone()]);
        assert!(store.get(&a.id).is_none());
        assert!(store.get(&b.id).is_some());
    }

    #[test]
    fn emptying_the_bin_purges_every_binned_note_and_nothing_else() {
        let _f = fixture();
        let store = open();
        let a = store.create(None);
        let b = store.create(None);
        let c = store.create(None);
        store.delete(&a.id).unwrap();
        store.delete(&b.id).unwrap();
        let purged = store.empty_bin();
        assert_eq!(purged.len(), 2);
        assert!(store.get(&c.id).is_some());
        assert_eq!(store.take_purged().len(), 2);
    }

    #[test]
    fn a_binned_note_past_retention_expires_at_open() {
        let _f = fixture();
        let old = Note {
            deleted_at: Some(AppleDate::now().plus_days(-(RECYCLE_RETENTION_DAYS + 5))),
            ..Note::new(None)
        };
        let fresh = Note {
            deleted_at: Some(AppleDate::now()),
            ..Note::new(None)
        };
        {
            let store = NotesStore::from_notes(vec![old.clone(), fresh.clone()]).unwrap();
            // The expired one is gone; the fresh one is still in the bin.
            let binned = store.binned();
            assert_eq!(binned.len(), 1, "the fresh binned note was purged early");
            assert_eq!(binned[0].id, fresh.id);
            assert_eq!(store.take_purged(), vec![old.id.clone()]);
            store.flush();
        }
        let again = open();
        assert!(again.get(&fresh.id).is_some(), "a fresh binned note was purged early");
        assert!(again.get(&old.id).is_none());
    }

    #[test]
    fn a_corrupt_file_is_quarantined_and_recovered() {
        let _f = fixture();
        paths::ensure_directories().unwrap();
        let path = paths::notes_file();
        sn_core::atomic::write_json_atomic(&sn_core::atomic::backup_path(&path), &vec![Note::new(None)])
            .unwrap();
        std::fs::write(&path, b"{ broken").unwrap();

        let store = open();
        assert_eq!(store.len(), 1);
        assert!(matches!(store.load_report(), LoadReport::RecoveredFromBackup));
        let quarantined = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .count();
        assert_eq!(quarantined, 1, "the broken file must be preserved");
    }

    #[test]
    fn the_written_file_is_an_array_with_every_key_present() {
        let _f = fixture();
        let store = open();
        let note = store.create(Some(util::new_uuid()));
        store.set_pinned(&note.id, true).unwrap();
        store.flush();

        let text = std::fs::read_to_string(paths::notes_file()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let arr = parsed.as_array().expect("notes.json is a top-level array");
        assert_eq!(arr.len(), 1);
        let obj = arr[0].as_object().unwrap();
        for key in [
            "id",
            "explicitTitle",
            "text",
            "createdAt",
            "updatedAt",
            "deletedAt",
            "notebookId",
            "isPinned",
            "isFavorite",
        ] {
            assert!(obj.contains_key(key), "missing {key}");
        }
    }

    #[test]
    fn the_written_file_matches_the_other_apps_field_for_field() {
        let _f = fixture();
        let store = open();
        let note = store.create(None);
        store.flush();

        let raw = std::fs::read_to_string(paths::notes_file()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let obj = &parsed.as_array().unwrap()[0];
        assert_eq!(obj["id"].as_str().unwrap(), note.id);
        assert!(obj["createdAt"].is_number(), "dates are JSON numbers");
        assert!(obj["deletedAt"].is_null());
        assert!(obj["isPinned"].is_boolean());
    }

    #[test]
    fn a_5k_note_library_keystroke_costs_microseconds() {
        let _f = fixture();
        let notes: Vec<Note> = (0..5_000)
            .map(|i| {
                let mut n = Note::new(None);
                n.text = format!("Note {i}. Some body text to make the payload realistic.");
                n
            })
            .collect();

        let store = NotesStore::from_notes(notes).unwrap();
        let id = store.active()[0].id.clone();

        // What a keystroke pays: one write-lock acquisition and one enqueue.
        // The encode — the only O(n) part — runs on the writer thread when the
        // debounce fires, which is why this bound is microseconds and not the
        // ~100 ms an eager encode costs here.
        for _ in 0..20 {
            let started = std::time::Instant::now();
            store.update_text(&id, "Edited body.").unwrap();
            let elapsed = started.elapsed();
            assert!(
                elapsed.as_micros() < 5_000,
                "a keystroke took {elapsed:?} on a 5k-note library"
            );
        }

        // The write happens off-thread; flush proves it lands with the final
        // text, not one of the intermediate ones.
        store.flush();
        let again = open();
        assert_eq!(again.len(), 5_000);
        assert_eq!(again.get(&id).unwrap().text, "Edited body.");
    }

    #[test]
    fn a_kill_during_a_write_leaves_a_loadable_library() {
        // Simulates dying mid-write: write a good file, then clobber the temp.
        let _f = fixture();
        {
            let store = open();
            let note = store.create(None);
            store.update_text(&note.id, "survives").unwrap();
            store.flush();
            assert_eq!(store.get(&note.id).unwrap().text, "survives");
        }
        // A stale temp file from a killed process must be ignored on load.
        let path = paths::notes_file();
        let name = path.file_name().unwrap().to_str().unwrap();
        let stale = path.with_file_name(format!(".{name}.tmp-99999-1"));
        std::fs::write(&stale, b"{ half written").unwrap();
        let again = open();
        assert_eq!(again.len(), 1);
        assert_eq!(again.active()[0].text, "survives");
    }

    #[test]
    fn the_bookmark_hooks_are_present_and_harmless() {
        // Placeholder until the player lands: the store compiles against the
        // bookmark type without owning a bookmark file.
        let _f = fixture();
        let store = open();
        let note = store.create(None);
        assert!(store.bookmark_of(&note.id).is_none());
        store.put_bookmark(PlaybackBookmark::for_note(&note.id, 0, 0));
        store.put_bookmark(PlaybackBookmark::for_note(&note.id, 0, util::stable_hash("x")));
        let _ = note_key(&note.id);
    }

    #[test]
    fn an_edit_does_not_clear_a_bookmark_the_store_does_not_own() {
        // The Swift bug: every update dropped the note's bookmark. Here the
        // store has no bookmark to drop, and the version bump is the only thing
        // that changes — the player decides whether its bookmark survives.
        let _f = fixture();
        let store = open();
        let note = store.create(None);
        let before = store.version();
        store.update_text(&note.id, "edited").unwrap();
        assert!(store.version() > before);
        assert!(store.bookmark_of(&note.id).is_none());
    }

    #[test]
    fn flush_after_a_purge_writes_a_file_without_the_note() {
        let _f = fixture();
        let store = open();
        let a = store.create(None);
        let b = store.create(None);
        store.purge(&a.id).unwrap();
        store.flush();
        let again = open();
        assert!(again.get(&a.id).is_none());
        assert!(again.get(&b.id).is_some());
    }
}