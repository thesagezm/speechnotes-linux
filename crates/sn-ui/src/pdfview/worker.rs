//! The PDF render worker: `pdftoppm` draws one page per job on a thread,
//! results come back as file paths. Split out of `pdfview.rs` (Round 4,
//! pure code-motion).

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

pub(crate) struct Job {
    pub(crate) pdf: PathBuf,
    /// 1-based, the numbering `pdftoppm` uses.
    pub(crate) page: usize,
    pub(crate) dpi: u32,
    /// The cache file the finished PNG is moved to.
    pub(crate) out: PathBuf,
    pub(crate) epoch: u64,
}

/// The worker's answer: the page's PNG, or the reason there is none.
#[derive(Debug)]
pub(crate) struct Done {
    pub(crate) page: usize,
    pub(crate) path: Option<PathBuf>,
    pub(crate) epoch: u64,
    pub(crate) error: Option<String>,
}

/// A page's row: the box that reserves its space and the picture that draws
/// it. The box is what the layout measures; the picture is what the user
/// sees.
/// Spawn the render thread. One per view.
///
/// The worker draws into a temporary file and moves it into the cache, so a
/// reader that scrolls back to a page mid-render never sees a half-written
/// PNG.
pub(crate) fn spawn_worker(jobs: Receiver<Job>, done: Sender<Done>) {
    std::thread::spawn(move || {
        while let Ok(job) = jobs.recv() {
            if job.out.exists() {
                let _ = done.send(Done {
                    page: job.page,
                    path: Some(job.out),
                    epoch: job.epoch,
                    error: None,
                });
                continue;
            }
            let Some(dir) = job.out.parent() else {
                continue;
            };
            if std::fs::create_dir_all(dir).is_err() {
                let _ = done.send(Done {
                    page: job.page,
                    path: None,
                    epoch: job.epoch,
                    error: Some("the page cache could not be created".to_string()),
                });
                continue;
            }
            let stem = job
                .out
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "page".to_string());
            let prefix = dir.join(format!(".{stem}.part"));
            // JPEG, not PNG. The rasteriser's cost is *encoding*, not
            // decoding: measured on the 46-page Chinkoyo deck, one page at
            // this DPI took **1.57 s** as PNG and **0.046 s** as JPEG — 34×
            // — and the pixels are the same page. A page that takes a second
            // and a half to appear is a page the reader waits for; the
            // reader's own scroll asked for a new page every time the
            // scroll settled, and each one paid that second and a half.
            //
            // The file is loaded by `gdk::Texture::from_filename`, which
            // decodes by content, not by name — and the app already puts
            // JPEG PDF covers on the shelf, so this format is one GTK
            // already reads here.
            let output = std::process::Command::new("pdftoppm")
                .arg("-jpeg")
                .arg("-jpegopt")
                .arg("quality=95")
                .arg("-r")
                .arg(job.dpi.to_string())
                .arg("-f")
                .arg(job.page.to_string())
                .arg("-l")
                .arg(job.page.to_string())
                .arg("-singlefile")
                .arg(&job.pdf)
                .arg(&prefix)
                .output();
            // `pdftoppm` appends the extension to the prefix it is given, so
            // the file it writes is "<prefix>.jpg" — the prefix's own ".part"
            // is not an extension to be replaced.
            let produced = PathBuf::from(format!("{}.jpg", prefix.display()));
            let ok = matches!(&output, Ok(o) if o.status.success()) && produced.exists();
            let error = if ok {
                None
            } else {
                match &output {
                    Ok(o) => Some(
                        String::from_utf8_lossy(&o.stderr)
                            .lines()
                            .next()
                            .unwrap_or("the page could not be drawn")
                            .to_string(),
                    ),
                    Err(e) => Some(format!("pdftoppm could not be run: {e}")),
                }
            };
            if ok {
                let _ = std::fs::rename(&produced, &job.out);
            }
            let _ = std::fs::remove_file(&produced);
            let _ = done.send(Done {
                page: job.page,
                path: ok.then(|| job.out.clone()),
                epoch: job.epoch,
                error,
            });
        }
    });
}

/// The cache file for a page: the book's id, the DPI and the page number.
///
/// The DPI is in the key on purpose: a zoom change re-renders at a new size
/// instead of overwriting the page the reader may come back to.
///
/// The extension is `.jpg` because that is what the rasteriser writes (see
/// [`spawn_worker`]). A `.png` file from the old rasteriser is a *different*
/// name, so it can never be mistaken for a rendered page — it is simply
/// dead weight, swept by [`sweep_stale_page_cache`].
pub(crate) fn cache_path(book_id: &str, dpi: u32, page: usize) -> PathBuf {
    sn_core::paths::cache_dir()
        .join("pdf-pages")
        .join(book_id)
        .join(dpi.to_string())
        .join(format!("p{page:05}.jpg"))
}

/// Remove the PNG page cache the old rasteriser left behind.
///
/// The cache grew to 477 MB on this box, on a disk that is 97% full, and
/// every one of those files was a page the new rasteriser will never read
/// (the names differ: `p00007.png` against `p00007.jpg`). The sweep runs
/// once at startup, off the UI thread, and only touches the `pdf-pages`
/// tree — the app's own cache, nothing else.
pub fn sweep_stale_page_cache() {
    let root = sn_core::paths::cache_dir().join("pdf-pages");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let mut removed = 0usize;
    for book in entries.filter_map(|e| e.ok()) {
        let Ok(dpis) = std::fs::read_dir(book.path()) else {
            continue;
        };
        for dpi in dpis.filter_map(|e| e.ok()) {
            let Ok(pages) = std::fs::read_dir(dpi.path()) else {
                continue;
            };
            for page in pages.filter_map(|e| e.ok()) {
                let path = page.path();
                if path.extension().is_some_and(|x| x == "png") {
                    let _ = std::fs::remove_file(&path);
                    removed += 1;
                }
            }
        }
    }
    if removed > 0 {
        log::info!("swept {removed} stale PNG page-cache files");
    }
}
