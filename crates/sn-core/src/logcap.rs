//! A log file that keeps itself under a size cap.
//!
//! The Swift port capped `speechnotes.log` at 2 MiB and kept the last 1 MiB
//! when it rolled. `tracing_appender`'s rolling appender rotates by date, which
//! produces a new filename per day — not what this app's format documents, and
//! not what a user looking for one file expects. So the cap is implemented here:
//! one file, truncated in place once it grows past the ceiling.
//!
//! The truncation happens *before* the overflowing write, and it keeps the tail
//! — a log's most valuable lines are the most recent ones.

use tracing_subscriber::fmt::MakeWriter;

use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Bytes written before the file is trimmed.
pub const CAP_BYTES: u64 = 2 * 1024 * 1024;
/// Bytes kept when trimming.
pub const KEEP_BYTES: u64 = CAP_BYTES / 2;

/// A `Write` adapter that trims the underlying file when it exceeds the cap.
pub struct CappedLog {
    file: File,
    path: PathBuf,
    written: u64,
    cap: u64,
    keep: u64,
}

impl CappedLog {
    /// Open (creating if needed) `path`, trimming it first if it is already over
    /// the cap from a previous run.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut this = Self {
            file: OpenOptions::new().create(true).append(true).open(path)?,
            path: path.to_path_buf(),
            written: 0,
            cap: CAP_BYTES,
            keep: KEEP_BYTES,
        };
        this.trim_if_over()?;
        Ok(this)
    }

    /// A cap for tests, so the behaviour is checkable without writing 2 MiB.
    pub fn with_cap(path: &Path, cap: u64) -> io::Result<Self> {
        let mut this = Self {
            file: OpenOptions::new().create(true).append(true).open(path)?,
            path: path.to_path_buf(),
            written: 0,
            cap,
            keep: cap / 2,
        };
        this.trim_if_over()?;
        Ok(this)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes_written(&self) -> u64 {
        self.written
    }

    fn trim_if_over(&mut self) -> io::Result<()> {
        let len = self.file.metadata()?.len();
        if len > self.cap {
            self.trim()?;
        }
        self.written = self.file.metadata()?.len();
        Ok(())
    }

    /// Drop everything but the last `keep` bytes, in place.
    fn trim(&mut self) -> io::Result<()> {
        let len = self.file.metadata()?.len();
        if len <= self.keep {
            return Ok(());
        }
        let bytes = std::fs::read(&self.path)?;
        let tail = &bytes[len as usize - self.keep as usize..];
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(tail)?;
        self.file.flush()?;
        self.written = self.keep;
        Ok(())
    }
}

/// A `Write` handle shared across threads, for `tracing`'s blocking writer.
///
/// `tracing` emits from whichever thread produced the event, and the file handle
/// is shared, so every write takes the lock. The adapter holds the lock for the
/// lifetime of one write rather than per `write` call, which keeps a log line
/// from being split by an interleaving thread.
#[derive(Clone, Default)]
pub struct SharedLog {
    inner: std::sync::Arc<std::sync::Mutex<Option<CappedLog>>>,
}

impl SharedLog {
    /// Open `path` and wrap it. Falls back to `/dev/null` when the log file
    /// cannot be opened: a storage problem should cost the log, not the app.
    pub fn open(path: &Path) -> Self {
        let inner = CappedLog::open(path)
            .or_else(|e| {
                tracing::warn!(error = %e, "log file unavailable; logging to /dev/null");
                CappedLog::open(std::path::Path::new("/dev/null"))
            })
            .ok();
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(inner)),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut CappedLog) -> R) -> Option<R> {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            // A poisoned lock means a write panicked; the file is still usable.
            Err(p) => p.into_inner(),
        };
        guard.as_mut().map(f)
    }

    /// Append a complete buffer in one locked write.
    pub fn commit(&self, bytes: &[u8]) -> io::Result<()> {
        self.with(|l| l.write_all(bytes).and_then(|()| l.flush()))
            .ok_or_else(|| io::Error::other("log file closed"))?
    }

    pub fn bytes_written(&self) -> u64 {
        self.with(|l| l.bytes_written()).unwrap_or(0)
    }
}

/// The `Write` half of [`SharedLog`]: buffers, then commits under one lock.
///
/// The buffer is the point. `write!` and `writeln!` reach `Write` as a *sequence*
/// of small writes — a field, a literal, a field — and if each took the lock
/// separately, two threads logging at once would interleave mid-line and produce
/// a log full of spliced records. Buffering until `flush` makes one committed
/// call per line, which is the only way the file stays readable.
///
/// The cost is that a hard crash can lose the last unflushed line. `tracing`'s
/// formatter flushes at the end of every event, and `Drop` flushes here too, so
/// in practice the window is a single line.
pub struct SharedLogWriter<'a> {
    log: &'a SharedLog,
    buffer: Vec<u8>,
}

impl<'a> MakeWriter<'a> for SharedLog {
    type Writer = SharedLogWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        SharedLogWriter {
            log: self,
            buffer: Vec::with_capacity(256),
        }
    }
}

impl Write for SharedLogWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let bytes = std::mem::take(&mut self.buffer);
        self.log.commit(&bytes)
    }
}

impl Drop for SharedLogWriter<'_> {
    fn drop(&mut self) {
        if let Err(e) = self.flush() {
            tracing::debug!(error = %e, "log flush failed on drop");
        }
    }
}

/// `tracing_appender::non_blocking` wraps a plain `Write`, so `SharedLog` has to
/// be one too. It writes under the same lock as the `MakeWriter` path, so the
/// blocking and non-blocking configurations cannot interleave a line.
impl Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner
            .lock()
            .map_err(|_| io::Error::other("log lock poisoned"))?
            .as_mut()
            .ok_or_else(|| io::Error::other("log file closed"))?
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner
            .lock()
            .map_err(|_| io::Error::other("log lock poisoned"))?
            .as_mut()
            .ok_or_else(|| io::Error::other("log file closed"))?
            .flush()
    }
}

impl Write for CappedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written + buf.len() as u64 > self.cap {
            self.trim()?;
        }
        let n = self.file.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sn-logcap-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("speechnotes.log")
    }

    #[test]
    fn writes_go_to_the_file() {
        let p = tmp("write");
        let mut w = CappedLog::open(&p).unwrap();
        w.write_all(b"hello\n").unwrap();
        w.flush().unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"hello\n");
    }

    #[test]
    fn appends_rather_than_truncates_on_open() {
        let p = tmp("append");
        std::fs::write(&p, b"first\n").unwrap();
        let mut w = CappedLog::open(&p).unwrap();
        w.write_all(b"second\n").unwrap();
        w.flush().unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"first\nsecond\n");
    }

    #[test]
    fn trims_once_the_cap_is_exceeded_keeping_the_tail() {
        let p = tmp("trim");
        let mut w = CappedLog::with_cap(&p, 100).unwrap();
        for i in 0..40 {
            writeln!(w, "line {i:03}").unwrap();
        }
        w.flush().unwrap();

        let bytes = std::fs::read(&p).unwrap();
        assert!(
            bytes.len() <= 100,
            "file grew to {} bytes, cap is 100",
            bytes.len()
        );
        // The newest line survives; the oldest is gone.
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("line 039"), "tail was discarded: {text:?}");
        assert!(!text.contains("line 000"), "head was kept: {text:?}");
    }

    #[test]
    fn trims_on_open_when_a_previous_run_left_an_oversized_file() {
        let p = tmp("oversized");
        let mut body = String::new();
        for i in 0..500 {
            body.push_str(&format!("old line {i:03}\n"));
        }
        std::fs::write(&p, body.as_bytes()).unwrap();

        let mut w = CappedLog::with_cap(&p, 200).unwrap();
        w.write_all(b"new\n").unwrap();
        w.flush().unwrap();

        let bytes = std::fs::read(&p).unwrap();
        assert!(bytes.len() <= 200, "file is {} bytes", bytes.len());
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.ends_with("new\n"), "the newest write was lost: {text:?}");
    }

    #[test]
    fn the_shared_writer_serialises_concurrent_lines() {
        let p = tmp("make-writer");
        let log = SharedLog::open(&p);
        let mut handles = Vec::new();
        for t in 0..4 {
            let log = log.clone();
            handles.push(std::thread::spawn(move || {
                let mut w = log.make_writer();
                for i in 0..50 {
                    writeln!(w, "t{t}-{i:03}").unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.lines().count(), 200, "a line was lost or corrupted");
        for t in 0..4 {
            for i in 0..50 {
                assert!(
                    text.contains(&format!("t{t}-{i:03}")),
                    "missing t{t}-{i:03}"
                );
            }
        }
    }

    #[test]
    fn a_survives_a_panicking_write() {
        // A poisoned mutex must not silence the log for the rest of the process.
        let p = tmp("poison");
        let log = SharedLog::open(&p);
        let log2 = log.clone();
        let _ = std::thread::spawn(move || {
            let mut w = log2.make_writer();
            let _ = w.write_all(b"before the panic
");
            panic!("simulated");
        })
        .join();

        let mut w = log.make_writer();
        w.write_all(b"after the panic
").unwrap();
        w.flush().unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("before the panic"));
        assert!(text.contains("after the panic"), "log died with the panic");
    }

    #[test]
    fn keeps_writing_after_a_trim() {
        let p = tmp("post-trim");
        let mut w = CappedLog::with_cap(&p, 60).unwrap();
        for i in 0..20 {
            writeln!(w, "line {i:02}").unwrap();
        }
        w.flush().unwrap();
        let len = std::fs::metadata(&p).unwrap().len();
        assert!(len <= 60, "file is {len} bytes");
        let raw = std::fs::read(&p).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("line 19"));
    }
}