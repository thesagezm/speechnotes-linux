//! Throwaway diagnostic: the link table for one real PDF.

fn main() {
    let path = std::env::args().nth(1).expect("a pdf path");
    let bytes = std::fs::read(&path).expect("read");
    let doc = sn_doc::pdf::Document::parse(&bytes).expect("parse");
    let sizes = doc.page_sizes();
    println!("page 359 size: {:?}", sizes.get(358));
    let links = doc.page_links();
    let on359: Vec<_> = links
        .iter()
        .filter(|l| l.page == 359 && matches!(l.target, sn_doc::pdf::PageLinkTarget::Page(_)))
        .collect();
    println!("goto links on page 359: {}", on359.len());
    for l in &on359 {
        let sn_doc::pdf::PageLinkTarget::Page(n) = l.target else { continue };
        println!(
            "  rect ({:.1},{:.1},{:.1},{:.1}) -> page {n}",
            l.rect.0, l.rect.1, l.rect.2, l.rect.3
        );
    }
}
