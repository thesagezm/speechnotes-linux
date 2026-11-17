//! Seed a (scratch) library with a slide deck from the command line — the
//! same `import_slides` the app's "Import a book" button calls, so an
//! isolated gui-drive instance can have books without driving the native
//! file dialog. Honours `XDG_DATA_HOME` like the app does.
//!
//!     XDG_DATA_HOME=/tmp/scratch/xdg cargo run -p sn-store --example import_deck -- deck.pptx
fn main() {
    for arg in std::env::args().skip(1) {
        let path = std::path::Path::new(&arg);
        match sn_store::books::BooksStore::import_slides(path) {
            Ok(book) => println!(
                "imported {}: {} slides, {}",
                book.manifest.title,
                book.manifest.spine_count,
                book.manifest.id
            ),
            Err(e) => {
                eprintln!("failed: {arg}: {e}");
                std::process::exit(1);
            }
        }
    }
}
