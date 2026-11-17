//! BookDrop: the LocalSend v2.2 receiver.
//!
//! Any LocalSend app (the desktop/mobile client, Readest's Nearby BookDrop,
//! another Speechnotes) can push files to this app over the LAN. The
//! receiver announces nothing: LocalSend senders find peers by scanning the
//! /24 subnet with unicast HTTP POSTs to `/api/localsend/v2/register`, so
//! the only requirement is an HTTP server up on the LAN. The protocol
//! surface is v2.2 of <https://github.com/localsend/protocol>; the iOS
//! implementation (`App/Sources/Services/LocalSend/`) is the reference.
//!
//! The rules this module exists to hold, all learned on the iOS port:
//!
//! * **Validate session, file, token AND sender IP on every upload** — a
//!   third party on the LAN must not inject files into someone's session.
//! * **`sha256` (when offered) verified → 422 on mismatch.** The digest is
//!   streamed, never materialized: book files reach hundreds of MB.
//! * **Bodies spill to disk, always.** Nothing here reads a whole upload
//!   into memory.
//! * **One active session at a time** → 409 for a second prepare. Sessions
//!   time out (5 min) and clean their temp files.
//! * **Omitted file ids count as rejected** (partial accept is the spec's
//!   answer to "I can take some of these").
//! * **On-disk names are the RECEIVER's, never the sender's id.** The file
//!   map's keys are attacker-chosen strings; building a landing path from
//!   one let a `files: {"../../evil": …}` prepare write outside the
//!   session's temp directory — anywhere the user can write. Every disk
//!   name now starts with an id this side generated.
//! * **Sessions carry a budget** — at most [`MAX_SESSION_FILES`] files and
//!   [`MAX_SESSION_BYTES`] bytes, no single file over [`MAX_FILE_BYTES`].
//!   Without it a prepare could promise a terabyte of "books" and a
//!   willing sender could fill the disk.
//! * **A wrong access code gets expensive.** At LAN latency a /24 can try
//!   thousands of codes a second; after three consecutive failures each
//!   further wrong one stalls its answer on an exponential climb to 30 s.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::os::unix::fs::PermissionsExt;

use sha2::Digest as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The default port the protocol negotiates around. All ports are
/// negotiable — a sender reads the port from our register response — but
/// this is what every LocalSend app tries first.
pub const DEFAULT_PORT: u16 = 53317;

/// The port window every LocalSend app probes and the iOS receiver binds
/// into: the first free port from 53317 to 53327. Binding only 53317 fails
/// outright on a machine that also runs the LocalSend desktop app — the two
/// apps must coexist, and the sender honours the port our DTO announces.
pub const PORT_RANGE: std::ops::RangeInclusive<u16> = 53317..=53327;

/// How many transfer records the history list keeps.
pub const HISTORY_CAP: usize = 20;

/// Extensions this app routes once landed. This is the iOS accept list minus
/// the formats with no importer here (`wav` lands in iOS's exports folder,
/// which this app does not have) plus `.docx`, which imports as a note with
/// images, and the headless book formats (`mobi`/`azw`/`azw3`/`prc`/`pdb`,
/// `fb2`, `rtf`, `html`/`htm`/`htmlz`), which import as books — `.txt`
/// stays a note, as documented. A format the sender offers that is not in
/// this list counts as rejected — the spec's partial accept.
pub const ACCEPTED_EXTENSIONS: [&str; 25] = [
    "epub", "pdf", "m4b", "m4a", "mp4", "mp3", "jex", "opus", "ogg", "oga", "md", "markdown",
    "txt", "text", "docx", "mobi", "azw", "azw3", "prc", "pdb", "fb2", "rtf", "html", "htm",
    "htmlz",
];

/// How long a prepared session waits for its files before it is discarded.
pub const SESSION_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The largest single file this receiver will land. Honest books reach a
/// few hundred MB; past two gigabytes a "book" is a disk-fill attack.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The largest total one session may land, and how many files it may carry.
pub const MAX_SESSION_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_SESSION_FILES: usize = 64;

/// Body bytes read from a refused or over-shooting sender before its socket
/// is dropped. Beyond this the answer is a closed connection, which is all
/// a peer this side has already rejected deserves — the previous 1 GiB
/// drain let one hostile POST occupy the single-threaded receiver for
/// minutes.
const MAX_DRAIN_BYTES: usize = 1024 * 1024;

/// How the access-code gate escalates: free failures before this many, then
/// the climb. Three typos must not cost a human anything.
const PIN_GRACE_FAILURES: u32 = 3;
/// The ceiling of the wrong-code stall.
const PIN_MAX_STALL: Duration = Duration::from_secs(30);

/// The device DTO every response carries. `deviceType` must be present: a
/// missing one made Readest's phone show up as a "computer".
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeviceInfo {
    pub alias: String,
    pub version: String,
    #[serde(rename = "deviceModel")]
    pub device_model: Option<String>,
    #[serde(rename = "deviceType")]
    pub device_type: String,
    pub fingerprint: String,
    pub port: u16,
    pub protocol: String,
    pub download: bool,
}

/// One file the sender offers in prepare-upload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileMeta {
    #[allow(dead_code)] // part of the wire shape; sessions key on the map key
    pub id: String,
    #[serde(rename = "fileName")]
    pub file_name: String,
    pub size: u64,
    /// The spec (and the iOS sender) treat this as optional: the iOS
    /// `SendItem.fileType` is nil for files the share sheet has no category
    /// for, and a required field here turned "iPhone sends a note" into a
    /// 400. Default to empty and filter on the file name's extension.
    #[serde(rename = "fileType", default)]
    pub file_type: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

/// The prepare-upload request body. `pin` rides in the body (and again on
/// every upload's query string), so its absence must not be a decode error.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PrepareUpload {
    pub info: DeviceInfo,
    #[serde(default)]
    pub pin: Option<String>,
    pub files: HashMap<String, FileMeta>,
}

/// One landed (or failed) transfer, for the history list. `at` is when the
/// file landed — the iOS record's `Date`, so a history row can say when, not
/// only what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRecord {
    pub name: String,
    pub size: u64,
    pub outcome: TransferOutcome,
    pub at: std::time::SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferOutcome {
    /// The file landed and was routed; the payload is where it went.
    Imported(String),
    Failed(String),
    Rejected,
}

/// A transfer in progress.
struct Session {
    // The session's id is its map key; carrying it again is for debug logs.
    #[allow(dead_code)]
    id: String,
    sender_host: Option<String>,
    dir: PathBuf,
    files: HashMap<String, FileMeta>,
    tokens: HashMap<String, String>,
    /// file id → the on-disk name THIS side chose at prepare. Never built
    /// from the sender's id again: the map keys are arbitrary strings, and
    /// a path made of them is a traversal, not a name.
    disk_names: HashMap<String, String>,
    received: std::collections::HashSet<String>,
    created: std::time::Instant,
}

impl Drop for Session {
    fn drop(&mut self) {
        // A session's temp directory goes with it, whatever ended it.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The receiver. `take_history` is what the UI polls; `router` is what a
/// landed file is handed to (set once at startup).
pub struct Receiver {
    device: DeviceInfo,
    /// One-shot status from `admit_upload` back to `handle_connection`
    /// (single-threaded accept loop, so a plain slot is enough).
    admission_status: std::sync::Mutex<Option<u16>>,
    /// The port actually bound, updated at serve() — the DTO reads this.
    bound_port: Arc<AtomicU16>,
    /// Auto-accept is the BookDrop setting: off means every prepare gets 403.
    pub auto_accept: Arc<AtomicBool>,
    /// The receive-side access code; `None`/empty means the LAN peer needs
    /// none. Re-read from the prefs on every admission (like iOS), so setting
    /// a code does not need a restart.
    required_pin: Arc<Mutex<Option<String>>>,
    /// Consecutive wrong-code attempts, and when the latest happened. The
    /// stall grows from the fourth; a quiet spell of five minutes forgives.
    pin_gate: Arc<Mutex<PinGate>>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    history: Arc<Mutex<Vec<TransferRecord>>>,
    running: Arc<AtomicBool>,
    /// Where a landed file goes next. Returns where it was routed, or the
    /// reason it was not.
    router: Arc<Mutex<Option<Box<dyn Fn(&std::path::Path) -> Result<String, String> + Send>>>>,
}

/// The wrong-code gate's memory.
#[derive(Default)]
struct PinGate {
    fails: u32,
    last_failure: Option<std::time::Instant>,
}

impl Receiver {
    /// Build a receiver with this device's identity.
    pub fn new(alias: String, fingerprint: String, port: u16) -> Self {
        Self {
            admission_status: std::sync::Mutex::new(None),
            bound_port: Arc::new(AtomicU16::new(port)),
            device: DeviceInfo {
                alias,
                version: "2.2".to_string(),
                device_model: None,
                // "desktop", not "headless": the iOS peer list shows
                // "Computer" for a desktop and a bare "Device" for headless,
                // and this machine is a computer the user can see.
                device_type: "desktop".to_string(),
                fingerprint,
                port,
                protocol: "http".to_string(),
                download: false,
            },
            auto_accept: Arc::new(AtomicBool::new(true)),
            required_pin: Arc::new(Mutex::new(None)),
            pin_gate: Arc::new(Mutex::new(PinGate::default())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            history: Arc::new(Mutex::new(Vec::new())),
            running: Arc::new(AtomicBool::new(false)),
            router: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_router(
        &self,
        router: Box<dyn Fn(&std::path::Path) -> Result<String, String> + Send>,
    ) {
        *self.router.lock().unwrap() = Some(router);
    }

    /// Set (or clear, with `None`/empty) the access code. Wire-checked at
    /// prepare and again on every upload, so a code typed into settings
    /// protects the very next transfer without a restart.
    pub fn set_pin(&self, pin: Option<String>) {
        let pin = pin.filter(|p| !p.is_empty());
        *self.required_pin.lock().unwrap() = pin;
    }

    /// True when the offer's code satisfies the current requirement.
    ///
    /// The offered and required codes are trimmed the way the iOS receiver
    /// trims them: a code typed with a trailing space in either app's
    /// settings must not lock the other one out.
    fn pin_ok(&self, offered: Option<&str>) -> bool {
        let required = self.required_pin.lock().unwrap();
        match required.as_deref() {
            None | Some("") => true,
            Some(required) => offered.is_some_and(|o| o.trim() == required.trim()),
        }
    }

    /// Record one wrong code and return how long its answer must be held
    /// back. The first [`PIN_GRACE_FAILURES`] failures are free (a human
    /// retyping); after that each further one costs twice the previous,
    /// capped at [`PIN_MAX_STALL`] — at LAN speed a bare 401 turns a
    /// four-digit code into a five-minute crack, and a stall is the only
    /// brake a single-threaded receiver needs.
    fn note_pin_failure(&self) -> Duration {
        let mut gate = self.pin_gate.lock().unwrap();
        if gate.last_failure.is_some_and(|t| t.elapsed() > Duration::from_secs(300)) {
            gate.fails = 0;
        }
        gate.fails = gate.fails.saturating_add(1);
        gate.last_failure = Some(std::time::Instant::now());
        pin_stall_for(gate.fails)
    }

    /// A code that satisfied resets the climb.
    fn note_pin_success(&self) {
        let mut gate = self.pin_gate.lock().unwrap();
        gate.fails = 0;
        gate.last_failure = None;
    }

    /// The transfer history, newest first, capped, without draining — the
    /// UI's live read. [`Self::take_history`] remains the draining form the
    /// headless receiver prints from.
    pub fn history_snapshot(&self) -> Vec<TransferRecord> {
        let mut history = self.history.lock().unwrap();
        history.truncate(HISTORY_CAP);
        history.clone()
    }

    /// The transfer history, newest first, capped at 20.
    pub fn take_history(&self) -> Vec<TransferRecord> {
        let mut history = self.history.lock().unwrap();
        history.truncate(HISTORY_CAP);
        std::mem::take(&mut *history)
    }

    /// The device DTO the register/info endpoints answer with, carrying the
    /// actually-bound port once serving.
    pub fn device(&self) -> DeviceInfo {
        let mut device = self.device.clone();
        device.port = self.bound_port.load(Ordering::Relaxed);
        device
    }

    /// The live session's progress as `(files landed, files offered)`, or
    /// `None` when no transfer is in flight. The settings status bar reads
    /// this; it is a lock over one small map, cheap enough to poll at 700 ms.
    pub fn progress(&self) -> Option<(usize, usize)> {
        let sessions = self.sessions.lock().unwrap();
        let session = sessions.values().next()?;
        Some((session.received.len(), session.files.len()))
    }

    /// Bind the first free port in [`PORT_RANGE`], so the receiver coexists
    /// with the LocalSend desktop app (which holds 53317 by default) instead
    /// of failing to start. Senders honour the port our register DTO
    /// announces, so any port in the range is as good as the first.
    ///
    /// Binding is the one step that can fail for a reason the user needs to
    /// hear ("they are all taken"), so it happens on the caller's thread and
    /// the error comes back as an error.
    pub fn bind(&self) -> std::io::Result<TcpListener> {
        let mut last_err = None;
        for port in PORT_RANGE {
            match TcpListener::bind(("0.0.0.0", port)) {
                Ok(listener) => {
                    self.bound_port
                        .store(listener.local_addr()?.port(), Ordering::Relaxed);
                    self.running.store(true, Ordering::Relaxed);
                    return Ok(listener);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    last_err = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::AddrInUse, "no port in 53317-53327 was free")
        }))
    }

    /// Bind one exact port — the form the tests use, where the port must be
    /// predictable and shared with the client side.
    pub fn bind_port(&self, port: u16) -> std::io::Result<TcpListener> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        self.bound_port
            .store(listener.local_addr()?.port(), Ordering::Relaxed);
        self.running.store(true, Ordering::Relaxed);
        Ok(listener)
    }

    /// Serve until `stop`, on an already-bound listener. One thread; one
    /// connection at a time is honest for a receiver — LocalSend sends files
    /// in sequence by default, and a queue here costs nothing the transfer is
    /// not already spending.
    pub fn serve_listener(&self, listener: TcpListener) {
        // Reap expired sessions on the accept loop's clock: a session that
        // timed out must not keep its temp files forever. The thread exits
        // when the receiver stops, so a start/stop/start cycle does not
        // leave a reaper per cycle running forever.
        let sessions = self.sessions.clone();
        let reaper_running = self.running.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(30));
            if !reaper_running.load(Ordering::Relaxed) {
                return;
            }
            let mut sessions = match sessions.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            let expired: Vec<String> = sessions
                .iter()
                .filter(|(_, s)| s.created.elapsed() > SESSION_TIMEOUT)
                .map(|(id, _)| id.clone())
                .collect();
            for id in expired {
                if let Some(session) = sessions.remove(&id) {
                    drop(session); // Drop cleans the temp dir.
                }
            }
        });
        for stream in listener.incoming() {
            if !self.running.load(Ordering::Relaxed) {
                break;
            }
            let Ok(mut stream) = stream else { continue };
            let peer = stream
                .peer_addr()
                .ok()
                .map(|a| a.ip().to_string());
            if let Err(e) = self.handle_connection(&mut stream, peer) {
                tracing::debug!("bookdrop: connection error: {e}");
            }
        }
    }

    /// Bind and serve in one call: the shape the tests use.
    pub fn serve(&self, port: u16) -> std::io::Result<()> {
        let listener = self.bind_port(port)?;
        self.serve_listener(listener);
        Ok(())
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
        // A connect to our own port wakes the accept loop; the flag above
        // makes it exit. Best effort: the loop also exits on the next
        // incoming connection whenever that is.
        let wake_port = self.bound_port.load(Ordering::Relaxed);
        if let Ok(mut wake) = TcpStream::connect(("127.0.0.1", wake_port)) {
            let _ = wake.write_all(b"");
        }
        self.sessions.lock().unwrap().clear();
    }

    /// Read one HTTP request, dispatch it, write one response.
    ///
    /// The body is STREAMED to the session's directory (never buffered in
    /// RAM): the module's own invariant, which an earlier cut violated by
    /// reading up to 2 GiB into a Vec — a LAN peer with one POST and a lying
    /// Content-Length could OOM-kill the app, and with it every unsaved
    /// edit. Only `upload` has a body worth reading; every JSON endpoint's
    /// body is capped at 64 KiB, which a prepare request exceeds by tens of
    /// files at worst.
    fn handle_connection(&self, stream: &mut TcpStream, peer: Option<String>) -> std::io::Result<()> {
        // A peer that opens a socket and sends nothing must not wedge the
        // single-connection accept loop (slowloris).
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut request = match read_request_head(stream) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        let is_upload = request.path.contains("/api/localsend/v2/upload");
        // The JSON endpoints are small; anything larger than 64 KiB there is
        // not a LocalSend request.
        if !is_upload {
            if request.content_length > 64 * 1024 {
                let _ = write_response(stream, &Response::empty(413));
                return Ok(());
            }
            while request.body_stream.len() < request.content_length {
                let mut chunk = [0u8; 8 * 1024];
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => request.body_stream.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            let response = self.route(&request, peer);
            let _ = write_response(stream, &response);
            return Ok(());
        }
        if request.method != "POST" {
            // Only POST carries an upload; a GET naming an upload URL is a
            // probe, and it gets nothing but the method's own answer.
            self.drain_body(stream, request.content_length);
            let _ = write_response(stream, &Response::empty(405));
            return Ok(());
        }
        // The upload: validate the session/token/IP FIRST, from the head,
        // before a byte of body is read — a stranger must not be able to
        // make the receiver pull a gigabyte just by naming an URL.
        let admission = self.admit_upload(&request, peer);
        let Some((session_id, file_id, meta, dir, target)) = admission else {
            // The 403/409/422 the admission computed, with the body drained
            // (bounded) so the peer's pipeline does not wedge on our RST.
            let status = self
                .admission_status
                .lock()
                .unwrap()
                .take()
                .unwrap_or(403);
            self.drain_body(stream, request.content_length);
            let _ = write_response(stream, &Response::empty(status));
            return Ok(());
        };
        let response = self.stream_upload(stream, &request, &session_id, &file_id, &meta, &dir, &target);
        let _ = write_response(stream, &response);
        Ok(())
    }

    /// Drain up to `len` body bytes into a scratch buffer, discarding them,
    /// so a rejected sender's TCP stream stays coherent through the 4xx.
    /// Capped at [`MAX_DRAIN_BYTES`]: a peer this side refused does not get
    /// to keep the single-threaded receiver reading garbage — past the cap
    /// the socket simply closes.
    fn drain_body(&self, stream: &mut TcpStream, len: usize) {
        let mut remaining = len.min(MAX_DRAIN_BYTES);
        let mut chunk = [0u8; 64 * 1024];
        while remaining > 0 {
            let want = remaining.min(chunk.len());
            match stream.read(&mut chunk[..want]) {
                Ok(0) | Err(_) => break,
                Ok(n) => remaining -= n,
            }
        }
    }

    /// The endpoint table. Every branch returns a full HTTP response.
    fn route(&self, request: &Request, peer: Option<String>) -> Response {
        let path = request.path.split('?').next().unwrap_or("");
        match (request.method.as_str(), path) {
            ("POST", "/api/localsend/v2/register") | ("GET", "/api/localsend/v2/info") => {
                // `device()`, not `device`: the bound port is the truth, and
                // with the range bind it is not always 53317. The original
                // form announced the constructed port, so a receiver that
                // fell back to 53318 told senders to upload to 53317 —
                // whoever held THAT port got the files.
                Response::json(200, &self.device())
            }
            ("POST", "/api/localsend/v2/prepare-upload") => {
                self.prepare_upload(request, peer)
            }
            ("POST", "/api/localsend/v2/cancel") => {
                if let Some(id) = request.query("sessionId") {
                    let mut sessions = self.sessions.lock().unwrap();
                    // Only the session's own sender may cancel it: a LAN
                    // peer that guesses the id must not kill a transfer.
                    let owned = sessions
                        .get(id)
                        .map(|s| s.sender_host.is_none() || s.sender_host.as_deref() == peer.as_deref())
                        .unwrap_or(false);
                    if owned {
                        if let Some(session) = sessions.remove(id) {
                            drop(session);
                        }
                    }
                }
                Response::empty(200)
            }
            _ => Response::empty(404),
        }
    }

    fn prepare_upload(&self, request: &Request, peer: Option<String>) -> Response {
        let Ok(payload) = serde_json::from_slice::<PrepareUpload>(&request.body_stream) else {
            return Response::empty(400);
        };
        // The code may arrive in the body (the spec's field) or on the query
        // string (the form the official app's sender uses); either satisfies,
        // like the iOS receiver. An EMPTY body code counts as absent: the
        // iOS sender always writes the field — blank included — and a body
        // of `""` must not shadow a code the same sender put on the query.
        // That exact shape (body `""` + query code) was a real transfer
        // refused with 401 while both devices carried the same code.
        let body_pin = payload.pin.unwrap_or_default();
        let offered = if body_pin.trim().is_empty() {
            request.query("pin").map(str::to_string)
        } else {
            Some(body_pin)
        };
        // A wrong or missing code is 401, ahead of the acceptance switch: the
        // spec's order, and what tells a sender to re-prompt rather than
        // report a refusal. The stall comes from the gate: a peer hammering
        // guesses pays for the attempt before it reads the refusal.
        if !self.pin_ok(offered.as_deref()) {
            let stall = self.note_pin_failure();
            std::thread::sleep(stall);
            return Response::empty(401);
        }
        self.note_pin_success();
        if !self.auto_accept.load(Ordering::Relaxed) {
            return Response::empty(403);
        }
        {
            let sessions = self.sessions.lock().unwrap();
            if !sessions.is_empty() {
                return Response::empty(409); // one transfer at a time
            }
        }
        // Partial accept: omitted ids count as rejected per spec. The budget
        // is part of the same answer — a file past the size, count or total
        // caps is "cannot take it", not "refuse the batch". Sorted by id so
        // the budget spends the same way every time.
        let mut candidates: Vec<(String, FileMeta)> = payload
            .files
            .into_iter()
            .filter(|(_, meta)| {
                let ext = meta
                    .file_name
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                ACCEPTED_EXTENSIONS.contains(&ext.as_str())
            })
            .collect();
        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        let mut budget: u64 = 0;
        let accepted: HashMap<String, FileMeta> = candidates
            .into_iter()
            .filter(|(_, meta)| {
                if meta.size > MAX_FILE_BYTES {
                    return false;
                }
                budget = budget.saturating_add(meta.size);
                budget <= MAX_SESSION_BYTES
            })
            .take(MAX_SESSION_FILES)
            .collect();
        if accepted.is_empty() {
            return Response::empty(403);
        }
        let session_id = new_id();
        let mut tokens: HashMap<String, String> = HashMap::new();
        // The on-disk names are minted HERE, from this side's own ids. The
        // sender's map keys never touch a path again: `files: {"../../x": …}`
        // used to ride `dir.join(name)` straight out of the temp directory.
        let mut disk_names: HashMap<String, String> = HashMap::new();
        for (id, meta) in &accepted {
            tokens.insert(id.clone(), new_id());
            disk_names.insert(
                id.clone(),
                format!("{}-{}", new_disk_id(), safe_file_name(&meta.file_name, id)),
            );
        }
        let dir = std::env::temp_dir().join(format!("BookDrop-{session_id}"));
        // 0700: a received file is the user's private data until it is
        // routed; other local users must not read it out of /tmp.
        if std::fs::create_dir_all(&dir).is_err()
            || std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).is_err()
        {
            return Response::empty(500);
        }
        let token_map = tokens.clone();
        self.sessions.lock().unwrap().insert(
            session_id.clone(),
            Session {
                id: session_id.clone(),
                sender_host: peer,
                dir,
                files: accepted,
                tokens,
                disk_names,
                received: std::collections::HashSet::new(),
                created: std::time::Instant::now(),
            },
        );
        Response::json(200, &serde_json::json!({
            "sessionId": session_id,
            "files": token_map,
        }))
    }

    /// Validate an upload request from its HEAD only: session exists,
    /// sender IP matches, token matches, file known, not a duplicate.
    /// Returns what the streaming writer needs, or None (with
    /// `admission_status` set) when the request is refused.
    fn admit_upload(
        &self,
        request: &Request,
        peer: Option<String>,
    ) -> Option<(String, String, FileMeta, PathBuf, PathBuf)> {
        let session_id = request.query("sessionId").map(str::to_string);
        let file_id = request.query("fileId").map(str::to_string);
        let token = request.query("token").map(str::to_string);
        let pin = request.query("pin").map(str::to_string);
        let (Some(session_id), Some(file_id), Some(token)) = (session_id, file_id, token) else {
            *self.admission_status.lock().unwrap() = Some(400);
            return None;
        };
        // The code is re-checked per upload (a code typed mid-session stops
        // the in-flight one), and auto-accept stays live too. A wrong code
        // on an upload stalls like one on a prepare — the guess-a-thon must
        // not move to the upload URL where no session is needed to try.
        if !self.pin_ok(pin.as_deref()) {
            let stall = self.note_pin_failure();
            std::thread::sleep(stall);
            *self.admission_status.lock().unwrap() = Some(401);
            return None;
        }
        self.note_pin_success();
        if !self.auto_accept.load(Ordering::Relaxed) {
            *self.admission_status.lock().unwrap() = Some(403);
            return None;
        }
        let sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get(&session_id) else {
            *self.admission_status.lock().unwrap() = Some(400);
            return None;
        };
        // A third party must not inject files into someone's session.
        if session.sender_host.is_some() && session.sender_host.as_deref() != peer.as_deref() {
            *self.admission_status.lock().unwrap() = Some(403);
            return None;
        }
        if session.tokens.get(&file_id).map(String::as_str) != Some(token.as_str()) {
            *self.admission_status.lock().unwrap() = Some(403);
            return None;
        }
        if session.received.contains(&file_id) {
            *self.admission_status.lock().unwrap() = Some(409); // duplicate upload
            return None;
        }
        let Some(meta) = session.files.get(&file_id).cloned() else {
            *self.admission_status.lock().unwrap() = Some(400);
            return None;
        };
        // The landing name was minted at prepare from THIS side's ids; the
        // sender's file id never reaches a path.
        let (dir, disk_name) = match (
            session.dir.clone(),
            session.disk_names.get(&file_id).cloned(),
        ) {
            (dir, Some(name)) => (dir, name),
            _ => {
                *self.admission_status.lock().unwrap() = Some(400);
                return None;
            }
        };
        drop(sessions);
        Some((session_id, file_id, meta, dir.clone(), dir.join(disk_name)))
    }

    /// Stream the body to `target` in fixed-size chunks, enforcing
    /// `meta.size` byte-for-byte; verify sha256 when the sender offered one.
    fn stream_upload(
        &self,
        stream: &mut TcpStream,
        request: &Request,
        session_id: &str,
        file_id: &str,
        meta: &FileMeta,
        dir: &PathBuf,
        target: &PathBuf,
    ) -> Response {
        use std::io::Write;
        let mut file = match std::fs::File::create(target) {
            Ok(f) => f,
            Err(_) => return Response::empty(500),
        };
        use sha2::Sha256;
        let mut hasher = Sha256::new();
        let mut written: u64 = 0;
        // Bytes the head read buffered ahead of the body.
        let mut pending: &[u8] = &request.body_stream;
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let block: &[u8] = if !pending.is_empty() {
                pending
            } else {
                match stream.read(&mut chunk) {
                    Ok(0) => &[],
                    Ok(n) => &chunk[..n],
                    Err(_) => &[],
                }
            };
            if block.is_empty() {
                break;
            }
            // Never write past the announced size: an over-shooting sender
            // is refused, not truncated.
            let room = meta.size.saturating_sub(written);
            if room == 0 {
                drop(file);
                let _ = std::fs::remove_file(target);
                return Response::empty(422);
            }
            let take = (block.len() as u64).min(room) as usize;
            if file.write_all(&block[..take]).is_err() {
                let _ = std::fs::remove_file(target);
                return Response::empty(500);
            }
            hasher.update(&block[..take]);
            written += take as u64;
            if !pending.is_empty() {
                pending = &pending[take..];
            }
            if written >= meta.size {
                // Anything the sender still has in flight is surplus; drain
                // a bounded amount so its pipeline does not wedge.
                if !pending.is_empty() {
                    self.drain_body(stream, usize::MAX);
                }
                break;
            }        }
        drop(file);
        if written != meta.size {
            let _ = std::fs::remove_file(target);
            return Response::empty(422);
        }
        if let Some(expected) = meta.sha256.as_deref().filter(|s| !s.is_empty()) {
            let hex: String = hasher
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if hex != expected.to_ascii_lowercase() {
                let _ = std::fs::remove_file(target);
                return Response::empty(422);
            }
        }
        let complete = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some(session) = sessions.get_mut(session_id) else {
                return Response::empty(400);
            };
            session.received.insert(file_id.to_string());
            session.received.len() >= session.files.len()
        };
        if complete {
            // Route every landed file, then end the session. Each landed
            // file keeps the receiver-minted disk name it arrived under; the
            // history shows the sender's own (sanitised) name.
            let landed: Vec<(String, String)> = {
                let mut sessions = self.sessions.lock().unwrap();
                let Some(mut session) = sessions.remove(session_id) else {
                    return Response::empty(200);
                };
                let items = session
                    .files
                    .iter()
                    .filter(|(id, _)| session.received.contains(*id))
                    .filter_map(|(id, f)| {
                        let disk = session.disk_names.get(id).cloned()?;
                        let shown = safe_file_name(&f.file_name, id);
                        Some((disk, shown))
                    })
                    .collect();
                // Defuse Session::drop's cleanup: the router below consumes
                // these files first.
                session.dir = std::env::temp_dir().join(format!("BookDrop-gone-{session_id}"));
                items
            };
            let router_guard = self.router.lock().unwrap();
            for (disk, shown) in &landed {
                let path = dir.join(disk);
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                let outcome = match router_guard.as_ref() {
                    Some(router) => match router(&path) {
                        Ok(where_) => TransferOutcome::Imported(where_),
                        Err(why) => TransferOutcome::Failed(why),
                    },
                    None => TransferOutcome::Rejected,
                };
                let mut history = self.history.lock().unwrap();
                history.insert(
                    0,
                    TransferRecord {
                        name: shown.clone(),
                        size,
                        outcome,
                        at: std::time::SystemTime::now(),
                    },
                );
                history.truncate(HISTORY_CAP);
            }
            drop(router_guard);
            let _ = std::fs::remove_dir_all(dir);
        }
        Response::empty(200)
    }
}

/// The HTTP request as read off the wire: head plus whatever body bytes had
/// already arrived.
struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    content_length: usize,
    body_stream: Vec<u8>,
}

impl Request {
    fn query(&self, key: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Read the request head (and any body bytes already buffered).
///
/// Two clocks bound a hostile opener: the head may be at most 16 KiB (a
/// LocalSend head is under one), and the whole head must arrive within
/// 20 seconds of the connection. The socket's own 30 s read timeout only
/// bounds ONE stalled read — a sender dribbling a byte every 25 s used to
/// hold the single-connection loop for hours.
fn read_request_head(stream: &mut TcpStream) -> std::io::Result<Request> {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut buffer: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    // Read until the head terminator.
    let head_end = loop {
        let window = find_double_crlf(&buffer);
        if let Some(end) = window {
            break end;
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "request ended before head",
            ));
        }
        buffer.extend_from_slice(&chunk[..n]);
        if buffer.len() > 16 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head too large",
            ));
        }
        if std::time::Instant::now() > deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "request head took too long",
            ));
        }
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut request_line = lines.next().unwrap_or("").split_whitespace();
    let method = request_line.next().unwrap_or("").to_string();
    let target = request_line.next().unwrap_or("").to_string();
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        if name.trim().eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let (path, query) = parse_target(&target);
    let body_stream = buffer[head_end + 4..].to_vec();
    Ok(Request {
        method,
        path,
        query,
        content_length,
        body_stream,
    })
}

/// The `\r\n\r\n` position in a request buffer.
fn find_double_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Split `/path?key=value&key2=v2` into its parts.
fn parse_target(target: &str) -> (String, Vec<(String, String)>) {
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => return (target.to_string(), Vec::new()),
    };
    let pairs = query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((url_decode(k), url_decode(v)))
        })
        .collect();
    (path, pairs)
}

/// Percent-decode a query component. `+` is a space in query strings.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// An HTTP response under construction.
struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Response {
    fn empty(status: u16) -> Self {
        Self {
            status,
            content_type: "text/plain",
            body: Vec::new(),
        }
    }

    fn json<T: serde::Serialize>(status: u16, value: &T) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: serde_json::to_vec(value).unwrap_or_default(),
        }
    }
}

/// The reason phrases the protocol's error codes deserve.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

fn write_response(stream: &mut TcpStream, response: &Response) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        reason(response.status),
        response.content_type,
        response.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

/// A landed file's name on disk: the sender's last path component with the
/// dangerous characters out, or `file-<id>` when nothing readable survives.
fn safe_file_name(name: &str, fallback: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().filter(|c| *c != '\0').collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return format!("file-{fallback}");
    }
    cleaned
}

/// A fresh session/file id.
fn new_id() -> String {
    sn_core::util::new_uuid()
}

/// The id half of every on-disk name: 32 hex characters, no dashes, so the
/// UI's display-name strip (`id.len() >= 32`) recognises it, and so the
/// name can never be mistaken for a relative path.
fn new_disk_id() -> String {
    sn_core::util::new_uuid()
        .chars()
        .filter(|c| *c != '-')
        .collect()
}

/// The stall for the `n`-th consecutive wrong code. Free for the first
/// [`PIN_GRACE_FAILURES`] (a human retyping), then a doubling climb from
/// half a second to [`PIN_MAX_STALL`].
fn pin_stall_for(fails: u32) -> Duration {
    if fails <= PIN_GRACE_FAILURES {
        return Duration::ZERO;
    }
    let steps = (fails - PIN_GRACE_FAILURES - 1).min(6);
    Duration::from_millis(500)
        .saturating_mul(1u32 << steps)
        .min(PIN_MAX_STALL)
}

/// This device's LocalSend name: the hostname is what a phone on the same
/// Wi-Fi already calls the machine. Shared by the receiver's DTO and the
/// sender's.
pub fn device_alias() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Speechnotes".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- the pentest probes -------------------------------------------------
    // Every probe here is a real attack against a live receiver over a real
    // socket: the response codes and the filesystem are the evidence.

    /// One raw HTTP/1.1 POST, the way an attacker writes one.
    fn raw_post(port: u16, path: &str, body: &[u8]) -> (u16, String) {
        use std::io::Read;
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: attacker\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let status = raw
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, raw)
    }

    /// The body half of a response (after the blank line).
    fn body_of(raw: &str) -> &str {
        raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("")
    }

    /// A receiver on an OS-chosen port whose router records full paths.
    fn armed_receiver() -> (
        std::sync::Arc<Receiver>,
        u16,
        std::sync::Arc<Mutex<Vec<PathBuf>>>,
    ) {
        let receiver = std::sync::Arc::new(Receiver::new("Box".into(), "box-fp".into(), 0));
        let landed: std::sync::Arc<Mutex<Vec<PathBuf>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));
        {
            let landed = landed.clone();
            receiver.set_router(Box::new(move |path| {
                landed.lock().unwrap().push(path.to_path_buf());
                Ok("library".to_string())
            }));
        }
        let listener = receiver.bind_port(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = receiver.clone();
        std::thread::spawn(move || server.serve_listener(listener));
        (receiver, port, landed)
    }

    /// THE P0: the prepare body's file-map keys are sender-chosen strings.
    /// A key like `../../../pwned` used to flow straight into the landing
    /// path — an arbitrary file write as this user, no code needed. The
    /// on-disk name must be the receiver's own, and nothing may land
    /// outside the session's temp directory.
    #[test]
    fn a_hostile_file_id_cannot_escape_the_session_dir() {
        let (receiver, port, landed) = armed_receiver();
        let key = "../../../pwned";
        let body = serde_json::json!({
            "info": receiver.device(),
            "files": { key: {"id": key, "fileName": "note.md", "size": 5,
                              "fileType": "text/markdown", "sha256": null} },
        });
        let (status, raw) = raw_post(
            port,
            "/api/localsend/v2/prepare-upload",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(status, 200, "{raw}");
        let parsed: serde_json::Value = serde_json::from_str(body_of(&raw)).unwrap();
        let session = parsed["sessionId"].as_str().unwrap().to_string();
        let token = parsed["files"][key].as_str().unwrap().to_string();
        let upload = format!("/api/localsend/v2/upload?sessionId={session}&fileId=../../../pwned&token={token}");
        let (status, raw) = raw_post(port, &upload, b"pwned");
        assert_eq!(status, 200, "{raw}");

        let paths = landed.lock().unwrap().clone();
        assert_eq!(paths.len(), 1, "the file routed once");
        let path = &paths[0];
        assert!(
            path.components()
                .all(|c| !matches!(c, std::path::Component::ParentDir)),
            "a parent component survived: {}",
            path.display()
        );
        assert!(
            path.to_string_lossy().contains("BookDrop-"),
            "the file must land inside the session dir: {}",
            path.display()
        );
        // And the classic targets of the old bug are untouched: the
        // pre-patch landing path resolved to `/pwned-note.md`.
        for escaped in [
            std::path::Path::new("/pwned-note.md"),
            std::path::Path::new("/tmp/pwned-note.md"),
            &std::env::temp_dir().join("pwned-note.md"),
        ] {
            assert!(!escaped.exists(), "{} escaped", escaped.display());
        }
    }

    /// The same-name collision the uuid prefix exists for, at wire level:
    /// two files named note.md in one session must both land, separately.
    #[test]
    fn two_same_named_files_in_one_session_both_land() {
        let (receiver, port, landed) = armed_receiver();
        let body = serde_json::json!({
            "info": receiver.device(),
            "files": {
                "a": {"id": "a", "fileName": "note.md", "size": 3, "sha256": null},
                "b": {"id": "b", "fileName": "note.md", "size": 3, "sha256": null},
            },
        });
        let (status, raw) = raw_post(
            port,
            "/api/localsend/v2/prepare-upload",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(status, 200, "{raw}");
        let parsed: serde_json::Value = serde_json::from_str(body_of(&raw)).unwrap();
        let session = parsed["sessionId"].as_str().unwrap().to_string();
        for key in ["a", "b"] {
            let token = parsed["files"][key].as_str().unwrap().to_string();
            let upload =
                format!("/api/localsend/v2/upload?sessionId={session}&fileId={key}&token={token}");
            let (status, raw) = raw_post(port, &upload, b"abc");
            assert_eq!(status, 200, "{key}: {raw}");
        }
        let paths = landed.lock().unwrap().clone();
        assert_eq!(paths.len(), 2, "both files routed");
        assert_ne!(paths[0], paths[1], "they are distinct files");
    }

    /// A prepare that promises a file past the size cap, or a session past
    /// the total cap, is answered by partial accept: the liar's files are
    /// simply not in the token map.
    #[test]
    fn a_prepare_cannot_promise_a_terabyte() {
        let (receiver, port, _landed) = armed_receiver();
        let giB: u64 = 1024 * 1024 * 1024;
        let body = serde_json::json!({
            "info": receiver.device(),
            "files": {
                "huge": {"id": "huge", "fileName": "book.epub", "size": 8 * giB, "sha256": null},
            },
        });
        let (status, raw) = raw_post(
            port,
            "/api/localsend/v2/prepare-upload",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(status, 403, "one over-cap file leaves nothing to accept: {raw}");

        // Under the per-file cap but over the session total: the first
        // files fit, the one that breaks the budget is dropped.
        let body = serde_json::json!({
            "info": receiver.device(),
            "files": {
                "a": {"id": "a", "fileName": "a.epub", "size": 3 * giB / 2, "sha256": null},
                "b": {"id": "b", "fileName": "b.epub", "size": 3 * giB / 2, "sha256": null},
                "c": {"id": "c", "fileName": "c.epub", "size": 3 * giB, "sha256": null},
            },
        });
        let (status, raw) = raw_post(
            port,
            "/api/localsend/v2/prepare-upload",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(status, 200, "{raw}");
        let parsed: serde_json::Value = serde_json::from_str(body_of(&raw)).unwrap();
        let files = parsed["files"].as_object().unwrap();
        assert!(files.contains_key("a") && files.contains_key("b"), "{files:?}");
        assert!(!files.contains_key("c"), "the budget-breaker is dropped: {files:?}");
    }

    /// Wrong codes get expensive: three are free, then the climb. Six wrong
    /// prepares must cost at least three seconds — at pre-patch LAN speed
    /// those six cost microseconds and the next ten thousand did too.
    #[test]
    fn wrong_codes_get_expensive() {
        // The shape of the climb, purely.
        assert_eq!(pin_stall_for(1), Duration::ZERO);
        assert_eq!(pin_stall_for(3), Duration::ZERO);
        assert_eq!(pin_stall_for(4), Duration::from_millis(500));
        assert_eq!(pin_stall_for(5), Duration::from_secs(1));
        assert_eq!(pin_stall_for(9), Duration::from_secs(16));
        assert_eq!(pin_stall_for(10), Duration::from_secs(30), "capped");
        assert_eq!(pin_stall_for(99), Duration::from_secs(30), "still capped");

        // End-to-end on the wire.
        let (receiver, port, _landed) = armed_receiver();
        receiver.set_pin(Some("9999".into()));
        let body = serde_json::json!({ "info": receiver.device(), "files": {} });
        let start = std::time::Instant::now();
        for attempt in 1..=6 {
            let (status, raw) = raw_post(
                port,
                "/api/localsend/v2/prepare-upload?pin=0000",
                &serde_json::to_vec(&body).unwrap(),
            );
            assert_eq!(status, 401, "attempt {attempt}: {raw}");
        }
        assert!(
            start.elapsed() >= Duration::from_secs(3),
            "six wrong codes took only {:?}",
            start.elapsed()
        );
        // A correct code resets the climb: the next wrong one is free again.
        let (status, _) = raw_post(
            port,
            "/api/localsend/v2/prepare-upload?pin=9999",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(status, 403, "code accepted; the empty offer is the refusal");
        let before = std::time::Instant::now();
        let (status, _) = raw_post(
            port,
            "/api/localsend/v2/prepare-upload?pin=0000",
            &serde_json::to_vec(&body).unwrap(),
        );
        assert_eq!(status, 401);
        assert!(
            before.elapsed() < Duration::from_secs(1),
            "the climb did not reset after a success"
        );
    }

    /// Uploads are POST-only; a GET naming an upload URL learns the method.
    #[test]
    fn an_upload_url_refuses_get() {
        use std::io::Read;
        let (_receiver, port, _landed) = armed_receiver();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"GET /api/localsend/v2/upload?sessionId=x&fileId=y&token=z HTTP/1.1\r\nHost: a\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        assert!(raw.contains("405"), "{raw}");
    }

    /// A request head past the 16 KiB cap is cut off without an answer —
    /// the connection is simply closed.
    #[test]
    fn an_oversized_head_is_cut_off() {
        use std::io::Read;
        let (_receiver, port, _landed) = armed_receiver();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let head = format!("GET / HTTP/1.1\r\nHost: a\r\nX-Filler: {}\r\n", "A".repeat(20 * 1024));
        stream.write_all(head.as_bytes()).unwrap();
        let mut raw = String::new();
        let _ = stream.read_to_string(&mut raw);
        assert!(!raw.contains("200"), "no HTTP answer for a hostile head");
    }

    /// `stop` is the off-switch the settings toggle owns: after it returns,
    /// the port must refuse connections — the listener thread wakes on the
    /// self-connect and the socket goes away with it.
    #[test]
    fn stop_actually_closes_the_port() {
        let (receiver, port, _landed) = armed_receiver();
        assert!(
            std::net::TcpStream::connect(("127.0.0.1", port)).is_ok(),
            "while serving, the port answers"
        );
        receiver.stop();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the port still answers after stop()"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn a_safe_name_survives_hostile_senders() {
        assert_eq!(safe_file_name("book.epub", "X"), "book.epub");
        assert_eq!(safe_file_name("a/b/c.epub", "X"), "c.epub");
        assert_eq!(safe_file_name("a\\b\\c.epub", "X"), "c.epub");
        assert_eq!(safe_file_name("..", "X"), "file-X");
        assert_eq!(safe_file_name("", "X"), "file-X");
        assert_eq!(safe_file_name("with\0nul.epub", "X"), "withnul.epub");
    }

    #[test]
    fn query_strings_decode_percent_escapes_and_plus() {
        let (path, query) = parse_target("/api/localsend/v2/upload?sessionId=a%20b&fileId=c+d&token=e%2Ff");
        assert_eq!(path, "/api/localsend/v2/upload");
        assert_eq!(
            query,
            vec![
                ("sessionId".to_string(), "a b".to_string()),
                ("fileId".to_string(), "c d".to_string()),
                ("token".to_string(), "e/f".to_string()),
            ]
        );
        assert_eq!(Request {
            method: "GET".into(),
            path: path.clone(),
            query: query.clone(),
            content_length: 0,
            body_stream: Vec::new(),
        }
        .query("sessionId"), Some("a b"));
    }

    #[test]
    fn the_device_dto_carries_every_field_the_wire_needs() {
        let receiver = Receiver::new("Box".into(), "fp".into(), DEFAULT_PORT);
        let json = serde_json::to_string(&receiver.device()).unwrap();
        for field in [
            "alias", "version", "deviceModel", "deviceType", "fingerprint",
            "port", "protocol", "download",
        ] {
            assert!(json.contains(&format!("\"{field}\"")), "missing {field}: {json}");
        }
        assert!(json.contains("\"deviceType\""), "{json}");
    }

    #[test]
    fn a_prepare_body_with_a_hostile_extension_is_refused_wholesale() {
        // prepare-upload refusal happens before any session exists; test the
        // extension filter directly through the accepted set.
        let ext = "exe";
        assert!(!ACCEPTED_EXTENSIONS.contains(&ext));
        assert!(ACCEPTED_EXTENSIONS.contains(&"epub"));
        assert!(ACCEPTED_EXTENSIONS.contains(&"jex"));
        assert!(ACCEPTED_EXTENSIONS.contains(&"md"), "notes travel as markdown");
    }

    #[test]
    fn the_access_code_decides_admission() {
        let receiver = Receiver::new("Box".into(), "fp".into(), DEFAULT_PORT);
        assert!(receiver.pin_ok(None), "no code set: anything goes");
        assert!(receiver.pin_ok(Some("anything")));
        receiver.set_pin(Some("1234".into()));
        assert!(!receiver.pin_ok(None), "missing code is refused");
        assert!(!receiver.pin_ok(Some("0000")), "wrong code is refused");
        assert!(receiver.pin_ok(Some("1234")));
        receiver.set_pin(Some(String::new()));
        assert!(receiver.pin_ok(None), "clearing the code reopens the door");
        receiver.set_pin(None);
        assert!(receiver.pin_ok(Some("stale")), "None clears too");
    }

    #[test]
    fn the_history_is_capped_and_newest_first() {
        let receiver = Receiver::new("Box".into(), "fp".into(), DEFAULT_PORT);
        for i in 0..25 {
            receiver.history.lock().unwrap().insert(
                0,
                TransferRecord {
                    name: format!("n{i}"),
                    size: 0,
                    outcome: TransferOutcome::Imported("library".into()),
                    at: std::time::SystemTime::now(),
                },
            );
        }
        let history = receiver.take_history();
        assert_eq!(history.len(), 20, "capped");
        assert_eq!(history[0].name, "n24", "newest first");
        assert!(receiver.take_history().is_empty(), "take drains");
    }

    #[test]
    fn the_access_code_trims_like_ios_and_accepts_the_query_form() {
        let receiver = Receiver::new("Box".into(), "fp".into(), DEFAULT_PORT);
        // A typed code with a stray space must not lock peers out (iOS trims
        // both sides; so does this).
        receiver.set_pin(Some("1234 ".into()));
        assert!(receiver.pin_ok(Some("1234")), "trimmed comparison");
        assert!(!receiver.pin_ok(Some("1234 x")), "a different code is refused");
        // The prepare endpoint accepts the code from the query string too —
        // the form the official LocalSend sender uses.
        let request = Request {
            method: "POST".into(),
            path: "/api/localsend/v2/prepare-upload?pin=1234".into(),
            query: vec![("pin".to_string(), "1234".to_string())],
            content_length: 0,
            body_stream: serde_json::to_vec(&serde_json::json!({
                "info": receiver.device(),
                "files": {"f1": {"id": "f1", "fileName": "book.epub", "size": 4,
                                  "fileType": "", "sha256": null}},
            }))
            .unwrap(),
        };
        receiver.auto_accept.store(false, Ordering::Relaxed);
        let response = receiver.prepare_upload(&request, Some("127.0.0.1".into()));
        // 403 = the pin PASSED and the acceptance switch refused — not 401,
        // which would claim the code was wrong.
        assert_eq!(response.status, 403);
    }

    #[test]
    fn a_file_meta_without_a_file_type_decodes() {
        // The iOS sender omits `fileType` for files the share sheet has no
        // category for; a required field there turned that into a 400.
        let json = r#"{"info":{"alias":"a","version":"2.2","deviceType":"mobile",
            "fingerprint":"f","port":0,"protocol":"http","download":false},
            "files":{"f1":{"id":"f1","fileName":"note.md","size":4,"sha256":null}}}"#;
        assert!(serde_json::from_str::<PrepareUpload>(json).is_ok());
    }


    #[test]
    fn register_answers_with_the_port_actually_bound() {
        let receiver = Receiver::new("Box".into(), "fp".into(), DEFAULT_PORT);
        let listener = receiver.bind_port(0).unwrap();
        let bound = listener.local_addr().unwrap().port();
        let request = Request {
            method: "POST".into(),
            path: "/api/localsend/v2/register".into(),
            query: vec![],
            content_length: 0,
            body_stream: Vec::new(),
        };
        let response = receiver.route(&request, None);
        let dto: DeviceInfo = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(dto.port, bound, "the DTO must announce the bound port, not the default");
    }

    #[test]
    fn binding_takes_the_first_free_port_in_the_range() {
        // This test runs on boxes where a live speechnotes (or LocalSend)
        // already holds 53317, so no single port can be assumed free.
        // Hold everything in the range we CAN hold, prove a full range
        // fails cleanly, then free exactly one port and require the
        // receiver to land on it — skipping every taken port below,
        // 53317 included when something else owns it.
        let mut held = Vec::new();
        for port in PORT_RANGE {
            if let Ok(listener) = std::net::TcpListener::bind(("0.0.0.0", port)) {
                held.push((port, listener));
            }
        }
        assert!(!held.is_empty(), "every port in the range is taken by others");
        let receiver = Receiver::new("Box".into(), "fp".into(), DEFAULT_PORT);
        assert!(
            receiver.bind().is_err(),
            "a full range must fail, not panic or wrap around"
        );

        let (port, listener) = held.remove(held.len() / 2);
        drop(listener);
        let bound = receiver.bind().expect("binds on the one freed port");
        assert_eq!(receiver.device().port, port);
        drop(bound);
    }
}
