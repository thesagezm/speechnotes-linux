//! Note images: store, deduplicate, and account for the files a note holds.
//!
//! Images live in `note-images/<note-id>/` and are referenced from the note's
//! own markdown as `![alt](speechnotes://note-image/<name>)`, which is the
//! scheme iOS and Android already write and read.
//!
//! **Dedup by content, not by name.** Picking the same picture twice in a note
//! must not write it to disk twice, and two notes that happen to contain the
//! same picture should share one file. That is a hash of the bytes, and it is
//! also the only rule that survives the user renaming or re-picking a file.
//!
//! This is the one implementation for every writer of note images — the
//! editor's insert path and the docx import both call [`store`] — so the dedup
//! and naming rules cannot drift apart the way the import path's private copy
//! once could.

use std::path::{Path, PathBuf};

use sn_core::AppleDate;

/// What an image reference resolves to.
#[derive(Debug, Clone)]
pub struct StoredImage {
    /// File name inside the note's image directory.
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
}

/// Where a note's images live.
pub fn note_dir(images_root: &Path, note_id: &str) -> PathBuf {
    images_root.join(note_id)
}

/// Reject anything that is not a plain image file name.
///
/// The name arrives from a file picker, but it is joined onto a directory and
/// written into a note that may be re-opened later, so it is validated once,
/// here, rather than trusted at three call sites.
pub fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.contains('/')
        && !name.contains('\\')
        && name != "."
        && name != ".."
        && !name.contains("..")
        && name.chars().all(|c| !c.is_control())
}

/// The file name for `bytes`: a short content hash plus a sanitised stem.
pub fn name_for(source: &Path, bytes: &[u8]) -> String {
    let stem: String = source
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".to_string());
    // A stem can be anything the user's filesystem allows; keep it readable
    // and drop anything that would need quoting.
    let mut clean: String = stem
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    while clean.starts_with('-') {
        clean.remove(0);
    }
    if clean.is_empty() {
        clean = "image".to_string();
    }
    clean.truncate(60);
    let ext = source
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .filter(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"))
        .unwrap_or_else(|| "png".to_string());
    format!("{clean}-{}.{ext}", sn_core::util::stable_hash_bytes(bytes))
}

/// Store `bytes` for `note_id`, returning the image.
///
/// Idempotent by **content**: the same picture inserted twice is one file,
/// however it was named on the way in. The readable stem is a convenience for
/// the person looking at the directory and is deliberately not part of the
/// identity — putting it in the name meant "same picture, two different source
/// names" wrote two files, which is the case dedup exists to prevent.
///
/// So the name is `<stem>-<content hash>.<ext>`, and a store first looks for any
/// file already in the note's directory whose name ends in that hash. Finding
/// one means the bytes are already here, whatever they were called.
///
/// The bytes are sniffed for an image signature first. A file that is not an
/// image would otherwise sit in the note's directory forever, referenced by a
/// note, and fail at render time.
pub fn store(
    images_root: &Path,
    note_id: &str,
    source: &Path,
    bytes: &[u8],
) -> std::io::Result<StoredImage> {
    if !sn_core::util::is_supported_image(bytes) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "that file is not an image",
        ));
    }
    let dir = note_dir(images_root, note_id);
    std::fs::create_dir_all(&dir)?;

    let hash = sn_core::util::stable_hash_bytes(bytes);
    if let Some(existing) = find_by_hash(&dir, hash) {
        return Ok(existing);
    }

    let name = name_for(source, bytes);
    let path = dir.join(&name);
    sn_core::atomic::write_atomic(&path, bytes)?;
    Ok(StoredImage { name, path, bytes: bytes.len() as u64 })
}

/// An image already in `dir` whose content hash is `hash`, if there is one.
fn find_by_hash(dir: &Path, hash: i64) -> Option<StoredImage> {
    let suffix = format!("-{hash}.");
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.contains(&suffix) {
            let bytes = entry.metadata().ok()?.len();
            return Some(StoredImage { name, path: entry.path(), bytes });
        }
    }
    None
}

/// Delete a note's images, keeping any name still referenced elsewhere.
///
/// A purge is the only time this runs, and it is the one place a note's files
/// could be removed while another note in the same directory still points at
/// them — the directory is per note, so that cannot happen today, and the
/// check is what stops it happening if the layout ever changes.
pub fn purge(images_root: &Path, note_id: &str, still_referenced: &[String]) -> std::io::Result<()> {
    let dir = note_dir(images_root, note_id);
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if still_referenced.iter().any(|r| *r == name) {
            continue;
        }
        let _ = std::fs::remove_file(entry.path());
    }
    let _ = std::fs::remove_dir(&dir);
    Ok(())
}

/// How many image files a note is holding, for the settings pane.
pub fn count(images_root: &Path, note_id: &str) -> usize {
    std::fs::read_dir(note_dir(images_root, note_id))
        .map(|d| d.flatten().count())
        .unwrap_or(0)
}

/// The modification time of the newest image, or now when there is none.
pub fn newest(images_root: &Path, note_id: &str) -> AppleDate {
    let newest = std::fs::read_dir(note_dir(images_root, note_id))
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.metadata().ok())
                .filter_map(|m| m.modified().ok())
                .max()
        });
    match newest.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()) {
        Some(d) => AppleDate::from_unix(d.as_secs_f64()),
        None => AppleDate::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "sn-img-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The smallest byte string that is a real image: a 1x1 PNG.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x72,
        0xB6, 0x0D, 0x24, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0xF0, 0x1F, 0x84, 0x19, 0x60, 0x0C, 0x00, 0x47, 0xCA, 0x07, 0xF9, 0x67, 0x59,
        0x6E, 0xB7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn the_same_bytes_produce_one_file_whatever_their_names() {
        let root = tmp();
        let a = store(&root, "note-1", Path::new("shot.PNG"), PNG).unwrap();
        let b = store(&root, "note-1", Path::new("other name.png"), PNG).unwrap();
        assert_eq!(a.name, b.name, "the name is the hash of the bytes");
        assert_eq!(count(&root, "note-1"), 1);
        assert_eq!(a.path, b.path);
    }

    #[test]
    fn a_name_from_a_picker_cannot_escape_the_directory() {
        assert!(is_safe_name("photo.png"));
        assert!(!is_safe_name("../escape.png"));
        assert!(!is_safe_name("a/b.png"));
        assert!(!is_safe_name(".."));
        assert!(!is_safe_name(""));
        assert!(!is_safe_name("with\nnewline.png"));
        let n = name_for(Path::new("/tmp/a b*c.png"), PNG);
        assert!(is_safe_name(&n), "{n}");
        assert!(n.ends_with(".png"), "{n}");
    }

    #[test]
    fn something_that_is_not_an_image_is_refused() {
        let root = tmp();
        let err = store(&root, "note-1", Path::new("x.png"), b"not an image at all").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(count(&root, "note-1"), 0, "nothing written");
    }

    #[test]
    fn a_purge_leaves_a_name_that_is_still_referenced() {
        let root = tmp();
        let first = store(&root, "note-1", Path::new("a.png"), PNG).unwrap();
        let mut other = PNG.to_vec();
        let last = other.len() - 1;
        other[last] = 0x81;
        let second = store(&root, "note-1", Path::new("b.png"), &other).unwrap();

        purge(&root, "note-1", &[second.name.clone()]).unwrap();
        assert!(
            !note_dir(&root, "note-1").join(&first.name).exists(),
            "the unreferenced file is gone"
        );
        assert!(
            note_dir(&root, "note-1").join(&second.name).exists(),
            "a still-referenced file is kept"
        );
    }
}
