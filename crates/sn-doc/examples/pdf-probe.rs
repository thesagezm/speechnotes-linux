//! Probe a PDF page's structure: fonts, resources, contents shape, and what
//! each engine reads. Diagnostic, read-only.
//!
//! `cargo run -p sn-doc --example pdf-probe -- <pdf> <1-based-page>`

use sn_doc::pdf::{Document, Obj};

fn obj_name(doc: &Document, o: Option<&Obj>) -> String {
    match o.and_then(|o| o.as_dict(doc)) {
        Some(_) => "dict".to_string(),
        None => match o {
            Some(Obj::Name(n)) => format!("/{n}"),
            Some(Obj::Ref(id)) => format!("ref {id}"),
            Some(_) => "other".to_string(),
            None => "-".to_string(),
        },
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_default();
    let page_no: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let bytes = std::fs::read(&path).expect("read pdf");
    let doc = Document::parse(&bytes).expect("parse pdf");

    let pages = doc.pages();
    let Some((id, page)) = pages.get(page_no - 1) else {
        println!("no page {page_no}");
        return;
    };
    println!("page {} obj {}", page_no, id);
    println!("page dict keys: {:?}", {
        let mut k: Vec<_> = page.keys().cloned().collect();
        k.sort();
        k
    });
    println!("page /Resources: {}", obj_name(&doc, page.get("Resources")));
    let mut parent = doc
        .lookup(page, "Parent")
        .and_then(|o| o.as_dict(&doc));
    let mut level = 0;
    while let Some(p) = parent {
        level += 1;
        let has = p.contains_key("Resources");
        println!("  parent level {level}: has /Resources = {has}");
        parent = doc.lookup(p, "Parent").and_then(|o| o.as_dict(&doc));
    }
    match page.get("Contents") {
        Some(Obj::Ref(r)) => println!("/Contents = ref {}", r),
        Some(Obj::Array(items)) => println!("/Contents = array of {} refs", items.len()),
        Some(_) => println!("/Contents = inline stream"),
        None => println!("/Contents = none"),
    }
    if let Some(res) = doc.dict_of(page, "Resources") {
        if let Some(fonts) = doc.dict_of(res, "Font") {
            for (name, value) in fonts {
                let Some(dict) = value.as_dict(&doc) else {
                    println!("font {name}: not a dict");
                    continue;
                };
                let subtype = doc.name_of(dict, "Subtype").unwrap_or("-");
                let base = doc.name_of(dict, "BaseFont").unwrap_or("-");
                let tu = doc.lookup(dict, "ToUnicode").is_some();
                let enc = obj_name(&doc, doc.lookup(dict, "Encoding"));
                let has_diff = doc
                    .lookup(dict, "Encoding")
                    .and_then(|o| o.as_dict(&doc))
                    .map(|d| d.contains_key("Differences"))
                    .unwrap_or(false);
                let df = doc.lookup(dict, "DescendantFonts").is_some();
                println!(
                    "font {name}: {subtype} base={base} ToUnicode={tu} Encoding={enc} Differences={has_diff} DescendantFonts={df}"
                );
            }
        } else {
            println!("resources carry no /Font");
        }
        if let Some(xo) = doc.dict_of(res, "XObject") {
            println!("XObject entries: {}", xo.len());
            for (name, value) in xo.iter().take(12) {
                if let Some(dict) = value.as_dict(&doc) {
                    println!("  /{name}: {}", doc.name_of(dict, "Subtype").unwrap_or("-"));
                }
            }
        } else {
            println!("no /XObject in resources");
        }
    }
    let chars_engine = sn_doc::pdftext::extract_chars(&doc, page_no - 1);
    let chars_interp = sn_doc::pdftext::extract_chars_interpreted(&doc, page_no - 1);
    println!(
        "chars: engine-pick={} interpreted={}",
        chars_engine.len(),
        chars_interp.len()
    );
    let text = sn_doc::pdftext::page_text_from_chars(&doc, page_no, chars_interp);
    println!(
        "interpreted page text ({} chars):\n{}",
        text.chars().count(),
        text
    );
}
