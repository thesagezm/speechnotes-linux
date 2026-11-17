//! Note images: the markdown-facing half.
//!
//! The storage core — where the files live, the dedup-by-content store, the
//! name rules — lives in [`sn_store::images`], where the docx import shares it
//! without dragging GTK into the headless path. This module re-exports that
//! core for the editor's callers and keeps only what is made of markdown: the
//! `speechnotes://note-image/` reference and the names a note's text points at.
//!
//! Images live in `note-images/<note-id>/` and are referenced from the note's
//! own markdown as `![alt](speechnotes://note-image/<name>)`, which is the
//! scheme iOS and Android already write and read.

pub use sn_store::images::{
    count, is_safe_name, name_for, newest, note_dir, purge, store, StoredImage,
};

/// The scheme the app's images are written with.
pub const IMAGE_SCHEME: &str = "speechnotes://note-image/";

/// The markdown reference for a stored image.
pub fn reference(name: &str, alt: &str) -> String {
    format!("![{}]({IMAGE_SCHEME}{name})", escape_alt(alt))
}

fn escape_alt(alt: &str) -> String {
    alt.replace(['[', ']'], "")
}

/// The image names a note references, in order, with duplicates removed.
///
/// Used when a note is purged: the files go with it, but only the ones no
/// other reference in that note needs. Duplicates in one note are one file by
/// construction, so this is really about not deleting a name twice.
pub fn referenced_names(text: &str) -> Vec<String> {
    let doc = sn_format::parse_markdown(text);
    let mut out: Vec<String> = Vec::new();
    for name in image_names_in(&doc.blocks) {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

fn image_names_in(blocks: &[sn_format::markdown::Block]) -> Vec<String> {
    use sn_format::markdown::Block;
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Heading { inlines, .. } | Block::Paragraph { inlines } => {
                out.extend(names_in(inlines))
            }
            Block::ListItem { inlines, .. } | Block::TaskItem { inlines, .. } => {
                out.extend(names_in(inlines))
            }
            Block::Quote { blocks } => out.extend(image_names_in(blocks)),
            Block::Table { header, rows, .. } => {
                for c in header.iter().chain(rows.iter().flatten()) {
                    out.extend(names_in(c));
                }
            }
            _ => {}
        }
    }
    out
}

fn names_in(inlines: &[sn_format::markdown::Inline]) -> Vec<String> {
    inlines
        .iter()
        .filter_map(|i| match i {
            sn_format::markdown::Inline::Image { src, .. } => {
                src.strip_prefix(IMAGE_SCHEME).map(str::to_string)
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// One directory per test, removed first: the tests share a process and a
    /// fixed path would let one test's cleanup delete another's fixture
    /// mid-run — which is exactly how the first version of these tests failed.
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

    /// A 1x1 JPEG, so "two different images" tests two different byte strings
    /// rather than a PNG with one byte changed — which may or may not still be
    /// a PNG, and made these tests depend on the last byte's value.
    const JPEG: &[u8] = &[
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00,
        0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06,
        0x05, 0x08, 0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B,
        0x0C, 0x19, 0x12, 0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20,
        0x24, 0x2E, 0x27, 0x20, 0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29, 0x2C, 0x30, 0x31,
        0x34, 0x34, 0x34, 0x1F, 0x39, 0x3D, 0x38, 0x32, 0x3C, 0x2E, 0x33, 0x32, 0xFF, 0xC0, 0x00,
        0x0B, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00, 0x1F, 0x00,
        0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0xFF, 0xC4,
        0x00, 0x14, 0x10, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00, 0x7B,
        0x40, 0xFF, 0xD9,
    ];

    /// The smallest byte string that is a real image: a 1x1 PNG.
    /// A REAL 2x2 RGBA PNG. The first fixture here was 1x1 with an IDAT one
    /// byte short of what its own filter+pixels need — every test passed
    /// because no test decoded it with a real decoder, and a "missing image"
    /// label in the app was how the bad fixture eventually surfaced.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x72,
        0xB6, 0x0D, 0x24, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0xF0, 0x1F, 0x84, 0x19, 0x60, 0x0C, 0x00, 0x47, 0xCA, 0x07, 0xF9, 0x67, 0x59,
        0x6E, 0xB7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    /// The name is the hash of the bytes alone. The readable stem is a
    /// convenience for the human looking at the directory, and is deliberately
    /// NOT part of the identity — otherwise picking the same picture twice
    /// under two different names would write it twice, which is the case this
    /// whole module exists to prevent.
    #[test]
    fn the_same_bytes_produce_one_file() {
        let root = tmp();
        let a = store(&root, "note-1", Path::new("shot.PNG"), PNG).unwrap();
        let b = store(&root, "note-1", Path::new("other name.png"), PNG).unwrap();
        assert_eq!(
            count(&root, "note-1"),
            1,
            "picking the same picture twice must write one file"
        );
        assert!(
            a.path == b.path || a.name == b.name,
            "the two calls must agree on where the image lives: {:?} vs {:?}",
            a.path,
            b.path
        );
    }

    #[test]
    fn different_bytes_get_different_files() {
        let root = tmp();
        let a = store(&root, "note-1", Path::new("a.png"), PNG).unwrap();
        let b = store(&root, "note-1", Path::new("a.jpg"), JPEG).unwrap();
        assert_ne!(a.name, b.name);
        assert_eq!(count(&root, "note-1"), 2);
    }

    #[test]
    fn notes_do_not_share_a_directory() {
        let root = tmp();
        let a = store(&root, "note-1", Path::new("a.png"), PNG).unwrap();
        let b = store(&root, "note-2", Path::new("a.png"), PNG).unwrap();
        assert_eq!(a.name, b.name, "same content, same name");
        assert_ne!(a.path, b.path, "but each note has its own copy");
    }

    #[test]
    fn a_name_from_a_picker_cannot_escape_the_directory() {
        assert!(is_safe_name("photo.png"));
        assert!(!is_safe_name("../escape.png"));
        assert!(!is_safe_name("a/b.png"));
        assert!(!is_safe_name(".."));
        assert!(!is_safe_name(""));
        assert!(!is_safe_name("with\nnewline.png"));
        // A stem full of characters we strip still yields a safe name.
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
    fn the_reference_uses_the_scheme_the_other_apps_write() {
        let r = reference("shot-123.png", "a cat");
        assert_eq!(r, "![a cat](speechnotes://note-image/shot-123.png)");
    }

    #[test]
    fn a_note_that_references_the_same_image_twice_lists_it_once() {
        let text = "![a](speechnotes://note-image/x.png)\n\nagain ![b](speechnotes://note-image/x.png)\n\n![c](speechnotes://note-image/y.png)";
        assert_eq!(referenced_names(text), vec!["x.png", "y.png"]);
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
