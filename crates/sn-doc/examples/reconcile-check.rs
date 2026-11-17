//! Check the reader's reconciliation of a real page: the text layer, the
//! recognition, and the one page text the reader will speak — with the
//! word-level duplication the old line-append merge produced spelled out.
//!
//! Read-only on the book directory: nothing is written back.
//!
//! `cargo run -p sn-doc --example reconcile-check -- <book-dir> <page>`

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = args.get(1).cloned().unwrap_or_default();
    let page: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(388);
    let pdf_path = std::path::Path::new(&dir).join("original.pdf");
    let bytes = std::fs::read(&pdf_path).expect("read original.pdf");
    let doc = sn_doc::pdf::Document::parse(&bytes).expect("parse pdf");
    let from = page.saturating_sub(2);
    let to = page + 3;
    let pages = sn_doc::pdftext::extract_pages(&doc, from, to);
    let Some(p) = pages.iter().find(|p| p.page == page).or_else(|| pages.first()) else {
        println!("no page text");
        return;
    };
    let mut layer = p.text.clone();
    // The same second opinion the read path takes: a junk layer that the
    // interpreter read cleanly is replaced before the reconciliation.
    let garbage = sn_doc::textquality::layer_needs_ocr(&layer);
    if garbage && sn_doc::pdftext::pdfium_is_the_engine(&doc) {
        let alt = sn_doc::pdftext::page_text_from_chars(
            &doc,
            page - 1,
            sn_doc::pdftext::extract_chars_interpreted(&doc, page - 1),
        );
        if !sn_doc::textquality::layer_needs_ocr(&alt) {
            layer = alt;
        }
    }
    let cache = std::path::Path::new(&dir).join(format!("text/{:04}.txt", page));
    let ocr = std::fs::read_to_string(&cache).unwrap_or_default();

    println!("=== text layer ({} chars) ===\n{layer}\n", layer.chars().count());
    println!("=== recognition ({} chars) ===\n{ocr}\n", ocr.chars().count());
    println!(
        "layer: junk={} · reading_beats(layer, ocr)={}",
        garbage,
        sn_doc::textquality::reading_beats(&layer, &ocr)
    );
    let (led, report) = sn_doc::reconcile::reconcile_reported(&layer, &ocr);
    println!(
        "=== reconciled, layer leads ({} chars; {} line(s) appended, {} word(s) spliced) ===\n{led}",
        led.chars().count(),
        report.appended_lines,
        report.spliced_words
    );
    // The Force OCR direction: recognition leads, the layer fills its gaps.
    let (forced, freport) = sn_doc::reconcile::reconcile_reported(&ocr, &layer);
    println!(
        "=== reconciled, recognition leads / Force OCR ({} chars; {} line(s) appended, {} word(s) spliced) ===\n{forced}",
        forced.chars().count(),
        freport.appended_lines,
        freport.spliced_words
    );
}