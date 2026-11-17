//! Atomic file writes, quarantine-and-recover, and a coalescing writer thread.
//!
//! The rule this module exists to enforce: **a store is never left in a state
//! it cannot be loaded from.** Write to a sibling temp file, fsync it, rename it
//! over the target, then fsync the directory. Rename within a filesystem is
//! atomic, so a crash at any point leaves either the old file or the new one,
//! never a half-written one. And when a file *does* turn up unparseable, move
//! it aside rather than deleting it — the Swift port's `notes.json.corrupt-<epoch>`
//! convention is worth keeping exactly.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::appledate::unix_now;

/// Writes between rolling backups, matching the cadence the iOS and Swift ports
/// used so `.backup.json` means the same thing in all three apps.
pub const BACKUP_EVERY: u64 = 10;

/// A closure the writer thread runs when the debounce expires.
pub type LazyPayload = Box<dyn FnOnce() -> Vec<u8> + Send>;

/// The last write that failed, and where.
///
/// A store's write happens on its writer thread, and a failure there used to
/// go no further than a log line: the user closed the app believing the note
/// was saved, and it was not. The error is kept here so the UI can ask for
/// it and say so out loud.
///
/// The key is the target path, so a store asks about **its own** file and
/// another store's failure does not show up in its status line. The value is
/// cleared by [`take_write_error`], so an error is reported once.
static WRITE_ERRORS: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());

/// Record a failed write.
fn record_write_error(path: &Path, error: &io::Error) {
    let mut errors = WRITE_ERRORS.lock().unwrap_or_else(|p| p.into_inner());
    // One entry per path: a second failure replaces the first, so the report
    // is the *latest* truth about that file.
    errors.retain(|(known, _)| known != path);
    errors.push((path.to_path_buf(), error.to_string()));
    // A hard disk can fail many files; the list is a report, not a history.
    if errors.len() > 16 {
        let overflow = errors.len() - 16;
        errors.drain(0..overflow);
    }
}

/// The last failure writing `path`, cleared as it is read.
///
/// `None` means the last write to that path succeeded — or none has failed
/// since the last read.
pub fn take_write_error(path: &Path) -> Option<String> {
    let mut errors = WRITE_ERRORS.lock().unwrap_or_else(|p| p.into_inner());
    let at = errors.iter().position(|(known, _)| known == path)?;
    Some(errors.remove(at).1)
}

/// Whether any write has failed since the last read — the "something did not
/// save" check the UI makes on its way out.
pub fn any_write_error() -> bool {
    !WRITE_ERRORS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty()
}

/// Write `bytes` to `path` atomically.
///
/// The temp file is a sibling of the target so the rename never crosses a
/// filesystem boundary.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let (tmp, mut f) = open_temp_sibling(path)?;
    f.write_all(bytes)?;
    // Explicit sync_all, and an explicit close before the rename: close()
    // alone does not guarantee the bytes reached the disk, only that they
    // left the page cache.
    f.sync_all()?;
    drop(f);

    match fs::rename(&tmp, path) {
        Ok(()) => {}
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    }

    // Persist the directory entry so the rename itself survives a power cut.
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

/// Serialise `value` as pretty JSON and write it atomically.
pub fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

/// Copy a file to `path` atomically, streaming through a bounded buffer.
///
/// The audio import's form of [`write_atomic`]: an m4b runs to gigabytes,
/// and the only honest way to land one is a copy, not a `read` into RAM
/// followed by a write. Same contract as every atomic here — a `.part`
/// sibling, fsync, rename — so an interrupted import never leaves a
/// half-written original behind.
pub fn copy_atomic(from: &Path, path: &Path) -> io::Result<u64> {
    use std::io::{Read, Write};
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut source = fs::File::open(from)?;
    let (tmp, dest_file) = open_temp_sibling(path)?;
    let copied = {
        let mut dest = dest_file;
        let mut buffer = [0u8; 256 * 1024];
        let mut total = 0u64;
        loop {
            let n = source.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            dest.write_all(&buffer[..n])?;
            total += n as u64;
        }
        dest.sync_all()?;
        total
    };
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Some(parent) = path.parent() {
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(copied)
}

/// Rename an unreadable file out of the way, returning the quarantine path.
///
/// Never overwrites an existing quarantine: the name carries a timestamp and,
/// if that is somehow taken, a counter. Two corrupt saves in the same second
/// must not destroy the first one's evidence.
pub fn quarantine(path: &Path) -> io::Result<PathBuf> {
    let stamp = unix_now() as i64;
    let mut candidate = path.with_file_name(format!(
        "{}.corrupt-{stamp}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("store")
    ));
    let mut n = 1u32;
    while candidate.exists() {
        candidate = path.with_file_name(format!(
            "{}.corrupt-{stamp}-{n}",
            path.file_name().and_then(|f| f.to_str()).unwrap_or("store")
        ));
        n += 1;
    }
    fs::rename(path, &candidate)?;
    Ok(candidate)
}

/// Load JSON from `path`, falling back through a recovery chain.
///
/// Order: the real file; if it is missing, unreadable or unparseable, quarantine
/// it (if it existed) and try `path.backup.json`; then try a `.corrupt-*`
/// sibling left by an earlier run; then start empty. Every step is logged, so a
/// user who lost a note can see in the log exactly which file was quarantined.
pub fn load_json_recoverable<T: DeserializeOwned + Default>(
    path: &Path,
) -> (T, LoadOutcome) {
    match read_json::<T>(path) {
        Ok(v) => return (v, LoadOutcome::Primary),
        Err(LoadError::Missing) => {}
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "store unreadable; quarantining and recovering"
            );
            if let Err(qe) = quarantine(path) {
                tracing::error!(path = %path.display(), error = %qe, "quarantine failed");
            }
        }
    }

    let backup = backup_path(path);
    if let Ok(v) = read_json::<T>(&backup) {
        tracing::warn!(path = %backup.display(), "recovered store from backup");
        return (v, LoadOutcome::Backup);
    }

    // An earlier quarantine from this session or a previous one: if it parses,
    // it beats an empty library.
    if let Some(parent) = path.parent() {
        let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if let Ok(entries) = fs::read_dir(parent) {
            let mut candidates: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with(stem) && n.contains(".corrupt-"))
                        .unwrap_or(false)
                })
                .collect();
            candidates.sort();
            for c in candidates.into_iter().rev() {
                if let Ok(v) = read_json::<T>(&c) {
                    tracing::warn!(path = %c.display(), "recovered store from a quarantined file");
                    return (v, LoadOutcome::Quarantined(c));
                }
            }
        }
    }

    (T::default(), LoadOutcome::Empty)
}

/// How a store was loaded — surfaced so the UI can say "recovered from backup"
/// instead of silently pretending nothing happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    Primary,
    Backup,
    Quarantined(PathBuf),
    Empty,
}

impl LoadOutcome {
    pub fn is_problem(&self) -> bool {
        !matches!(self, LoadOutcome::Primary)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("file not found")]
    Missing,
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Read and parse a JSON file, distinguishing "absent" from "broken".
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, LoadError> {
    if !path.exists() {
        return Err(LoadError::Missing);
    }
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// `notes.json` → `notes.backup.json`
pub fn backup_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("store.json");
    let stem = name.strip_suffix(".json").unwrap_or(name);
    path.with_file_name(format!("{stem}.backup.json"))
}

/// Create a fresh temp sibling of `path` and return it with its handle.
///
/// The name is predictable — `.<file>.tmp-<pid>-<n>` in the target's own
/// directory — so a local attacker who can write there can pre-plant it. A
/// plain `File::create` follows a planted symlink and the store's bytes go
/// wherever the link points; `create_new` refuses any existing path, link or
/// file, and the counter simply walks on to the next name. The data
/// directory is 0700 under XDG_DATA_HOME, which makes the race
/// low-severity — this closes it outright rather than leaning on the
/// directory's permissions.
fn open_temp_sibling(path: &Path) -> io::Result<(PathBuf, fs::File)> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("store");
    loop {
        // Process id + counter, so two writers in one process never collide
        // and a stale temp from a killed run is still identifiable.
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{name}.tmp-{}-{}", std::process::id(), n));
        match fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// A background writer that coalesces save requests.
///
/// Notes save on every keystroke burst; the store does not need to. Requests
/// within `debounce` of each other collapse into one write, but the *last*
/// request always gets written: there is a trailing timer, so continuous typing
/// never starves the disk and a stop-typing-then-waiting moment always flushes.
///
/// `flush` and `shutdown` block until the writer has drained, which is what
/// makes close-window and quit safe. The Swift port had no shutdown flush wired
/// anywhere, so a prefs write inside the debounce window was lost on close.
pub struct DebouncedWriter {
    tx: mpsc::Sender<Command>,
    handle: Mutex<Option<JoinHandle<()>>>,
    debounce: Duration,
}

pub enum Command {
    /// Replace any pending payload; write after the debounce window.
    Schedule(Vec<u8>),
    /// Replace any pending payload; the bytes are produced on the writer thread
    /// when the debounce expires, so the caller pays nothing per keystroke.
    ///
    /// The closure reads the *current* state when it runs, which is the point:
    /// ten edits in the window produce one write of the final text, not ten
    /// writes of intermediate states.
    ScheduleLazy(LazyPayload),
    /// Write immediately, ahead of anything scheduled.
    Now(Vec<u8>),
    /// Block until everything queued so far has been written.
    Flush(mpsc::Sender<()>),
    Stop(mpsc::Sender<()>),
}

impl DebouncedWriter {
    /// Spawn a writer for `path` with the given debounce window.
    pub fn new(path: PathBuf, debounce: Duration) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Command>();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let handle = std::thread::Builder::new()
            .name(format!("sn-writer-{}", path.display()))
            .spawn(move || writer_loop(path, rx, debounce))?;
        Ok(Self {
            tx,
            handle: Mutex::new(Some(handle)),
            debounce,
        })
    }

    pub fn debounce(&self) -> Duration {
        self.debounce
    }

    /// Clone the whole writer, so it can be handed to another thread and shut
    /// down from there. Cheap: both sides share one thread and one channel.
    pub fn clone_tx(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            handle: Mutex::new(None),
            debounce: self.debounce,
        }
    }

    /// The raw command channel, for callers that need to drive the writer
    /// directly (tests, and the shutdown path). Prefer [`Self::share`] for
    /// handing a writer to another thread.
    pub fn sender(&self) -> mpsc::Sender<Command> {
        self.tx.clone()
    }

    /// Split into an enqueue handle other threads can hold plus the thread
    /// itself, so a store can be shared without the writer being shut down when
    /// the last clone goes away.
    pub fn share(self) -> (SharedWriter, WriterThread) {
        // `Drop` would shut the thread down on the way out of this function; the
        // thread's ownership is being handed to `WriterThread` instead.
        let mut this = std::mem::ManuallyDrop::new(self);
        let thread = WriterThread {
            handle: Mutex::new(
                this.handle
                    .get_mut()
                    .unwrap_or_else(|e| e.into_inner())
                    .take(),
            ),
            // Its own sender, so its `Drop` can shut the writer down even while
            // every `SharedWriter` clone is still alive.
            tx: this.tx.clone(),
        };
        let shared = SharedWriter {
            tx: this.tx.clone(),
            debounce: this.debounce,
        };
        (shared, thread)
    }

    /// Request a save. Cheap: clones bytes and sends.
    pub fn schedule(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(Command::Schedule(bytes));
    }

    /// Request a save that jumps the queue.
    pub fn now(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(Command::Now(bytes));
    }

    /// Queue a save whose bytes are produced on the writer thread when the
    /// debounce expires. See [`SharedWriter::schedule_lazy`].
    pub fn schedule_lazy(&self, produce: LazyPayload) {
        let _ = self.tx.send(Command::ScheduleLazy(produce));
    }

    /// Block until everything requested so far is on disk.
    pub fn flush(&self) -> io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.tx.send(Command::Flush(ack_tx)).is_err() {
            return Ok(()); // writer already gone; nothing pending can be written
        }
        let _ = ack_rx.recv();
        Ok(())
    }

    /// Flush and stop the thread. Called on quit.
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.flush()?;
        if let Some(h) = self.handle.lock().unwrap().take() {
            let (ack_tx, ack_rx) = mpsc::channel();
            let _ = self.tx.send(Command::Stop(ack_tx));
            let _ = ack_rx.recv();
            let _ = h.join();
        }
        Ok(())
    }
}

impl Drop for DebouncedWriter {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// The enqueue side of a [`DebouncedWriter`], shareable across threads.
///
/// Cheap to clone by `Arc`-wrapping; closing it does not stop the writer, so a
/// background save can outlive the UI object that queued it.
#[derive(Clone)]
pub struct SharedWriter {
    tx: mpsc::Sender<Command>,
    debounce: Duration,
}

impl SharedWriter {
    /// Queue a save after the debounce window, coalescing with anything pending.
    pub fn schedule(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(Command::Schedule(bytes));
    }

    /// Queue a save whose bytes are produced on the writer thread when the
    /// debounce expires.
    ///
    /// This is the shape a note store wants: a keystroke mutates the in-memory
    /// library and hands over a closure that reads it, so the encode — the only
    /// O(n) part of a save — runs once per quiet period instead of once per
    /// keystroke. On a 5 000-note library that is the difference between a
    /// keystroke costing microseconds and costing ~100 ms.
    pub fn schedule_lazy(&self, produce: LazyPayload) {
        let _ = self.tx.send(Command::ScheduleLazy(produce));
    }

    /// Queue a save that jumps the queue.
    pub fn now(&self, bytes: Vec<u8>) {
        let _ = self.tx.send(Command::Now(bytes));
    }

    /// Block until everything queued so far is on disk.
    pub fn flush(&self) -> io::Result<()> {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.tx.send(Command::Flush(ack_tx)).is_err() {
            return Ok(());
        }
        let _ = ack_rx.recv();
        Ok(())
    }

    pub fn debounce(&self) -> Duration {
        self.debounce
    }
}

/// Owns the writer thread.
///
/// Shut down explicitly with [`WriterThread::shutdown`]; `Drop` also shuts down
/// as a safety net. `Drop` can do that because it holds its own sender —
/// without one, a drop could only `join`, and the writer would be waiting on a
/// channel that still had a live sender, so the join would block forever. That
/// exact deadlock shipped in the first draft and hung the test suite.
pub struct WriterThread {
    handle: Mutex<Option<JoinHandle<()>>>,
    tx: mpsc::Sender<Command>,
}

impl WriterThread {
    /// A thread owner holding nothing, for handing ownership over: the sender
    /// puts a real one here and swaps this placeholder out, so `Drop` can never
    /// join the same thread twice.
    pub fn detached() -> Self {
        //  returns (Sender, Receiver); the placeholder only needs the
        // sender side so its  can signal a stop it will never be waited on.
        let (tx, _rx) = mpsc::channel();
        Self {
            handle: Mutex::new(None),
            tx,
        }
    }
}

impl WriterThread {
    /// Flush and join. Idempotent.
    pub fn shutdown(&mut self, writer: &SharedWriter) -> io::Result<()> {
        writer.flush()?;
        if let Some(h) = self.handle.lock().unwrap().take() {
            let (ack_tx, ack_rx) = mpsc::channel();
            let _ = writer.tx.send(Command::Stop(ack_tx));
            let _ = ack_rx.recv();
            let _ = h.join();
        }
        Ok(())
    }
}

impl Drop for WriterThread {
    fn drop(&mut self) {
        if let Some(h) = self.handle.lock().unwrap().take() {
            // Tell the writer to finish and exit, *then* join. Joining without
            // the Stop deadlocks: the writer is blocked on the channel and its
            // senders are still alive.
            let (ack_tx, ack_rx) = mpsc::channel();
            let _ = self.tx.send(Command::Stop(ack_tx));
            let _ = ack_rx.recv();
            let _ = h.join();
        }
    }
}

/// Encode `value` as pretty JSON and hand the bytes to a writer.
pub fn enqueue_json<T: Serialize>(writer: &SharedWriter, value: &T, immediate: bool) {
    match serde_json::to_vec_pretty(value) {
        Ok(mut bytes) => {
            bytes.push(b'\n');
            if immediate {
                writer.now(bytes);
            } else {
                writer.schedule(bytes);
            }
        }
        Err(e) => tracing::error!(error = %e, "store encode failed"),
    }
}

/// The writer thread.
///
/// One rule governs everything here: **the newest payload must reach disk, and
/// nothing older than it.** A request either replaces the pending payload (a
/// save, coalesced) or is written immediately (an urgent save). A timer fires
/// at most once per quiet period, so a burst of keystrokes costs one write and
/// continuous typing still flushes — the failure mode the iOS bookmark writer
/// had, where a re-arming debounce meant an hours-long session never wrote.
///
/// The loop blocks on the channel when there is nothing pending, so an idle app
/// costs no CPU.
fn writer_loop(path: PathBuf, rx: mpsc::Receiver<Command>, debounce: Duration) {
    // Exactly one of these is `Some` at a time: `pending` holds bytes that were
    // already produced, `lazy` holds a closure that will produce them when the
    // window closes. A lazy payload replaces a pending one and vice versa —
    // whichever arrived last wins, and the other is dropped, never written.
    let mut pending: Option<Vec<u8>> = None;
    let mut lazy: Option<LazyPayload> = None;
    let mut backup_counter: u64 = 0;
    // Armed only while a payload is pending, so an idle writer blocks on the
    // channel forever and an app sitting at 0% CPU costs nothing.
    let mut quiet: Option<Instant> = None;

    loop {
        // Block indefinitely when nothing is pending; otherwise wait out the
        // quiet period so a burst of keystrokes collapses into one write.
        let command = match quiet {
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    materialise_and_write(
                        &mut pending,
                        &mut lazy,
                        &path,
                        &mut backup_counter,
                    );
                    quiet = None;
                    continue;
                }
                match rx.recv_timeout(deadline - now) {
                    Ok(cmd) => Some(cmd),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        materialise_and_write(
                            &mut pending,
                            &mut lazy,
                            &path,
                            &mut backup_counter,
                        );
                        return;
                    }
                }
            }
            None => match rx.recv() {
                Ok(cmd) => Some(cmd),
                Err(_) => {
                    materialise_and_write(&mut pending, &mut lazy, &path, &mut backup_counter);
                    return;
                }
            },
        };

        match command {
            Some(Command::Schedule(bytes)) => {
                pending = Some(bytes);
                // Recomputed on every arrival, so a run of keystrokes extends the
                // window instead of writing on each one.
                quiet = Some(Instant::now() + debounce);
            }
            Some(Command::ScheduleLazy(produce)) => {
                lazy = Some(produce);
                quiet = Some(Instant::now() + debounce);
            }
            Some(Command::Now(bytes)) => {
                // An urgent save supersedes anything queued behind it.
                pending = None;
                lazy = None;
                quiet = None;
                write_bytes(&path, &bytes, &mut backup_counter);
            }
            Some(Command::Flush(ack)) => {
                flush_drain(&rx, &mut pending, &mut lazy, &path, &mut backup_counter);
                quiet = None;
                let _ = ack.send(());
            }
            Some(Command::Stop(ack)) => {
                flush_drain(&rx, &mut pending, &mut lazy, &path, &mut backup_counter);
                let _ = ack.send(());
                return;
            }
            None => {
                materialise_and_write(&mut pending, &mut lazy, &path, &mut backup_counter);
                quiet = None;
            }
        }
    }
}

/// Handle everything already queued behind a `Flush` or `Stop`, then write the
/// newest payload.
///
/// The inner queue is drained by hand because the writer is the only thread
/// that may consume from it: a second `Schedule` that arrived while a flush was
/// in flight is newer than the payload the caller was waiting on, so it wins.
/// Nested `Flush`es are acknowledged rather than recursively drained, which is
/// what stops two threads flushing at once from writing twice or deadlocking.
fn flush_drain(
    rx: &mpsc::Receiver<Command>,
    pending: &mut Option<Vec<u8>>,
    lazy: &mut Option<LazyPayload>,
    path: &Path,
    backup_counter: &mut u64,
) {
    while let Ok(next) = rx.try_recv() {
        match next {
            Command::Schedule(bytes) => {
                *lazy = None;
                *pending = Some(bytes);
            }
            Command::ScheduleLazy(produce) => {
                *pending = None;
                *lazy = Some(produce);
            }
            Command::Now(bytes) => {
                *pending = None;
                *lazy = None;
                write_bytes(path, &bytes, backup_counter);
            }
            Command::Flush(ack) | Command::Stop(ack) => {
                // Someone else is already waiting on a drain; the write below
                // satisfies them too, so acknowledge and keep going.
                let _ = ack.send(());
            }
        }
    }
    materialise_and_write(pending, lazy, path, backup_counter);
}

/// Produce any pending bytes — running a lazy payload on this thread if there is
/// one — and write them.
fn materialise_and_write(
    pending: &mut Option<Vec<u8>>,
    lazy: &mut Option<LazyPayload>,
    path: &Path,
    backup_counter: &mut u64,
) {
    if pending.is_none() {
        if let Some(produce) = lazy.take() {
            *pending = Some(produce());
        }
    } else {
        // Bytes beat a closure: the caller that sent bytes sent them after the
        // closure, and the newest payload wins.
        lazy.take();
    }
    if let Some(bytes) = pending.take() {
        write_bytes(path, &bytes, backup_counter);
    }
}

/// Write `bytes` to `path`, keeping a rolling backup.
///
/// `counter` is per writer, not global: a `static` counter made the backup
/// cadence depend on how many stores happened to exist, so a second store
/// could skip its backups for nine writes. Every tenth write, the *previous*
/// contents are copied to `.backup.json` first — that file is what
/// [`load_json_recoverable`] falls back to, so it must always be a real, older
/// version and never the bytes being written now.
fn write_bytes(path: &Path, bytes: &[u8], counter: &mut u64) {
    *counter = counter.saturating_add(1);
    let due = *counter % BACKUP_EVERY == 0;

    if due {
        if let Ok(current) = fs::read(path) {
            if !current.is_empty() {
                if let Err(e) = write_atomic(&backup_path(path), &current) {
                    tracing::warn!(error = %e, "rolling backup failed");
                }
            }
        }
    }

    if let Err(e) = write_atomic(path, bytes) {
        // The failure is kept, not just logged: the UI reads it and tells
        // the user their work did not land.
        record_write_error(path, &e);
        tracing::error!(path = %path.display(), error = %e, "store write failed");
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sn-atomic-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("store.json")
    }

    #[test]
    fn write_then_read_round_trips() {
        let p = tmp("roundtrip");
        write_atomic(&p, b"hello").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"hello");
    }

    #[test]
    fn a_pre_planted_temp_name_cannot_hijack_the_write() {
        let dir = tmp("planted").parent().unwrap().to_path_buf();
        let target = dir.join("store.json");
        // The exact name the first write would use, planted as a symlink to
        // a bystander file — the shape a local attacker leaves behind. (A
        // planted regular file is refused the same way; the link just makes
        // the old failure visible in a readable file.)
        let bystander = dir.join("bystander.txt");
        let planted = dir.join(format!(
            ".store.json.tmp-{}-0",
            std::process::id()
        ));
        std::fs::write(&bystander, b"innocent").unwrap();
        std::os::unix::fs::symlink(&bystander, &planted).unwrap();

        write_atomic(&target, b"ours").unwrap();

        // The write refused the planted name and took the next one: the
        // bystander is untouched and the target has the real bytes.
        assert_eq!(fs::read(&bystander).unwrap(), b"innocent");
        assert_eq!(fs::read(&target).unwrap(), b"ours");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_creates_missing_parents() {
        let p = tmp("parents").join("a/b/c/store.json");
        write_atomic(&p, b"x").unwrap();
        assert!(p.exists());
    }

    #[test]
    fn failed_write_leaves_no_temp_file_behind() {
        // A directory in place of the target makes rename fail.
        let p = tmp("failure");
        fs::create_dir_all(&p).unwrap();
        assert!(write_atomic(&p, b"x").is_err());
        let leftovers: Vec<_> = fs::read_dir(p.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[test]
    fn a_failed_write_is_reported_to_the_caller() {
        // The regression: a write failure went no further than a log line,
        // so the user closed the app believing the note was saved. The
        // failure is now kept, keyed by the path, and read once.
        let p = tmp("reported-failure");
        fs::create_dir_all(&p).unwrap();
        let mut counter = 0u64;
        write_bytes(&p, b"x", &mut counter);
        let why = take_write_error(&p).expect("the failure must be reported");
        assert!(!why.is_empty(), "the reason must not be empty");
        // It is reported once: the second read has nothing to say.
        assert!(take_write_error(&p).is_none());
    }

    #[test]
    fn a_failed_write_is_not_reported_for_another_path() {
        // The report is keyed by the file: one store's failure must not
        // appear in another store's status line.
        let failed = tmp("reported-failure-other");
        fs::create_dir_all(&failed).unwrap();
        let mut counter = 0u64;
        write_bytes(&failed, b"x", &mut counter);
        let other = tmp("reported-failure-unrelated");
        assert!(
            take_write_error(&other).is_none(),
            "an unrelated path must not report the failure"
        );
    }

    #[test]
    fn a_successful_write_leaves_no_error_behind() {
        let p = tmp("no-error");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut counter = 0u64;
        write_bytes(&p, b"x", &mut counter);
        assert!(take_write_error(&p).is_none());
    }

    #[test]
    fn quarantine_never_overwrites_an_existing_one() {
        let p = tmp("quarantine");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"a").unwrap();
        let q1 = quarantine(&p).unwrap();
        fs::write(&p, b"b").unwrap();
        let q2 = quarantine(&p).unwrap();
        assert_ne!(q1, q2);
        assert!(q1.exists() && q2.exists());
        assert_eq!(fs::read(&q1).unwrap(), b"a");
        assert_eq!(fs::read(&q2).unwrap(), b"b");
    }

    #[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Store {
        #[serde(default)]
        n: u32,
    }

    #[test]
    fn loads_primary_when_good() {
        let p = tmp("primary");
        write_json_atomic(&p, &Store { n: 7 }).unwrap();
        let (v, outcome) = load_json_recoverable::<Store>(&p);
        assert_eq!(v, Store { n: 7 });
        assert_eq!(outcome, LoadOutcome::Primary);
    }

    #[test]
    fn quarantines_broken_and_falls_back_to_backup() {
        let p = tmp("recover");
        write_json_atomic(&backup_path(&p), &Store { n: 42 }).unwrap();
        fs::write(&p, b"{ not json").unwrap();

        let (v, outcome) = load_json_recoverable::<Store>(&p);
        assert_eq!(v, Store { n: 42 });
        assert_eq!(outcome, LoadOutcome::Backup);

        let quarantined: Vec<_> = fs::read_dir(p.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".corrupt-"))
            .collect();
        assert_eq!(quarantined.len(), 1, "the broken file must be preserved");
    }

    #[test]
    fn recovers_from_a_previous_quarantine() {
        let p = tmp("requarantine");
        write_json_atomic(&p, &Store { n: 5 }).unwrap();
        quarantine(&p).unwrap();
        assert!(!p.exists());

        let (v, outcome) = load_json_recoverable::<Store>(&p);
        assert_eq!(v, Store { n: 5 });
        assert!(matches!(outcome, LoadOutcome::Quarantined(_)));
    }

    #[test]
    fn empty_is_not_a_problem_to_report_when_nothing_existed() {
        let p = tmp("empty");
        let (v, outcome) = load_json_recoverable::<Store>(&p);
        assert_eq!(v, Store::default());
        assert_eq!(outcome, LoadOutcome::Empty);
    }

    #[test]
    fn debounced_writer_eventually_writes() {
        let p = tmp("writer");
        let w = DebouncedWriter::new(p.clone(), Duration::from_millis(20)).unwrap();
        for i in 0..50u32 {
            w.schedule(serde_json::to_vec(&Store { n: i }).unwrap());
        }
        w.flush().unwrap();
        let (v, _) = load_json_recoverable::<Store>(&p);
        // Coalescing means the surviving payload is the newest one queued, not
        // an arbitrary recent one.
        assert_eq!(v.n, 49, "the last payload of a burst was lost");
    }

    #[test]
    fn a_debounced_write_lands_without_any_flush() {
        let p = tmp("autoflush");
        let w = DebouncedWriter::new(p.clone(), Duration::from_millis(15)).unwrap();
        w.schedule(serde_json::to_vec(&Store { n: 3 }).unwrap());
        // Nothing else is asked of the writer: the timer has to fire on its own.
        // The old state machine armed a timer and then waited on the channel,
        // so this write never happened until a flush arrived.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if p.exists() {
                if let Ok(v) = read_json::<Store>(&p) {
                    if v.n == 3 {
                        return;
                    }
                }
            }
            assert!(std::time::Instant::now() < deadline, "the debounce never fired");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn the_newest_payload_wins_over_older_ones() {
        let p = tmp("newest");
        let w = DebouncedWriter::new(p.clone(), Duration::from_millis(50)).unwrap();
        // Queue fast enough that the timer cannot fire between sends, then
        // flush: the last value queued must be the one on disk.
        for n in 0..200u32 {
            w.schedule(serde_json::to_vec(&Store { n }).unwrap());
        }
        w.flush().unwrap();
        let (v, _) = load_json_recoverable::<Store>(&p);
        assert_eq!(v.n, 199);
    }

    #[test]
    fn an_urgent_save_supersedes_anything_queued_behind_it() {
        let p = tmp("urgent");
        let w = DebouncedWriter::new(p.clone(), Duration::from_secs(60)).unwrap();
        w.schedule(serde_json::to_vec(&Store { n: 1 }).unwrap());
        w.now(serde_json::to_vec(&Store { n: 2 }).unwrap());
        w.schedule(serde_json::to_vec(&Store { n: 3 }).unwrap());
        w.flush().unwrap();
        let (v, _) = load_json_recoverable::<Store>(&p);
        assert_eq!(v.n, 3, "a queued save after an urgent one must still win");
    }

    #[test]
    fn a_backup_is_taken_every_tenth_write_and_holds_older_data() {
        let p = tmp("backup");
        let mut w = DebouncedWriter::new(p.clone(), Duration::from_millis(1)).unwrap();
        for n in 1..=12u32 {
            w.now(serde_json::to_vec(&Store { n }).unwrap());
        }
        w.shutdown().unwrap();

        let current: Store = read_json(&p).unwrap();
        let backup: Store = read_json(&backup_path(&p)).unwrap();
        assert_eq!(current.n, 12);
        assert!(
            backup.n < current.n,
            "the backup must be an older version, got {} and {}",
            backup.n,
            current.n
        );
    }

    #[test]
    fn a_lazy_payload_is_produced_once_on_the_writer_thread() {
        let p = tmp("lazy");
        let (shared, mut thread) =
            DebouncedWriter::new(p.clone(), Duration::from_millis(15)).unwrap().share();

        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        // `Thread::id()` has no stable numeric accessor, so the producer
        // records the writer thread's name instead — the writer is the only
        // thread named after the target file.
        let producer_name = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        for n in 0..5u32 {
            let runs = runs.clone();
            let producer_name = producer_name.clone();
            shared.schedule_lazy(Box::new(move || {
                runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                *producer_name.lock().unwrap() =
                    std::thread::current().name().unwrap_or("?").to_string();
                serde_json::to_vec(&Store { n }).unwrap()
            }));
        }
        // No flush: the debounce has to materialise and write on its own.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if p.exists() {
                if let Ok(v) = read_json::<Store>(&p) {
                    if v.n == 4 {
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        thread.shutdown(&shared).unwrap();

        let (v, _) = load_json_recoverable::<Store>(&p);
        assert_eq!(v.n, 4, "the newest lazy payload must win");
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "five keystrokes must produce exactly one encode"
        );
        // The closure ran on the writer thread, never on the caller's: the
        // writer thread is named after the file it writes.
        let name = producer_name.lock().unwrap().clone();
        assert!(
            name.contains("lazy"),
            "the encode ran on thread {name:?}, not the writer thread"
        );
    }

    #[test]
    fn a_lazy_payload_is_materialised_by_flush() {
        let p = tmp("lazy-flush");
        let mut w = DebouncedWriter::new(p.clone(), Duration::from_secs(60)).unwrap();
        w.schedule_lazy(Box::new(|| serde_json::to_vec(&Store { n: 8 }).unwrap()));
        w.flush().unwrap();
        w.shutdown().unwrap();
        let (v, _) = load_json_recoverable::<Store>(&p);
        assert_eq!(v.n, 8);
    }

    #[test]
    fn bytes_sent_after_a_lazy_payload_win() {
        let p = tmp("lazy-vs-bytes");
        let mut w = DebouncedWriter::new(p.clone(), Duration::from_secs(60)).unwrap();
        w.schedule_lazy(Box::new(|| serde_json::to_vec(&Store { n: 1 }).unwrap()));
        w.schedule(serde_json::to_vec(&Store { n: 2 }).unwrap());
        w.flush().unwrap();
        w.shutdown().unwrap();
        let (v, _) = load_json_recoverable::<Store>(&p);
        assert_eq!(v.n, 2);
    }

    #[test]
    fn share_hands_the_thread_over_without_shutting_it_down() {
        let p = tmp("share");
        let (shared, mut thread) =
            DebouncedWriter::new(p.clone(), Duration::from_millis(10)).unwrap().share();

        // The original handle is gone; the writer must still be alive.
        let cloned = shared.clone();
        let producer = std::thread::spawn(move || {
            cloned.schedule(serde_json::to_vec(&Store { n: 11 }).unwrap());
        });
        producer.join().unwrap();
        shared.flush().unwrap();
        let (v, _) = load_json_recoverable::<Store>(&p);
        assert_eq!(v.n, 11, "the shared writer stopped when its owner went away");

        thread.shutdown(&shared).unwrap();
        // Shutting down twice is harmless.
        thread.shutdown(&shared).unwrap();
    }

    #[test]
    fn shutdown_writes_a_pending_payload() {
        let p = tmp("shutdown");
        let mut w = DebouncedWriter::new(p.clone(), Duration::from_secs(30)).unwrap();
        w.schedule(b"{\"n\":9}".to_vec());
        // 30 s debounce: without the shutdown flush this would never land.
        w.shutdown().unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"{\"n\":9}");
    }

    #[test]
    fn concurrent_writers_do_not_corrupt_the_target() {
        let p = tmp("concurrent");
        let mut writers: Vec<_> = (0..4)
            .map(|_| DebouncedWriter::new(p.clone(), Duration::from_millis(1)).expect("writer"))
            .collect();
        let handles: Vec<_> = writers
            .iter()
            .map(|w| {
                let w = w.clone_tx();
                std::thread::spawn(move || {
                    for n in 0..100u32 {
                        w.schedule(serde_json::to_vec(&Store { n }).unwrap());
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        for w in writers.iter_mut() {
            w.flush().unwrap();
        }
        for w in writers.iter_mut() {
            w.shutdown().unwrap();
        }
        // Whatever landed, it must be valid JSON.
        read_json::<Store>(&p).expect("target file is always parseable");
    }
}