//! `sn-ui` — the GTK4 layer.
//!
//! Everything that touches a display server lives here; the stores, the text
//! pipeline and (later) the audio path do not, which is what makes them
//! testable headlessly.

pub mod a11y;
pub mod abook;
pub mod app;
pub mod aspect_image;
pub mod assets;
pub mod bookdrop;
pub mod bookview;
pub mod dialogs;
pub mod epubview;
pub mod export;
pub mod format;
pub mod icons;
pub mod imageview;
pub mod images;
pub mod imgcache;
pub mod motion;
pub mod pdfview;
pub mod picker;
pub mod player;
pub mod pomodoro;
pub mod reader;
pub mod render;
pub mod settings;
pub mod slash;
//  speak-from-selection — authored module, see the
// module header for the design and the board for the round-4 brief.
pub mod speaksel;
pub mod slideview;
pub mod tasks;
pub mod theme;
//  `webview.rs` (the URI-scheme/settings fence) exists as an original
// original dated 2026-10-27T19:25, after this lib.rs original (02:21) was
// captured; the final lib.rs must have declared it.
pub mod webview;
pub mod window;
