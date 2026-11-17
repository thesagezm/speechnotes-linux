//! Export a note, or the whole library, out of the app.
//!
//! Three formats, one rule: **an export is a snapshot, not a sync.** Nothing
//! here writes back into the library, and nothing here can fail halfway
//! through and leave a half-written file where a whole one is expected — the
//! bytes are built in full, then written atomically, so a cancelled dialog or a
//! full disk leaves the previous file untouched.
//!
//! Markdown export carries images by reference to the live files, so it stays
//! readable in any other markdown app. A "bundle" export (Markdown plus an
//! `images/` folder) is the self-contained form and is what the button offers.

use std::path::{Path, PathBuf};

use sn_store::model::Note;

/// What to export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    One(String),
    All,
}

/// The file formats on offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The note's source text, unchanged.
    Markdown,
    /// Prose only: markup removed, ready for a word processor or a speech engine.
    Text,
    /// Markdown plus a folder of the images, so it travels as one thing.
    Bundle,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Markdown => "Markdown (.md)",
            Format::Text => "Plain text (.txt)",
            Format::Bundle => "Markdown + images (folder)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Markdown | Format::Bundle => "md",
            Format::Text => "txt",
        }
    }
}

/// Bytes for a single-note export.
pub fn one(note: &Note, format: Format) -> String {
    match format {
        Format::Markdown | Format::Bundle => note.text.clone(),
        Format::Text => {
            let doc = sn_format::parse_markdown(&note.text);
            let mut out = doc.plain();
            out.push('\n');
            out
        }
    }
}

/// Bytes for a whole-library export: notes as a folder of markdown files.
///
/// One file per note, named `<index>-<slug>.md`, because a single concatenated
/// file cannot carry per-note titles or notebooks and no other markdown app can
/// split it back apart. The manifest at the top records the library's shape so
/// the export is readable without the app.
pub fn all(notes: &[Note], notebooks: &[(String, String)]) -> String {
    let mut out = String::new();
    out.push_str("# Speechnotes export\n\n");
    out.push_str(&format!("{} note(s), {} notebook(s).\n\n", notes.len(), notebooks.len()));
    for (id, name) in notebooks {
        out.push_str(&format!("- notebook `{id}`: {name}\n"));
    }
    out.push_str("\nOne file per note follows, in the order the list shows them.\n");
    out
}

/// The file name for a note's export.
///
/// The index keeps the folder in list order when a file manager sorts by name,
/// and the slug keeps it recognisable; the note id is not in the name because it
/// is 36 characters of noise to a human and the manifest has it.
pub fn slug_for(note: &Note, index: usize) -> String {
    let title = note.title();
    let mut slug: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-').to_lowercase();
    let slug: String = slug.chars().take(48).collect();
    let slug = if slug.is_empty() {
        "untitled".to_string()
    } else {
        slug
    };
    format!("{:03}-{slug}", index + 1)
}

/// Write `bytes` to `path` atomically, creating the parent directory.
pub fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    sn_core::atomic::write_atomic(path, bytes)
}

/// Copy a note's images next to an exported markdown file.
///
/// Returns how many were copied. A missing source file is skipped rather than
/// failing the export: the note's text is the thing the user asked for, and a
/// stale image reference should not cost them the export.
pub fn copy_images(
    images_root: &Path,
    note_id: &str,
    into: &Path,
) -> std::io::Result<usize> {
    let src = images_root.join(note_id);
    if !src.is_dir() {
        return Ok(0);
    }
    let dst = into.join("images");
    std::fs::create_dir_all(&dst)?;
    let mut copied = 0usize;
    for entry in std::fs::read_dir(&src)?.flatten() {
        let name = entry.file_name();
        if std::fs::copy(entry.path(), dst.join(&name)).is_ok() {
            copied += 1;
        }
    }
    Ok(copied)
}

/// Rewrite a note's image references to the bundled `images/` folder.
///
/// The exported markdown has to *work* where it lands: `speechnotes://` means
/// nothing to another app, so each reference becomes `images/<name>`, which is
/// what every markdown renderer resolves.
pub fn rebind_images(text: &str) -> String {
    text.replace(
        crate::images::IMAGE_SCHEME,
        "images/",
    )
}

/// A default export directory under the user's data dir.
pub fn default_dir() -> PathBuf {
    sn_core::paths::data_dir().join("Exports")
}

/// A suggested file name for a note export, without a directory.
pub fn suggested_name(note: &Note, format: Format) -> String {
    format!("{}.{}", slug_for(note, 0), format.extension())
}

// ---------------------------------------------------------------------------
// JEX (Joplin export) interchange
// ---------------------------------------------------------------------------
// User-reported (2026-10-30): `sn_store::jex` can parse and build
// archives — BookDrop's receiver and `--import-note` both import them — but no
// GUI surface reached it, so a user could not import or export a JEX notebook
// from the app. The settings Data page wires these two functions up.

/// One image a note carries, in the form [`sn_store::jex::ExportNote`] wants.
struct JexImage {
    /// The file's name inside the note's image directory (what the body's
    /// `speechnotes://note-image/<name>` reference names).
    name: String,
    resource_id: String,
    extension: String,
    mime: &'static str,
    bytes: Vec<u8>,
}

/// A stable Joplin resource id for a note's image file: 32 hex characters
/// (JEX ids are dash-less), derived from the note id and the image's name, so
/// the same picture exports under the same id every time (Joplin dedups
/// resources by id).
fn jex_resource_id(note_id: &str, name: &str) -> String {
    let seed = format!("{note_id}/{name}");
    let a = sn_core::util::stable_hash_bytes(seed.as_bytes());
    let b = sn_core::util::stable_hash_bytes(format!("jex/{seed}").as_bytes());
    format!("{a:016x}{b:016x}")
}

fn mime_of(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// A note's images as Joplin resources. Every file in the note's image
/// directory is included: the directory holds exactly what the note's text
/// references (the store dedups by content), and a file the text no longer
/// names is one delete away from being right again — exporting it is the
/// honest superset. A file that cannot be read is skipped, not fatal.
fn jex_images(note_id: &str) -> Vec<JexImage> {
    let dir = sn_core::paths::note_images_dir().join(note_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let ext = name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        out.push(JexImage {
            resource_id: jex_resource_id(note_id, &name),
            name,
            extension: ext.clone(),
            mime: mime_of(&ext),
            bytes,
        });
    }
    out
}

/// Build a `.jex` archive of one notebook (the notebook plus its notes), or
/// of the whole library when `notebook_id` is `None`. The bytes are complete
/// before any caller writes them: `build_archive` assembles the tar in memory
/// and the settings page writes it atomically, so a cancelled save dialog
/// cannot leave a half archive behind.
pub fn jex_archive(
    notes: &sn_store::NotesStore,
    notebooks: &sn_store::NotebooksStore,
    notebook_id: Option<&str>,
) -> Vec<u8> {
    let all = notebooks.all();
    let chosen: Vec<sn_store::model::Notebook> = match notebook_id {
        Some(id) => all.iter().filter(|nb| nb.id == id).cloned().collect(),
        None => all,
    };
    let active = notes.active();
    // Owned copies with the image links rewritten: the body's
    // `speechnotes://note-image/<name>` means nothing to Joplin — its
    // importer resolves resources by the `:/<resource id>` form, so each
    // reference becomes one before the archive is built. (The iOS exporter
    // shipped the raw body, and its images arrived in Joplin as unused
    // resources behind dead links; this is the one deliberate improvement.)
    let mut owned: Vec<Note> = active
        .iter()
        .filter(|note| match notebook_id {
            Some(id) => note.notebook_id.as_deref() == Some(id),
            None => true,
        })
        .cloned()
        .collect();
    let mut images_by_note: std::collections::HashMap<String, Vec<JexImage>> =
        std::collections::HashMap::new();
    for note in &mut owned {
        let images = jex_images(&note.id);
        for img in &images {
            let old = format!("({}{})", crate::images::IMAGE_SCHEME, img.name);
            let new = format!("(:/{})", img.resource_id);
            if note.text.contains(&old) {
                note.text = note.text.replace(&old, &new);
            }
        }
        images_by_note.insert(note.id.clone(), images);
    }
    let exports: Vec<sn_store::jex::ExportNote> = owned
        .iter()
        .map(|note| sn_store::jex::ExportNote {
            note,
            images: images_by_note
                .get(&note.id)
                .map(|imgs| {
                    imgs.iter()
                        .map(|img| {
                            (img.resource_id.clone(), img.extension.clone(), img.mime.to_string(), img.bytes.clone())
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    sn_store::jex::build_archive(&exports, &chosen)
}

/// A suggested `.jex` file name for a notebook (or the whole library).
pub fn jex_suggested_name(name: &str) -> String {
    let mut slug: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    while slug.starts_with('-') {
        slug.remove(0);
    }
    if slug.is_empty() {
        slug = "library".to_string();
    }
    slug.truncate(60);
    format!("{slug}.jex")
}

/// Import a `.jex` archive into the library. The store's own importer (the
/// same one BookDrop and `--import-note` use) merges on notebook name and
/// reports what landed; the caller flushes both stores and refreshes.
pub fn jex_import(
    notes: &sn_store::NotesStore,
    notebooks: &sn_store::NotebooksStore,
    path: &Path,
) -> Result<String, String> {
    let report = sn_store::imports::import_jex(notes, notebooks, path)?;
    notes.flush();
    notebooks.flush();
    Ok(format!(
        "Imported {} note(s), {} notebook(s) from {}",
        report.notes_imported,
        report.notebooks_created,
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(text: &str) -> Note {
        let mut n = Note::new(None);
        n.text = text.to_string();
        n
    }

    /// A Joplin resource id is 32 hex characters (the dash-less id form
    /// `jex::joplin_id` produces); the round trip back through
    /// `uuid_from_joplin_id` (what the import side does to turn a resource id
    /// into a file name) must resolve.
    #[test]
    fn a_jex_resource_id_is_a_valid_joplin_id() {
        for (note_id, name) in [
            ("2FCF3446-8B79-4761-A398-69B869134C70", "cat-123.png"),
            ("note-two", "weird name.jpeg"),
            ("", ""),
        ] {
            let id = jex_resource_id(note_id, name);
            assert_eq!(id.len(), 32, "{id}");
            assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{id}");
            assert!(
                sn_store::jex::uuid_from_joplin_id(&id).is_some(),
                "{id} does not round-trip"
            );
            // And it is stable: the same input names the same resource, so a
            // re-export dedups in Joplin instead of duplicating.
            assert_eq!(id, jex_resource_id(note_id, name));
        }
    }

    #[test]
    fn a_jex_file_name_is_slug_safe() {
        assert_eq!(jex_suggested_name("My Notebook"), "My-Notebook.jex");
        assert_eq!(jex_suggested_name("../evil/name"), "evil-name.jex");
        assert_eq!(jex_suggested_name("///"), "library.jex");
    }

    #[test]
    fn markdown_export_is_the_note_unchanged() {
        let n = note("# Title\n\nWith **bold** and a ![cat](speechnotes://note-image/a.png).");
        assert_eq!(one(&n, Format::Markdown), n.text);
    }

    #[test]
    fn text_export_strips_the_markup_but_keeps_the_words() {
        let n = note("# Heading\n\nWith **bold**, *italic* and `code`.");
        let out = one(&n, Format::Text);
        assert!(!out.contains('#'), "{out}");
        assert!(!out.contains("**"), "{out}");
        assert!(out.contains("bold"), "{out}");
        assert!(out.contains("italic"), "{out}");
        assert!(out.contains("code"), "{out}");
    }

    #[test]
    fn a_bundled_export_points_at_a_folder_that_exists_next_to_it() {
        let n = note("![cat](speechnotes://note-image/a.png)");
        let out = rebind_images(&one(&n, Format::Bundle));
        assert!(out.contains("images/a.png"), "{out}");
        assert!(!out.contains("speechnotes://"), "{out}");
    }

    #[test]
    fn slugs_are_ordered_and_readable() {
        let a = note("Hello, world!");
        let b = note("  ??  ");
        assert_eq!(slug_for(&a, 0), "001-hello-world");
        assert_eq!(slug_for(&b, 1), "002-untitled");
    }

    #[test]
    fn a_long_or_odd_title_still_produces_a_usable_filename() {
        let mut n = note("");
        n.explicit_title = Some("../escape/with slashes and \\ and | pipes".into());
        let slug = slug_for(&n, 0);
        assert!(!slug.contains('/'), "{slug}");
        assert!(!slug.contains('\\'), "{slug}");
        assert!(!slug.contains(".."), "{slug}");
        assert!(slug.len() <= 60, "{slug}");
    }

    #[test]
    fn the_library_export_leads_with_a_manifest() {
        let notes = vec![note("first"), note("second")];
        let books = vec![("NB1".to_string(), "Pediatric Surgery".to_string())];
        let out = all(&notes, &books);
        assert!(out.contains("2 note(s)"), "{out}");
        assert!(out.contains("Pediatric Surgery"), "{out}");
    }

    #[test]
    fn writing_an_export_leaves_no_half_file_behind() {
        let dir = std::env::temp_dir().join(format!("sn-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/note.md");
        write_file(&path, b"hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        // Overwriting goes through a temp file and a rename, so a reader never
        // sees a partial note.
        write_file(&path, b"goodbye").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "goodbye");
        // No temp files left in the directory.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "a temp file was left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
