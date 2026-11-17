//! Time the book-open path phase by phase against a (scratch) library —
//! the same calls the reader's `open_book_worker` makes, so the loading-time
//! pass can see where a PDF/EPUB/deck open spends its time. Honours
//! `XDG_DATA_HOME` like the app does.
//!
//!     XDG_DATA_HOME=/tmp/sn-beta-data cargo run -p sn-store --release \
//!         --example open_profile -- B02D9B93 107
fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let prefix = args.remove(0);
    let chapter: Option<usize> = args.first().and_then(|c| c.parse().ok());

    let t = std::time::Instant::now();
    let book = sn_store::books::BooksStore::find(&prefix)
        .expect("find failed")
        .unwrap_or_else(|| panic!("no book matches {prefix}"));
    println!(
        "find (full library scan + manifests)        {:>8.1} ms  ({} books, format {})",
        t.elapsed().as_secs_f64() * 1e3,
        sn_store::books::BooksStore::all().map(|b| b.len()).unwrap_or(0),
        book.manifest.format,
    );

    let index = chapter.unwrap_or(0);
    let t = std::time::Instant::now();
    let text = book
        .chapter_text(index)
        .unwrap_or_else(|e| panic!("chapter_text failed: {e}"));
    println!(
        "chapter_text({index})                        {:>8.1} ms  ({} chars)",
        t.elapsed().as_secs_f64() * 1e3,
        text.chars().count(),
    );

    if book.manifest.format == "pdf" {
        let t = std::time::Instant::now();
        let sizes = book.page_sizes().ok();
        println!(
            "page_sizes (whole-PDF page boxes)           {:>8.1} ms  ({} pages)",
            t.elapsed().as_secs_f64() * 1e3,
            sizes.map(|s| s.len()).unwrap_or(0),
        );
        // A second page, cold-ish: shows whether the doc parse amortises.
        let t = std::time::Instant::now();
        let _ = book.chapter_text(index + 1);
        println!(
            "chapter_text({}) (next page)              {:>8.1} ms",
            index + 1,
            t.elapsed().as_secs_f64() * 1e3,
        );
    }

    if book.is_slides() {
        let t = std::time::Instant::now();
        let _ = book
            .chapter_full(index)
            .map(|c| c.text)
            .unwrap_or_default();
        println!(
            "chapter_full({index}) (slide text+pictures)  {:>8.1} ms",
            t.elapsed().as_secs_f64() * 1e3,
        );
    }
}
