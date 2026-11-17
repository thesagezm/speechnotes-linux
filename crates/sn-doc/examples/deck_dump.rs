//! Dump a slide deck's parsed geometry — what the viewer lays out — so a
//! render bug can be compared against the file without the GUI.
//!
//!     cargo run -p sn-doc --example deck_dump -- deck.pptx [slide-index]
fn main() {
    let path = std::env::args().nth(1).expect("usage: deck_dump FILE [SLIDE]");
    let bytes = std::fs::read(&path).expect("read");
    let deck = sn_doc::slides::SlideDeck::parse(&bytes).expect("parse");
    println!(
        "deck {}x{} pt, {} slides, {} media",
        deck.w,
        deck.h,
        deck.slides.len(),
        deck.media.len()
    );
    let want: Option<usize> = std::env::args().nth(2).and_then(|s| s.parse().ok());
    for (i, slide) in deck.slides.iter().enumerate() {
        if let Some(w) = want {
            if i != w {
                continue;
            }
        }
        println!(
            "--- slide {} ({}) bg={:?} items={} spans={}",
            i,
            slide.title.trim(),
            slide.bg,
            slide.items.len(),
            slide.spans.len()
        );
        for (j, item) in slide.items.iter().enumerate() {
            let kind = match &item.content {
                sn_doc::slides::Content::Text { paras, .. } => {
                    let n: usize = paras.iter().map(|p| p.runs.len()).sum();
                    let sizes: Vec<String> = paras
                        .iter()
                        .flat_map(|p| p.runs.iter())
                        .filter_map(|r| {
                            if r.size > 0.0 {
                                Some(format!("{}", r.size))
                            } else {
                                None
                            }
                        })
                        .take(3)
                        .collect();
                    format!("text paras={} runs={n} sizes={sizes:?}", paras.len())
                }
                sn_doc::slides::Content::Picture { media, crop, .. } => {
                    format!("picture media={media} crop={crop:?}")
                }
                sn_doc::slides::Content::Table { rows, .. } => {
                    format!("table rows={}", rows.len())
                }
                sn_doc::slides::Content::Chart { .. } => "chart".to_string(),
                sn_doc::slides::Content::Frame { label, media } => {
                    format!("frame label={label:?} media={media:?}")
                }
            };
            println!(
                "  item {j}: x={} y={} w={} h={} rot={} {kind}",
                item.x, item.y, item.w, item.h, item.rot
            );
            if let sn_doc::slides::Content::Text { paras, .. } = &item.content {
                for (k, p) in paras.iter().enumerate().take(4) {
                    let text: String =
                        p.runs.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("");
                    println!(
                        "    para {k}: align={} line_pts={} ml={} {:?}",
                        p.align, p.line_pts, p.margin_left, text
                    );
                }
            }
        }
    }
}
