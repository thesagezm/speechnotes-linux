//! Logging: a rotating file, stderr, and a 300-line ring the UI reads.
//!
//! The Swift port wrote `speechnotes.log` and also mirrored every line to
//! stderr, capped the file at 2 MiB keeping the last 1 MiB, and kept 300 lines
//! in memory for the Logs pane. All three are useful and the format is
//! documented in `DATA-FORMAT.md`, so the format stays; the implementation is
//! `tracing` with a bounded appender plus an in-process ring.

use std::sync::{Arc, Mutex};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::paths;

/// Lines kept in memory for the Logs pane.
pub const RING_CAPACITY: usize = 300;

/// A bounded, thread-safe line buffer.
#[derive(Clone)]
pub struct LogRing {
    inner: Arc<Mutex<Vec<String>>>,
}

impl LogRing {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::with_capacity(RING_CAPACITY))),
        }
    }

    /// Most recent line last.
    pub fn recent(&self, n: usize) -> Vec<String> {
        let Ok(guard) = self.inner.lock() else { return Vec::new() };
        let take = n.min(guard.len());
        guard[guard.len() - take..].to_vec()
    }

    pub fn push(&self, line: String) {
        let Ok(mut guard) = self.inner.lock() else { return };
        guard.push(line);
        if guard.len() > RING_CAPACITY {
            let excess = guard.len() - RING_CAPACITY;
            guard.drain(..excess);
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for LogRing {
    fn default() -> Self {
        Self::new()
    }
}

/// A `tracing` layer that appends formatted lines to a ring buffer.
struct RingLayer {
    ring: LogRing,
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        } else if !self.0.is_empty() {
            self.0.push(' ');
            self.0.push_str(field.name());
            self.0.push('=');
            self.0.push_str(&format!("{value:?}"));
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for RingLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        self.ring
            .push(visitor.0.clone());
        //  the original cuts mid-`format!` in `RingLayer::on_event`;
        // the remainder of this file is authored from the module's own
        // doc comment (log file + stderr mirror + the ring) and the
        // call-site contract (`sn_core::LogRing::new()` +
        // `sn_core::logging::init(ring)` in bin/main.rs and sn-ui
        // window.rs run_entry). The shipped binary pins two more
        // behaviours: an unset RUST_LOG still logs at info (the live
        // speechnotes.log carries fmt lines), and the lines reach the
        // file (selftest "logging reaches the ring and the file") —
        // so the filter defaults to INFO and a file-writing fmt layer
        // is added; try_init keeps the double install (bin/main.rs
        // and window.rs run_entry both call this) a no-op, not a
        // panic. No explicit `_ctx.event` forward here: `Layered`
        // dispatches to the inner subscriber itself, and a manual
        // forward on top of that prints every stderr mirror twice.
    }

    fn enabled(&self, meta: &tracing::Metadata<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) -> bool {
        meta.level() <= &tracing::Level::DEBUG
    }
}

    /// Install the tracing subscriber: the ring, a rotating log file at
    /// `paths::log_file()` capped like the Swift port's (2 MiB, keep the last
    /// 1 MiB), and a mirror on stderr when a display is attached.
    pub fn init(ring: LogRing) {
        // The user's knob is RUST_LOG, but the app ships with it unset and an
        // empty filter enables nothing — the default is the Swift port's:
        // info and up.
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        // The file mirror: the same fmt format without ANSI escapes, through
        // the original `CappedLog` — one speechnotes.log that trims itself to
        // 2 MiB (keeping the last 1 MiB) instead of growing forever. A failed
        // open costs the log, not the app (SharedLog falls back to /dev/null).
        // Fixed 2026-10-30: an earlier tail opened a plain append
        // file here, which grew unbounded; this doc comment's own cap promise
        // and logcap.rs's MakeWriter say the writer is SharedLog.
        let file_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(crate::logcap::SharedLog::open(&paths::log_file()));
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .finish()
            .with(RingLayer { ring })
            .with(file_layer)
            .try_init()
            .ok();
    }
