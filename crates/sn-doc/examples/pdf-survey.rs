//! Extract a page range's text-layer prose the way the reader does, for
//! A/B against poppler. Diagnostic, read-only.
//!
//! `cargo run -p sn-doc --example pdf-survey -- <pdf> <first> <last>`

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_default();
    let first: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let last: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(first);
    let bytes = std::fs::read(&path).expect("read pdf");
    let doc = sn_doc::pdf::Document::parse(&bytes).expect("parse pdf");
    let pages = sn_doc::pdftext::extract_pages(&doc, first - 1, last);
    for p in pages {
        println!("===== page {} =====", p.page);
        println!("{}", p.text);
    }
}
