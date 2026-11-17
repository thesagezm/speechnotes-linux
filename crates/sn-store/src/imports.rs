//! Import files into the library: `.docx` (office) and `.jex` (Joplin).
//!
//! One function per format, one rule both share: **an import that lands
//! partial beats one that lands nothing.** A docx whose images cannot all
//! be stored still imports its text; a JEX entry that fails to parse is
//! skipped while the rest import. The return value is the honest tally.

use std::path::Path;

use crate::jex;
#[cfg(test)]
use crate::jex::ExportNote;
use crate::{Note, NotebooksStore, NotesStore};

/// What one import did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportReport {
    pub notes_imported: usize,
    pub notes_skipped: usize,
    pub notebooks_created: usize,
    pub images_stored: usize,
    pub errors: Vec<String>,
}

impl ImportReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Import a `.docx` file as one note.
///
/// The text becomes the note's markdown; the document's images are stored
/// under the note's image directory through the same dedup-by-content store
/// the editor's insert button uses, and the body's image references are
/// rewritten to the `speechnotes://note-image/` scheme — so a note imported
/// from Word shows its pictures, not "missing image" labels.
pub fn import_docx(
    notes: &NotesStore,
    path: &Path,
    notebook_id: Option<String>,
) -> Result<ImportReport, String> {
    // A real office document is measured in megabytes; the cap turns a LAN
    // peer's giant "docx" into a refused import instead of an OOM.
    let bytes = sn_core::util::read_capped(path, 256 * 1024 * 1024)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let parsed = sn_doc::docx::parse(&bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if parsed.text.trim().is_empty() && parsed.images.is_empty() {
        return Err(format!("{}: no text and no images in it", path.display()));
    }
    // The parser carries member paths, not bytes, so the archive is opened
    // once here and every image reads out of it. Re-parsing per image — the
    // old shape — cloned and re-walked the whole file for every picture.
    let zip = sn_doc::zip::Archive::parse(bytes)
        .map_err(|e| format!("{}: {e}", path.display()))?;

    let mut report = ImportReport::default();
    let note = notes.create(notebook_id);

    // The images, stored first so the body can reference them by the name
    // `store` chose (it dedups by content, so importing the same file twice
    // reuses the same files).
    let mut body = parsed.text.clone();
    let mut source_names: Vec<String> = Vec::new();
    for image_path in &parsed.images {
        let Ok(image_bytes) = zip.read(image_path) else {
            report
                .errors
                .push(format!("image {image_path} could not be read"));
            source_names.push(String::new());
            continue;
        };
        let source = Path::new(image_path);
        match crate::images::store(
            &sn_core::paths::note_images_dir(),
            &note.id,
            source,
            &image_bytes,
        ) {
            Ok(stored) => {
                report.images_stored += 1;
                source_names.push(stored.name);
            }
            Err(e) => {
                report
                    .errors
                    .push(format!("image {image_path}: {e}"));
                source_names.push(String::new());
            }
        }
    }
    // The parser emits inline `![](images/<file>)` markers only when the
    // document is IMAGE-ONLY; a document with text lists its images
    // separately. Rewrite the markers where they exist, and append the
    // stored references after the text where they do not — a note with a
    // lost picture is a broken note either way.
    let has_markers = body.lines().any(|l| l.starts_with("![](images/"));
    if has_markers {
        let mut rewritten = String::new();
        let mut image_iter = source_names.iter();
        for line in body.lines() {
            if line.starts_with("![](images/") {
                match image_iter.next() {
                    Some(name) if !name.is_empty() => {
                        rewritten.push_str(&format!(
                            "![]({SNIPPET_SCHEME}{name})\n"
                        ));
                    }
                    _ => {
                        rewritten.push_str(line);
                        rewritten.push('\n');
                    }
                }
                continue;
            }
            rewritten.push_str(line);
            rewritten.push('\n');
        }
        body = rewritten;
    } else {
        for name in &source_names {
            if !name.is_empty() {
                body.push_str(&format!("\n![]({SNIPPET_SCHEME}{name})\n"));
            }
        }
    }

    // The created note is blank; the body is set unconditionally (the
    // equality short-circuit below would otherwise leave every image-less
    // document an empty note).
    notes.update_text(&note.id, &body);
    report.notes_imported += 1;
    Ok(report)
}

const SNIPPET_SCHEME: &str = "speechnotes://note-image/";

/// Import a `.jex` archive: notebooks first (so notes can file into them),
/// then notes, then resources as note images.
///
/// Merge semantics, matching the other apps: an existing note id updates in
/// place; a new one is created. The docx rule about partial imports holds —
/// one bad note costs itself, not the archive.
pub fn import_jex(
    notes: &NotesStore,
    notebooks: &NotebooksStore,
    path: &Path,
) -> Result<ImportReport, String> {
    let bytes = sn_core::util::read_capped(path, 256 * 1024 * 1024)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let archive =
        jex::parse_archive(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut report = ImportReport::default();

    // Every mutation in the loops below would otherwise encode the whole
    // library once per note; one save at the end covers the archive.
    let _batch = notes.batch_saves();

    // Notebooks: JEX id → this library's notebook id.
    let mut notebook_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for imported in &archive.notebooks {
        // The notebook merges on NAME (both apps' rule); the JEX id is only
        // validated so a corrupt archive is named honestly.
        if jex::uuid_from_joplin_id(&imported.id).is_none() {
            report.notes_skipped += 1;
            report
                .errors
                .push(format!("notebook {} has an unreadable id", imported.id));
            continue;
        }
        // An existing notebook with that name is THE notebook — Joplin's
        // import merges on name, and so does ours.
        let existing = notebooks.by_name(&imported.title);
        let id = match existing {
            Some(nb) => nb.id,
            None => {
                let nb = notebooks.create(&imported.title);
                report.notebooks_created += 1;
                nb.id
            }
        };
        notebook_map.insert(imported.id.clone(), id);
    }

    // Notes: an id already in the library updates; otherwise it inserts with
    // its JEX identity intact, so a re-import updates in place.
    //
    // The library's ids come from `new_uuid()` (uppercase) but an older
    // import or a foreign file may have stored lowercase; the JEX id is
    // caseless hex, so the lookup folds before concluding "new". The fold
    // index is built once — the old lookup cloned the whole library per
    // note — and covers binned notes too, which the merge below refuses to
    // touch rather than duplicating.
    let mut known: std::collections::HashMap<String, String> = notes
        .active()
        .into_iter()
        .chain(notes.binned())
        .map(|n| (n.id.to_lowercase(), n.id))
        .collect();
    for imported in &archive.notes {
        let uuid = jex::uuid_from_joplin_id(&imported.id)
            .unwrap_or_else(sn_core::util::new_uuid);
        let notebook_id = imported
            .notebook_jex_id
            .as_deref()
            .and_then(|jex_id| notebook_map.get(jex_id).cloned());
        // One store scan for the note the id names — only when the id is
        // known, and its timestamp and bin state decide the merge.
        let existing =
            known
                .get(&uuid.to_lowercase())
                .and_then(|local_id| notes.get(local_id));
        match existing {
            Some(local) => {
                // A re-import must never clobber local work: the archive's
                // copy only wins when it is NEWER than the library's. A
                // binned note is left alone entirely — an import reviving a
                // deleted note is a surprise nobody asked for.
                if local.is_deleted() {
                    report.notes_skipped += 1;
                    continue;
                }
                if imported.updated_at.raw() <= local.updated_at.raw() {
                    report.notes_skipped += 1;
                    continue;
                }
                let local_id = local.id.clone();
                notes.update(&local_id, |note| {
                    note.text = imported.body.clone();
                    note.updated_at = imported.updated_at;
                    if note.notebook_id.is_none() {
                        note.notebook_id = notebook_id;
                    }
                });
                report.notes_imported += 1;
            }
            None => {
                let mut note = Note::new(notebook_id);
                note.id = uuid.clone();
                note.text = imported.body.clone();
                // The archive's own title, when it carried one. Joplin's
                // layout keeps the title separate from the body, so an
                // explicitly titled note gets its title back instead of
                // having one derived from the body's first sentence.
                note.explicit_title = imported.title.clone();
                note.created_at = imported.created_at;
                note.updated_at = imported.updated_at;
                notes.insert(note);
                // A later entry in the same archive with this id is an
                // update, and the index has to know the note exists now.
                known.insert(uuid.to_lowercase(), uuid.clone());
                report.notes_imported += 1;
            }
        }
    }

    // Resources: images land under the importing note's image directory and
    // the body's `![...](<jex-id>.<ext>)` references are rewritten to the
    // scheme. A resource whose note is absent is dropped — an image with no
    // note is a file nobody can see.
    //
    // Ownership is decided in ONE walk over the bodies: the first note that
    // names a resource claims it, and claimed resources drop out of the
    // working set. The old shape re-read every body once per resource —
    // O(resources × notes × body) on an import the user is waiting on.
    let note_ids: Vec<String> = archive
        .notes
        .iter()
        .map(|n| jex::uuid_from_joplin_id(&n.id).unwrap_or_else(sn_core::util::new_uuid))
        .collect();
    let mut owner_of: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for (note, note_id) in archive.notes.iter().zip(&note_ids) {
        if owner_of.len() == archive.resources.len() {
            break;
        }
        for resource in &archive.resources {
            if owner_of.contains_key(&resource.id) {
                continue;
            }
            let dashed = jex::uuid_from_joplin_id(&resource.id)
                .unwrap_or_else(|| resource.id.clone());
            if note.body.contains(&resource.id) || note.body.contains(&dashed) {
                owner_of.insert(resource.id.clone(), note_id.clone());
            }
        }
    }
    for resource in &archive.resources {
        let dashed = jex::uuid_from_joplin_id(&resource.id)
            .unwrap_or_else(|| resource.id.clone());
        let Some(owner) = owner_of.get(&resource.id).cloned() else {
            continue;
        };
        // No staging file: the store writes `bytes` and never reads back
        // through `source`, so a virtual path carries only the extension
        // into the stored name. A real file in shared /tmp would follow a
        // pre-planted symlink and write attacker bytes anywhere (the P0 the
        // critic caught — the id comes from the archive).
        let source_name = format!("resource.{}", resource.file_extension);
        let source = Path::new(&source_name);
        match crate::images::store(
            &sn_core::paths::note_images_dir(),
            &owner,
            source,
            &resource.data,
        ) {
            Ok(stored) => {
                report.images_stored += 1;
                // Rewrite every reference in the owning note's body.
                if let Some(note) = notes.get(&owner) {
                    // Both id spellings appear in the wild: the JEX 32-hex
                    // form (our exporter) and the dashed UUID (the body's
                    // `![pic](<uuid>.png)` in this app's own exports).
                    let mut text = note.text.clone();
                    for id_form in [&resource.id, &dashed] {
                        let new_ref = format!("({SNIPPET_SCHEME}{})", stored.name);
                        // The two reference spellings in the wild: our
                        // exporter's `(<id>.<ext>)` and Joplin's `(:/<id>)`.
                        for old_ref in [
                            format!("({}.{})", id_form, resource.file_extension),
                            format!("(:/{})", id_form),
                        ] {
                            if text.contains(&old_ref) {
                                text = text.replace(&old_ref, &new_ref);
                            }
                        }
                    }
                    if text != note.text {
                        notes.update_text(&owner, &text);
                    }
                }
            }
            Err(e) => {
                report.errors.push(format!("resource {}: {e}", resource.id));
            }
        }
    }

    Ok(report)
}

// A dead private `mod sn_ui_images` once lived here (store/StoredImage/
// find_by_hash/name_for mirrored from sn-ui::images); removed 2026-10-30 in a
// dead-code trim — zero references workspace-wide, and sn-ui's images.rs owns
// the real copies.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::test_support::XdgFixture;

    /// The docx fixture builder lives in sn-doc's tests; build the bytes
    /// here by the same layout (stored entries, correct central directory).
    fn docx(files: &[(&str, &[u8])]) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut table = [0u32; 256];
            for (i, slot) in table.iter_mut().enumerate() {
                let mut c = i as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { 0xedb88320 ^ (c >> 1) } else { c >> 1 };
                }
                *slot = c;
            }
            let mut crc = 0xffff_ffffu32;
            for &b in data {
                crc = table[((crc ^ b as u32) & 0xff) as usize] ^ (crc >> 8);
            }
            crc ^ 0xffff_ffff
        }
        let mut out: Vec<u8> = Vec::new();
        let mut central: Vec<u8> = Vec::new();
        let mut offset: u32 = 0;
        for (name, data) in files {
            let name_bytes = name.as_bytes();
            let crc = crc32(data);
            out.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
            out.extend_from_slice(&[0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0x00, 0x00]);
            out.extend_from_slice(name_bytes);
            let local_len = out.len() as u32 - offset;
            out.extend_from_slice(data);
            central.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
            // version made/needed, flags, method, time, date (12 bytes)
            central.extend_from_slice(&[0x0a, 0x00, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            // extra(2) comment(2) disk(2) internal(2) external(4)
            central.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name_bytes);
            offset += local_len + data.len() as u32;
        }
        let central_start = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&central_start.to_le_bytes());
        out.extend_from_slice(&[0x00, 0x00]);
        out
    }

    /// A REAL 2x2 RGBA PNG (short-IDAT fixtures decode fine until a real
    /// decoder sees them — the images.rs lesson).
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x06, 0x00, 0x00, 0x00, 0x72,
        0xB6, 0x0D, 0x24, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0xF0, 0x1F, 0x84, 0x19, 0x60, 0x0C, 0x00, 0x47, 0xCA, 0x07, 0xF9, 0x67, 0x59,
        0x6E, 0xB7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn a_docx_imports_its_text_and_its_images_into_note_images() {
        let _f = XdgFixture::new();
        sn_core::paths::ensure_directories().unwrap();
        let notes = NotesStore::open().unwrap();
        let xml = br#"<w:document xmlns:w="http://x" xmlns:a="http://a"><w:body>
<w:p><w:r><w:t>From Word.</w:t></w:r></w:p>
<w:p><w:r><w:drawing><a:blip r:embed="rId1"/></w:drawing></w:r></w:p>
</w:body></w:document>"#;
        let rels = br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/pic.png"/></Relationships>"#;
        let bytes = docx(&[
            ("word/document.xml", xml),
            ("word/_rels/document.xml.rels", rels),
            ("word/media/pic.png", PNG),
        ]);
        let dir = std::env::temp_dir().join(format!("import-docx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("from word.docx");
        std::fs::write(&path, &bytes).unwrap();

        let report = import_docx(&notes, &path, None).unwrap();
        assert_eq!(report.notes_imported, 1);
        assert_eq!(report.images_stored, 1, "the picture lands in note-images");
        assert!(report.is_ok(), "{:?}", report.errors);

        let note = &notes.active()[0];
        assert!(note.text.contains("From Word."), "{}", note.text);
        // The body's image reference is the app's scheme, not the docx
        // fallback — the preview resolves it from note-images.
        assert!(
            note.text.contains("speechnotes://note-image/"),
            "{}",
            note.text
        );
        assert!(!note.text.contains("images/pic.png"), "{}", note.text);
        // The file is really there and really the PNG.
        let dir = sn_core::paths::note_images_dir().join(&note.id);
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(entries.len(), 1);
        let stored = std::fs::read(entries[0].path()).unwrap();
        assert_eq!(stored, PNG);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&path.parent().unwrap());
    }

    #[test]
    fn a_docx_whose_images_fail_still_imports_its_text() {
        let _f = XdgFixture::new();
        sn_core::paths::ensure_directories().unwrap();
        let notes = NotesStore::open().unwrap();
        let xml = br#"<w:document xmlns:w="http://x" xmlns:a="http://a"><w:body>
<w:p><w:r><w:t>Text survives.</w:t></w:r></w:p>
<w:p><w:r><w:drawing><a:blip r:embed="rId1"/></w:drawing></w:r></w:p>
</w:body></w:document>"#;
        let rels = br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/pic.png"/></Relationships>"#;
        // The media member is NOT in the archive: the rels point at nothing.
        let bytes = docx(&[
            ("word/document.xml", xml),
            ("word/_rels/document.xml.rels", rels),
        ]);
        let dir = std::env::temp_dir().join(format!("import-docx2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.docx");
        std::fs::write(&path, &bytes).unwrap();

        let report = import_docx(&notes, &path, None).unwrap();
        assert_eq!(report.notes_imported, 1, "the text still lands");
        assert_eq!(report.images_stored, 0);
        let note = &notes.active()[0];
        assert!(note.text.contains("Text survives."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_jex_round_trips_through_the_stores_with_its_notebook_and_images() {
        let _f = XdgFixture::new();
        sn_core::paths::ensure_directories().unwrap();

        // Build an archive the way the exporter does, from a scratch library.
        {
            let source_notes = NotesStore::open().unwrap();
            let source_notebooks = NotebooksStore::open().unwrap();
            let nb = source_notebooks.create("Joplin import");
            let mut note = Note::new(Some(nb.id.clone()));
            note.id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into();
            note.text =
                "Body with a picture.\n\n![pic](11111111-1111-1111-1111-111111111111.png)".into();
            source_notes.insert(note);
            let image_note = source_notes.active()[0].clone();
            let export = ExportNote {
                note: &image_note,
                images: vec![(
                    jex::joplin_id("11111111-1111-1111-1111-111111111111"),
                    "png".into(),
                    "image/png".into(),
                    PNG.to_vec(),
                )],
            };
            let bytes = jex::build_archive(&[export], &source_notebooks.all().as_slice());
            let dir = std::env::temp_dir().join(format!("import-jex-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("export.jex");
            std::fs::write(&path, &bytes).unwrap();

            // Import into a FRESH library.
            drop(source_notes);
            drop(source_notebooks);
            // Clear the files the source library wrote so the import sees a
            // fresh library (the fixture's XDG root is per-test, but the
            // stores above wrote into it).
            std::fs::remove_file(sn_core::paths::notes_file()).unwrap();
            std::fs::remove_file(sn_core::paths::notebooks_file()).unwrap();

            let notes = NotesStore::open().unwrap();
            let notebooks = NotebooksStore::open().unwrap();
            let report = import_jex(&notes, &notebooks, &path).unwrap();
            assert_eq!(report.notes_imported, 1, "{report:?}");
            assert_eq!(report.notebooks_created, 1, "{report:?}");
            assert_eq!(report.images_stored, 1, "{report:?}");
            assert!(report.is_ok(), "{:?}", report.errors);

            let imported = &notes.active()[0];
            assert!(imported.text.contains("Body with a picture."));
            // The JEX resource reference became the app scheme, and the
            // image file exists under the note's directory.
            assert!(
                imported.text.contains("speechnotes://note-image/"),
                "{}",
                imported.text
            );
            let nb = &notebooks.all()[0];
            assert_eq!(nb.name, "Joplin import");
            let image_dir = sn_core::paths::note_images_dir().join(&imported.id);
            assert_eq!(std::fs::read_dir(&image_dir).unwrap().flatten().count(), 1);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_reimport_updates_in_place_instead_of_duplicating() {
        let _f = XdgFixture::new();
        sn_core::paths::ensure_directories().unwrap();
        let notes = NotesStore::open().unwrap();
        let notebooks = NotebooksStore::open().unwrap();

        let mut note = Note::new(None);
        note.id = "cccccccc-1111-2222-3333-444444444444".into();
        note.text = "Version one.".into();
        notes.insert(note);
        let note = &notes.active()[0];
        let export = ExportNote {
            note,
            images: Vec::new(),
        };
        let bytes = jex::build_archive(&[export], &[]);
        let dir = std::env::temp_dir().join(format!("import-jex2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("update.jex");
        std::fs::write(&path, &bytes).unwrap();

        let report = import_jex(&notes, &notebooks, &path).unwrap();
        // The archive's copy is not NEWER than the library's (both carry
        // effectively-now timestamps), so the merge rule keeps the local
        // one — and in no case is a duplicate created.
        assert_eq!(notes.len(), 1, "no duplicate was created");
        assert_eq!(notes.active()[0].text, "Version one.");
        let _ = report;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_import_matches_a_library_id_that_differs_only_in_case() {
        let _f = XdgFixture::new();
        sn_core::paths::ensure_directories().unwrap();
        let notes = NotesStore::open().unwrap();
        let notebooks = NotebooksStore::open().unwrap();

        // The library's copy is lower case — an older import or a foreign
        // file. The archive spells the same hex upper case, the form a
        // current export produces.
        let mut local = Note::new(None);
        local.id = "cccccccc-1111-2222-3333-444444444444".to_lowercase();
        local.text = "Local words.".into();
        local.updated_at = sn_core::AppleDate::now().plus_days(-1);
        notes.insert(local);

        let mut archived = Note::new(None);
        archived.id = "CCCCCCCC-1111-2222-3333-444444444444".into();
        archived.text = "Archived words.".into();
        let export = ExportNote {
            note: &archived,
            images: Vec::new(),
        };
        let bytes = jex::build_archive(&[export], &[]);
        let dir = std::env::temp_dir().join(format!("import-jex4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("case.jex");
        std::fs::write(&path, &bytes).unwrap();

        let report = import_jex(&notes, &notebooks, &path).unwrap();
        assert_eq!(report.notes_imported, 1, "{report:?}");
        assert_eq!(notes.len(), 1, "a case spelling difference is not a new note");
        assert_eq!(notes.active()[0].text, "Archived words.");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_jex_is_refused_by_name() {
        let _f = XdgFixture::new();
        sn_core::paths::ensure_directories().unwrap();
        let notes = NotesStore::open().unwrap();
        let notebooks = NotebooksStore::open().unwrap();
        let dir = std::env::temp_dir().join(format!("import-jex3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-a.jex");
        std::fs::write(&path, b"this is not a tar").unwrap();
        let err = import_jex(&notes, &notebooks, &path).unwrap_err();
        assert!(err.contains("not a JEX"), "{err}");
        assert!(notes.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_docx_that_is_not_a_zip_is_refused_with_the_reason() {
        let _f = XdgFixture::new();
        sn_core::paths::ensure_directories().unwrap();
        let notes = NotesStore::open().unwrap();
        let dir = std::env::temp_dir().join(format!("import-docx3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plain.docx");
        std::fs::write(&path, b"plain text, not a zip").unwrap();
        let err = import_docx(&notes, &path, None).unwrap_err();
        assert!(!err.is_empty());
        assert!(notes.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
