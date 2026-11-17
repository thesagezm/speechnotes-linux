//! Dump one font's ToUnicode CMap and the page's content-stream head, to see
//! exactly what the interpreter is asked to decode. Diagnostic, read-only.
//!
//! `cargo run -p sn-doc --example pdf-debug -- <pdf> <1-based-page> [fontname]`

use sn_doc::pdf::{Document, Obj};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_default();
    let page_no: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let bytes = std::fs::read(&path).expect("read pdf");
    let doc = Document::parse(&bytes).expect("parse pdf");
    if args.get(3).map(|s| s == "chars").unwrap_or(false) {
        let chars = sn_doc::pdftext::extract_chars(&doc, page_no - 1);
        for c in &chars {
            println!(
                "run {:4} '{}' x {:.1}..{:.1} y {:.1}..{:.1} size {:.1} space={}",
                c.run,
                c.c.escape_default(),
                c.x0,
                c.x1,
                c.y0,
                c.y1,
                c.size,
                c.space
            );
        }
        return;
    }
    let pages = doc.pages();
    let Some((_, page)) = pages.get(page_no - 1) else {
        println!("no page {page_no}");
        return;
    };
    let res = doc.dict_of(page, "Resources").expect("resources");
    let fonts = doc.dict_of(res, "Font").expect("fonts");
    let only = args.get(3).map(|s| s.trim_start_matches('/').to_string());
    for (name, value) in fonts {
        if let Some(sel) = &only {
            if name != sel {
                continue;
            }
        }
        let Some(dict) = value.as_dict(&doc) else { continue };
        println!("== font /{name}");
        if let Some(tu) = doc.lookup(dict, "ToUnicode") {
            match doc.stream_data(tu) {
                Some(bytes) => {
                    let s = String::from_utf8_lossy(&bytes);
                    println!("--- ToUnicode ({} bytes) ---", bytes.len());
                    println!("{}", &s[..s.len().min(3000)]);
                }
                None => println!("--- ToUnicode: stream_data returned NONE"),
            }
        }
        // The descendant font's /W widths head.
        if let Some(df) = doc.array_of(dict, "DescendantFonts") {
            if let Some(Obj::Ref(id)) = df.first() {
                if let Some(d) = doc.get(*id).and_then(|o| o.as_dict(&doc)) {
                    if let Some(w) = doc.array_of(d, "W") {
                        println!("--- /W ({} entries): {:?}", w.len(), {
                            let head: Vec<String> =
                                w.iter().take(10).map(|o| format!("{o:?}")).collect();
                            head
                        });
                    }
                    println!(
                        "descendant keys: {:?}",
                        { let mut k: Vec<_> = d.keys().cloned().collect(); k.sort(); k }
                    );
                    if let Some(cid2gid) = doc.lookup(d, "CIDToGIDMap") {
                        println!("CIDToGIDMap: {cid2gid:?}");
                    }
                }
            }
        }
    }
    // The content stream head.
    match page.get("Contents") {
        Some(Obj::Ref(id)) => {
            if let Some(obj) = doc.get(*id) {
                if let Some(data) = doc.stream_data(obj) {
                    println!("--- content stream ({} bytes), head ---", data.len());
                    let head = String::from_utf8_lossy(&data[..data.len().min(2400)]);
                    println!("{head}");
                }
            }
        }
        _ => println!("contents: not a single ref"),
    }
}
