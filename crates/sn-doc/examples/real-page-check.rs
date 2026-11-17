//! Check the reader's text-layer/recognition decision on a real book.
//!
//! `cargo run -p sn-doc --example real-page-check -- <book-dir> <page>`

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = args.get(1).cloned().unwrap_or_default();
    let page: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(965);
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
    let layer = p.text.clone();
    let cache = std::path::Path::new(&dir)
        .join(format!("text/{:04}.txt", page));
    let ocr = std::fs::read_to_string(&cache).unwrap_or_default();

    println!("=== text layer (what the PDF itself carries) ===\n{layer}\n");
    println!("=== recognition (the OCR cache) ===\n{ocr}\n");
    let beats = sn_doc::textquality::reading_beats(&layer, &ocr);
    println!("reading_beats(layer, ocr) = {beats}");
    let final_text = if beats { ocr.clone() } else { layer.clone() };
    println!("=== what the reader will now speak ({} chars) ===\n{final_text}", final_text.chars().count());
}
