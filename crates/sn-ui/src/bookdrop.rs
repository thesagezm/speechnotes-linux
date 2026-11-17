//! BookDrop inside the app: the listener's life, where landed files go, and
//! the sender's worker threads.
//!
//! `sn_net::bookdrop` owns the receive wire (LocalSend v2.2: sessions,
//! tokens, sha256) and `sn_net::sender` the send wire (discovery, prepare,
//! upload). This module owns everything around them that only an app knows:
//! the device identity, the listener thread, the import router, and the
//! channels the settings page drains on the UI thread.
//!
//! Two rules the design keeps:
//!
//! * **The UI thread never blocks.** `start` binds the port (the one step
//!   whose failure the user must hear) and hands the listener to a thread;
//!   scans and sends run whole on worker threads and report through
//!   channels a glib timeout drains.
//! * **One poll owns the receiver's state.** [`attach_app_poll`] — started
//!   once at activate, running for the app's whole life — re-reads the
//!   prefs, absorbs the receiver's history into the mirror the settings
//!   page reads, and refreshes the app's panes when a file lands. A settings
//!   window that is closed changes none of that: transfers land, the shelf
//!   updates, the history remembers.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};

use gtk4::glib;

use crate::app::App;

/// How often the polls run (ms).
pub const POLL_MS: u64 = 700;

/// BookDrop's running state: the receiver, its listener thread, and the
/// counters the status line reads. Shared as `Arc` — the listener thread
/// holds one, the UI thread another.
pub struct Service {
    receiver: Arc<sn_net::bookdrop::Receiver>,
    /// The port the listener thread last bound (0 = not running). The
    /// receiver knows its bound port too, but only after `serve` runs; this
    /// flips to 0 again the moment the thread exits, which is what "running"
    /// means to the status line.
    bound: Arc<AtomicU16>,
    last_error: Arc<Mutex<Option<String>>>,
    /// Set when a file landed and the app's panes need a rebuild. The app
    /// poll drains it on the UI thread — the import itself happens on the
    /// listener thread, where GTK must not be touched. An atomic, because
    /// the flag is set on that thread and read on this one.
    refresh: Arc<AtomicBool>,
    /// The transfer history the UI reads: drained from the receiver by the
    /// app poll, so a record that landed while every window was closed is
    /// still in the list when settings next opens. Newest first, capped.
    events: Mutex<Vec<sn_net::bookdrop::TransferRecord>>,
    /// The accept-loop thread of the current (or last) listener. `stop`
    /// joins it: the thread's socket dies WITH the thread, and a start
    /// that races that death finds 53317 still bound, falls to 53318, and
    /// repeated on every toggle walks the whole port range.
    listener_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The send half's live state (Fixed 2026-10-30: the send UI).
    /// Lives on the app-lifetime `Service`, not the page, so a send keeps
    /// running — and keeps reporting — while settings is closed.
    send: Mutex<SendState>,
}

/// What an outbound transfer is doing, as the page's progress line reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct SendProgress {
    /// True while a send is still in flight.
    pub active: bool,
    /// The whole story in one line: peer, phase, file counts.
    pub line: String,
    /// `finished/total` when the sender has announced a total; the bar.
    pub fraction: Option<f64>,
    /// A finished send's summary, kept until the next send clears it.
    pub done: Option<Result<String, String>>,
}

#[derive(Default)]
struct SendState {
    /// The in-flight send's phase channel, drained by the app poll.
    rx: Option<std::sync::mpsc::Receiver<sn_net::sender::SendPhase>>,
    /// Who is receiving, for the progress line.
    peer: Option<String>,
    /// The latest phase text.
    phase: String,
    /// `(files finished, files total)` once the sender announces them.
    counts: Option<(usize, usize)>,
    /// A finished send's summary.
    done: Option<Result<String, String>>,
}

impl Service {
    /// Build from the prefs store: identity and auto-accept come from what
    /// is stored, so a restart resumes the same configuration.
    pub fn with_stores(
        prefs: Option<&sn_store::PrefsStore>,
        notes: Arc<sn_store::NotesStore>,
        notebooks: Arc<sn_store::NotebooksStore>,
    ) -> Arc<Self> {
        let p = prefs.map(|p| p.get());
        let fingerprint = p
            .as_ref()
            .map(|p| p.book_drop_fingerprint.clone())
            .filter(|f| !f.is_empty())
            .unwrap_or_else(sn_core::util::new_uuid);
        let port = sn_net::bookdrop::DEFAULT_PORT;
        let receiver = sn_net::bookdrop::Receiver::new(
            sn_net::bookdrop::device_alias(),
            fingerprint,
            port,
        );
        if let Some(p) = &p {
            receiver.set_pin(pin_option(&p.book_drop_pin));
            receiver
                .auto_accept
                .store(p.book_drop_auto_accept, Ordering::Relaxed);
        }
        // The router is the importer table. It runs on the listener thread,
        // which is why the stores arrive as `Arc` and nothing here touches
        // GTK: a landed file must import while the UI thread is mid-frame.
        let refresh = Arc::new(AtomicBool::new(false));
        {
            let notes = notes.clone();
            let notebooks = notebooks.clone();
            let refresh_for_router = refresh.clone();
            receiver.set_router(Box::new(move |path| {
                let outcome = route(path, &notes, &notebooks);
                if outcome.is_ok() {
                    refresh_for_router.store(true, Ordering::Relaxed);
                }
                outcome
            }));
        }
        Arc::new(Self {
            receiver: Arc::new(receiver),
            bound: Arc::new(AtomicU16::new(0)),
            last_error: Arc::new(Mutex::new(None)),
            refresh,
            events: Mutex::new(Vec::new()),
            listener_thread: Mutex::new(None),
            send: Mutex::new(SendState::default()),
        })
    }

    /// The service for a windowed app: stores from `App`, prefs reapplied.
    pub fn for_app(app: &Rc<App>) -> Arc<Self> {
        let service =
            Self::with_stores(Some(&app.prefs), app.notes.clone(), app.notebooks.clone());
        // Persist the fingerprint this install generated, so the same machine
        // never appears as two peers after a prefs round trip.
        let fp = service.receiver.device().fingerprint;
        if app.prefs.get().book_drop_fingerprint != fp {
            app.prefs.update(|p| p.book_drop_fingerprint = fp);
        }
        service
    }

    /// Bind the first free port in the protocol's range and serve on a
    /// thread. The bind happens here, on the caller's thread, so a fully
    /// taken range is an error the caller can show instead of a thread that
    /// dies silently. The bound port is whatever was free — senders read it
    /// from our register DTO, so 53318 serves exactly as well as 53317.
    pub fn start(&self) -> std::io::Result<()> {
        if self.is_running() {
            return Ok(());
        }
        // A previous stop that is still winding its listener down holds
        // the low ports: join first, so this bind sees them released and
        // lands back on 53317 instead of creeping up the range one
        // toggle at a time.
        self.join_listener();
        // A previous run's error is history once a new one begins.
        *self.last_error.lock().unwrap() = None;
        let listener = self.receiver.bind()?;
        self.bound
            .store(self.receiver.device().port, Ordering::Relaxed);
        let receiver = self.receiver.clone();
        let bound = self.bound.clone();
        let handle = std::thread::spawn(move || {
            receiver.serve_listener(listener);
            // The thread's whole life was the accept loop: when it returns,
            // the listener is down and the status line must say so.
            bound.store(0, Ordering::Relaxed);
            tracing::info!("bookdrop listener stopped");
        });
        *self.listener_thread.lock().unwrap() = Some(handle);
        Ok(())
    }

    /// Stop the listener. Safe to call when it is not running.
    pub fn stop(&self) {
        self.receiver.stop();
        self.join_listener();
        self.bound.store(0, Ordering::Relaxed);
    }

    /// Wait for the accept-loop thread to actually exit. The receiver's
    /// `stop` self-connects to wake the loop, so the join is prompt —
    /// bounded by whatever single request the loop may still be serving.
    fn join_listener(&self) {
        if let Some(handle) = self.listener_thread.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    /// This install's LocalSend identity, stable across runs. The sender
    /// publishes the same one, so a peer never sees this machine twice.
    pub fn fingerprint(&self) -> String {
        self.receiver.device().fingerprint
    }

    pub fn is_running(&self) -> bool {
        self.bound.load(Ordering::Relaxed) != 0
    }

    /// The port the listener actually bound, or the LocalSend default when
    /// it is not up yet (the status line shows where it *would* listen).
    pub fn port(&self) -> u16 {
        let bound = self.bound.load(Ordering::Relaxed);
        if bound != 0 {
            bound
        } else {
            sn_net::bookdrop::DEFAULT_PORT
        }
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap().clone()
    }

    /// Current transfer progress as `(files landed, files offered)`, or
    /// `None` when the wire is idle. The settings page draws a line from it.
    pub fn progress(&self) -> Option<(usize, usize)> {
        self.receiver.progress()
    }

    /// The transfer history as the UI reads it: newest first, capped, and
    /// never drained — the app poll keeps the mirror current instead.
    pub fn snapshot_events(&self) -> Vec<sn_net::bookdrop::TransferRecord> {
        let mut events = self.events.lock().unwrap();
        events.truncate(sn_net::bookdrop::HISTORY_CAP);
        events.clone()
    }

    /// Absorb what the receiver recorded since the last call into the
    /// mirror. The app poll calls this on every tick; the headless receiver
    /// calls it through [`Self::drain_events`].
    fn absorb_events(&self) {
        let fresh = self.receiver.take_history();
        if fresh.is_empty() {
            return;
        }
        let mut events = self.events.lock().unwrap();
        let mut merged = fresh;
        merged.append(&mut events);
        merged.truncate(sn_net::bookdrop::HISTORY_CAP);
        *events = merged;
    }

    /// History since the last call, newest first — the draining form the
    /// headless receiver prints from.
    pub fn drain_events(&self) -> Vec<sn_net::bookdrop::TransferRecord> {
        self.absorb_events();
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    // ---- the send half ---------------------------------------------------

    /// Start sending `items` to `peer` on a worker thread. The progress
    /// channel lands in [`Self::send`], where the app poll drains it — the
    /// send outlives the settings page that started it.
    //  the send UI the plumbed-but-unwired
    // `send_to_peer` was waiting for.
    pub fn start_send(
        &self,
        items: Vec<sn_net::sender::SendItem>,
        peer: &sn_net::sender::Peer,
        pin: &str,
    ) {
        let rx = send_to_peer(items, peer.clone(), self.fingerprint(), pin.to_string());
        let mut send = self.send.lock().unwrap();
        send.rx = Some(rx);
        send.peer = Some(peer.label());
        send.phase = "Preparing…".to_string();
        send.counts = None;
        send.done = None;
    }

    /// Drain the in-flight send's phase channel into `send`. The app poll
    /// calls this every tick; a send started from a since-closed settings
    /// page still reports here.
    pub fn poll_send(&self) {
        let mut send = self.send.lock().unwrap();
        let Some(rx) = send.rx.take() else {
            return;
        };
        let mut alive = true;
        loop {
            match rx.try_recv() {
                Ok(sn_net::sender::SendPhase::Preparing) => {
                    send.phase = "Preparing…".to_string();
                }
                Ok(sn_net::sender::SendPhase::Uploading(finished, total)) => {
                    send.counts = Some((finished, total));
                }
                Ok(sn_net::sender::SendPhase::Done(summary)) => {
                    send.phase = String::new();
                    send.done = Some(Ok(summary));
                }
                Ok(sn_net::sender::SendPhase::Failed(why)) => {
                    send.phase = String::new();
                    send.done = Some(Err(why));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    alive = false;
                    break;
                }
            }
        }
        if alive {
            send.rx = Some(rx);
        }
    }

    /// The outbound transfer's state, for the page's progress bar.
    pub fn send_progress(&self) -> SendProgress {
        let send = self.send.lock().unwrap();
        let active = send.rx.is_some();
        let mut line = match (&send.peer, active) {
            (Some(peer), true) => format!("Sending to {peer}"),
            _ => String::new(),
        };
        if let Some((finished, total)) = send.counts {
            if total > 0 {
                if !line.is_empty() {
                    line.push_str(" — ");
                }
                line.push_str(&format!("{finished} of {total} file(s) sent"));
            }
        }
        if active && !send.phase.is_empty() {
            if !line.is_empty() {
                line.push_str(" — ");
            }
            line.push_str(&send.phase);
        }
        SendProgress {
            active,
            line,
            fraction: send.counts.and_then(|(f, t)| {
                if t > 0 {
                    Some(f as f64 / t as f64)
                } else {
                    None
                }
            }),
            done: send.done.clone(),
        }
    }

    /// Re-read the two settings that can change without a restart:
    /// a pref read is an `RwLock` read of a small struct, cheaper than a
    /// change signal we would have to plumb.
    pub fn sync_prefs(&self, prefs: &sn_store::Prefs) {        self.receiver
            .auto_accept
            .store(prefs.book_drop_auto_accept, Ordering::Relaxed);
        self.receiver.set_pin(pin_option(&prefs.book_drop_pin));
    }

    /// True when a file landed since the last call and the app's panes need
    /// a rebuild.
    pub fn take_refresh(&self) -> bool {
        self.refresh.swap(false, Ordering::Relaxed)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        // The listener thread outlives nothing else: stopping wakes it,
        // and the join lets its socket go before the process moves on.
        self.stop();
    }
}

fn pin_option(pin: &str) -> Option<String> {
    if pin.is_empty() {
        None
    } else {
        Some(pin.to_string())
    }
}

// ---------------------------------------------------------------------------
// The app poll: one timer, the app's whole life
// ---------------------------------------------------------------------------

/// The receiver's heartbeat, started once at activate and never removed.
///
/// This is the fix for the settings-owned poll: a transfer that lands while
/// every settings window is closed still re-reads the prefs, still lands in
/// the history mirror, and still rebuilds the notes list and the shelf.
/// The BookDrop settings page draws from the same state without owning it.
pub fn attach_app_poll(app: &Rc<App>) -> glib::SourceId {
    let app = app.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(POLL_MS), move || {
        let service = app.bookdrop.clone();
        let prefs = app.prefs.get();
        service.sync_prefs(&prefs);
        service.absorb_events();
        service.poll_send();
        if service.take_refresh() {
            crate::window::refresh_all(&app);
        }
        glib::ControlFlow::Continue
    })
}

/// The settings page's own tick: draw-only. It adds nothing to the service's
/// state (the app poll owns that) — it exists so the page's widgets update
/// while the page is open, and it is removed when the page is left.
pub fn attach_poll<F>(mut on_tick: F) -> glib::SourceId
where
    F: FnMut() + 'static,
{
    glib::timeout_add_local(std::time::Duration::from_millis(POLL_MS), move || {
        on_tick();
        glib::ControlFlow::Continue
    })
}

// ---------------------------------------------------------------------------
// The sender: worker threads that report through channels
// ---------------------------------------------------------------------------

/// Scan this network for BookDrop receivers on a worker thread. The channel
/// yields exactly one message — the peers found (empty when none answered)
/// — and closes; the UI's poll drains it.
pub fn scan_for_peers(
    fingerprint: String,
) -> std::sync::mpsc::Receiver<Vec<sn_net::sender::Peer>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Every subnet scans at once: the getifaddrs enumeration returns
        // several (Wi-Fi, hotspot, a container bridge), and a sequential
        // walk stacked their sweep times — seconds the peer chooser made
        // the user wait for an answer that was already on the first one.
        let handles: Vec<_> = sn_net::sender::local_networks()
            .into_iter()
            .map(|network| {
                let fingerprint = fingerprint.clone();
                std::thread::spawn(move || {
                    sn_net::sender::discover(&network, &fingerprint)
                })
            })
            .collect();
        let mut peers = Vec::new();
        for handle in handles {
            peers.extend(handle.join().unwrap_or_default());
        }
        // A machine reachable on two scanned interfaces is one device, not
        // two rows in the picker.
        let mut seen = std::collections::HashSet::new();
        peers.retain(|peer| seen.insert(peer.fingerprint.clone()));
        let _ = tx.send(peers);
    });
    rx
}

/// Resolve an address the user typed into a peer, on a worker thread.
///
/// The channel yields exactly one message — the peer, or why the address
/// could not be read — and closes; the UI's poll drains it. The lookup
/// probes the host (the typed port first, then the protocol's range), so a
/// receiver that took a port of its own is still found; an address that
/// answers nothing is still offered, because the transfer is a separate
/// exchange from the discovery handshake.
pub fn resolve_manual_peer(
    address: String,
    fingerprint: String,
) -> std::sync::mpsc::Receiver<Result<sn_net::sender::Peer, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(sn_net::sender::manual_peer(&address, &fingerprint));
    });
    rx
}

/// Send files to a peer on a worker thread. The channel yields each phase
/// as the send reaches it (preparing, per-file progress, done or failed)
/// and then closes; the UI's poll drains it.
pub fn send_to_peer(
    items: Vec<sn_net::sender::SendItem>,
    peer: sn_net::sender::Peer,
    fingerprint: String,
    pin: String,
) -> std::sync::mpsc::Receiver<sn_net::sender::SendPhase> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let reporter = tx.clone();
        let outcome = sn_net::sender::send_files(
            &items,
            &peer,
            &fingerprint,
            if pin.is_empty() { None } else { Some(pin.as_str()) },
            |phase| {
                let _ = reporter.send(phase);
            },
        );
        if let Err(why) = outcome {
            let _ = tx.send(sn_net::sender::SendPhase::Failed(why));
        }
    });
    rx
}

// ---------------------------------------------------------------------------
// Routing: where a landed file goes
// ---------------------------------------------------------------------------

/// Route one landed file to its importer, returning where it went (the
/// history row's text) or why it did not. Runs on the listener thread.
///
/// The table is the iOS router's, minus the formats this app does not read:
/// `.jex` → notes+notebooks, `.md`/`.markdown`/`.txt`/`.text` → one note,
/// `.docx` → one note with its images, audio → the audiobook library,
/// `.epub`/`.pdf`/the headless document formats → the shelf, anything else
/// refused by name. (.txt/.text keep their note behavior here — the notes
/// arm above takes them before the documents arm could.)
fn route(
    path: &std::path::Path,
    notes: &sn_store::NotesStore,
    notebooks: &sn_store::NotebooksStore,
) -> Result<String, String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    // The sender's file name (landed under `<fileId>-<name>`) for messages.
    let shown = display_name(path);
    match ext.as_str() {
        "jex" => {
            let report = sn_store::imports::import_jex(notes, notebooks, path)
                .map_err(|e| e.to_string())?;
            notes.flush();
            notebooks.flush();
            Ok(format!(
                "library ({} note(s), {} notebook(s))",
                report.notes_imported, report.notebooks_created
            ))
        }
        "md" | "markdown" | "txt" | "text" => {
            // Capped and lossy: a "note" can arrive from a LAN peer whose
            // size the sender invented (a capped read refuses the giant
            // one), and a note with a stray invalid byte still imports as
            // text rather than failing outright.
            let bytes = sn_core::util::read_capped(path, 16 * 1024 * 1024)
                .map_err(|e| e.to_string())?;
            let body = String::from_utf8_lossy(&bytes).into_owned();
            let note = notes.create(None);
            notes.update_text(&note.id, &body);
            notes.flush();
            Ok("notes".to_string())
        }
        "docx" => {
            let report = sn_store::imports::import_docx(notes, path, None)
                .map_err(|e| e.to_string())?;
            notes.flush();
            Ok(format!("notes ({} image(s))", report.images_stored))
        }
        // A WAV is a recording, not a book: it lands in the exports folder
        // beside the ones this app's own pipelines write — the same rule
        // the iOS router applies ("Saved to Exported audio").
        "wav" => {
            let dir = sn_core::paths::exports_dir();
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut target = dir.join(
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "recording.wav".to_string()),
            );
            if target.exists() {
                let stem = target
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "recording".to_string());
                target = dir.join(format!("{}-{}.wav", stem, sn_core::util::new_uuid()));
            }
            std::fs::copy(path, &target).map_err(|e| e.to_string())?;
            Ok("exported audio".to_string())
        }
        "epub" => {
            let book = sn_store::books::BooksStore::import(path).map_err(|e| e.to_string())?;
            Ok(format!("library “{}”", book.manifest.title))
        }
        "pdf" => {
            let book =
                sn_store::books::BooksStore::import_pdf(path).map_err(|e| e.to_string())?;
            Ok(format!("library “{}”", book.manifest.title))
        }
        "pptx" | "ppsx" | "potx" | "ppt" | "odp" => {
            let book = sn_store::books::BooksStore::import_slides(path)
                .map_err(|e| e.to_string())?;
            Ok(format!("library “{}”", book.manifest.title))
        }
        // The headless documents — the Mobipocket/Kindle family, FB2, RTF,
        // HTML/HTMLZ — land as books like any other. (.txt/.text never
        // reach this arm: the notes arm above takes them, which is the
        // documented note behavior BookDrop keeps.)
        ext if sn_doc::bookdoc::DocFamily::is_document_extension(ext) => {
            let book = sn_store::books::BooksStore::import_document(path)
                .map_err(|e| e.to_string())?;
            Ok(format!("library “{}”", book.manifest.title))
        }
        "m4b" | "m4a" | "mp4" | "mp3" | "opus" | "ogg" | "oga" => {
            let book = sn_store::books::BooksStore::import_audiobook(path)
                .map_err(|e| e.to_string())?;
            Ok(format!("library “{}”", book.manifest.title))
        }
        other => Err(format!("{shown}: a .{other} file has no importer here")),
    }
}

/// The file name a landed path displays as: `<fileId>-<original>` strips the
/// id, so history rows read like the sender named them.
fn display_name(path: &std::path::Path) -> String {
    let raw = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match raw.split_once('-') {
        Some((id, rest)) if id.len() >= 32 => rest.to_string(),
        _ => raw,
    }
}
