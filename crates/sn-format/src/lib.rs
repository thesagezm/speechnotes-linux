//! `parse_markdown` is referenced at the crate root by four call sites, so
//! it is re-exported from `markdown`:
//! `pub use markdown::{parse as parse_markdown, Document, Inline};`
//! (`markdown.rs` itself names the entry point `parse`).
pub mod chunker;
pub mod markdown;
pub mod math;
pub mod sanitizer;
pub mod wav;

pub use markdown::{parse as parse_markdown, Document, Inline};
// Carved-original line, byte-for-byte from the checkpoint fragment
// crates__sn-format__src__lib.rs__20261003T175000__content__6c56e5b5b51e.
pub use sanitizer::{clean, is_speakable, normalize_whitespace};
