//! `sn-store` — persistence, byte-compatible with the other Speechnotes apps.
//!
//! The models first; the stores that own them follow. Everything here is
//! headlessly testable, which is what makes a store bug a test failure rather
//! than a lost library.

pub mod books;
pub mod imports;
pub mod jex;
pub mod model;
//  images.rs (original 20261027T185702) postdates this lib.rs original, and
// imports.rs calls crate::images::store — module re-added by the sweep.
pub mod images;
pub mod notes;
pub mod query;
pub mod stores;

pub use model::{Note, Notebook, PlaybackBookmark, Scope, SortOrder};
pub use notes::{LoadReport, NotesStore, TEXT_SAVE_DEBOUNCE};
pub use query::{sections, Query, SearchIndex, Section};
pub use stores::{
    BookmarkStore, NotebooksStore, Prefs, PrefsStore, BOOKMARK_CAP, BOOKMARK_MIN_INTERVAL,
    PREFS_DEBOUNCE,
};
// User-reported (2026-10-30): the jex module was public but its
// types were not re-exported at the root, so `sn_store::ExportNote` (the form
// sibling crates reach for) did not resolve — only `sn_store::jex::ExportNote`
// did. Re-export the JEX interchange surface alongside the other stores'.
pub use jex::{
    build_archive, parse_archive, Archive, ExportNote, ImportedNote, ImportedNotebook,
    ImportedResource, JexError,
};
