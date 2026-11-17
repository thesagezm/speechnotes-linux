//! OCR probe: run one page image through the RapidOCR pipeline and print
//! every recognised line with its confidence, plus the detection log at
//! debug level. The quality-tuning loop's measuring stick:
//!
//! ```text
//! RUST_LOG=debug cargo run -p sn-doc --example ocr-dump -- page.png
//! ```
//!
//! `--plain` prints the `join_lines` shape the reader's fallback takes,
//! `--layout` (default) prints the papero layout text the reader prefers.

fn main() {
    env_logger::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.iter().find(|a| !a.starts_with('-')) else {
        eprintln!("usage: ocr-dump [--plain] PAGE.png");
        std::process::exit(2);
    };
    let png = std::fs::read(path).expect("read page image");
    if !sn_doc::rapidocr::available() {
        eprintln!("rapidocr not available (dylib + models must be installed)");
        std::process::exit(1);
    }
    let lines = sn_doc::rapidocr::recognize(&png).expect("recognise");
    println!("--- {} lines ---", lines.len());
    for l in &lines {
        println!(
            "  {:4.2} [{:5.0},{:5.0} {:5.0},{:5.0}] {}",
            l.score, l.x0, l.y0, l.x1, l.y1, l.text
        );
    }
    println!("--- layout text ---");
    let text = if args.iter().any(|a| a == "--plain") {
        sn_doc::rapidocr::page_text(&png).expect("page text")
    } else {
        sn_doc::rapidocr::page_image_text(&png).expect("page text")
    };
    println!("{}", text);
}
