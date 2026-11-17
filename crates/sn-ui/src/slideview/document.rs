//! Split out of slideview.rs 2026-10-31 (pure code-motion).

use std::collections::HashMap;

use sn_doc::slides::{Anchor, Content, Item, Slide, SlideDeck};

use super::{HOST, SCHEME};
use super::script::DECK_CSS;

// ---------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------

/// Build the whole deck as one document: a section per slide over a stage
/// sized in points, scaled to fit by the script.
pub(crate) fn deck_html(deck: &SlideDeck) -> String {
    let mut out = String::with_capacity(48 * 1024);
    out.push_str("<!DOCTYPE html><html data-w=\"");
    out.push_str(&num(deck.w));
    out.push_str("\" data-h=\"");
    out.push_str(&num(deck.h));
    out.push_str("\"><head><meta charset=\"utf-8\"><style>");
    out.push_str(":root{--dw:");
    out.push_str(&num(deck.w));
    out.push_str(";--dh:");
    out.push_str(&num(deck.h));
    out.push_str(";}");
    out.push_str(DECK_CSS);
    out.push_str("</style></head><body><main class=\"deck\">");
    for (index, slide) in deck.slides.iter().enumerate() {
        out.push_str(&slide_html(deck, index, slide));
    }
    out.push_str("</main></body></html>");
    out
}

/// One slide: its stage and its items in z-order (document order).
pub(crate) fn slide_html(deck: &SlideDeck, index: usize, slide: &Slide) -> String {
    // The highlight targets: (item, para) → byte range for text paragraphs,
    // and per-item byte ranges for the whole-item shapes (a picture's
    // recognised lines, a table's rows) the paragraph form cannot address.
    let mut para_spans: HashMap<(usize, usize), (usize, usize)> = HashMap::new();
    let mut item_spans: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
    for span in &slide.spans {
        match span.para {
            Some(para) => {
                para_spans.insert((span.item, para), (span.start, span.end));
            }
            None => {
                item_spans
                    .entry(span.item)
                    .or_default()
                    .push((span.start, span.end));
            }
        }
    }

    let mut out = String::new();
    out.push_str("<section class=\"slide\" id=\"s");
    out.push_str(&index.to_string());
    out.push_str("\"><div class=\"stage\" style=\"background:");
    out.push_str(slide.bg.as_deref().unwrap_or("#FFFFFF"));
    out.push_str(";\">");
    for (i, item) in slide.items.iter().enumerate() {
        out.push_str(&item_html(deck, i, item, &para_spans, item_spans.get(&i)));
    }
    out.push_str("</div></section>");
    out
}

/// One item: an absolutely-positioned box in points, with the shape's paint,
/// border, shadow and geometry, and its content inside.
pub(crate) fn item_html(
    deck: &SlideDeck,
    index: usize,
    item: &Item,
    para_spans: &HashMap<(usize, usize), (usize, usize)>,
    item_spans: Option<&Vec<(usize, usize)>>,
) -> String {
    let mut style = format!(
        "left:{}pt;top:{}pt;width:{}pt;height:{}pt",
        num(item.x),
        num(item.y),
        num(item.w),
        num(item.h)
    );
    // Rotation is clockwise in both PowerPoint's and CSS's books. A flip
    // mirrors the shape; PowerPoint keeps a text box's words readable
    // through a flip, so the text body counter-scales below.
    let mut transform: Vec<String> = Vec::new();
    if item.rot != 0.0 {
        transform.push(format!("rotate({}deg)", num(item.rot)));
    }
    let (sx, sy) = (
        if item.flip_h { -1.0 } else { 1.0 },
        if item.flip_v { -1.0 } else { 1.0 },
    );
    if item.flip_h || item.flip_v {
        transform.push(format!("scale({},{})", num(sx), num(sy)));
    }
    if !transform.is_empty() {
        style.push_str(";transform:");
        style.push_str(&transform.join(" "));
    }
    if let Some(fill) = &item.fill {
        style.push_str(";background:");
        style.push_str(fill);
    }
    if let Some(border) = &item.border {
        style.push_str(&format!(
            ";border:{}pt {} {}",
            num(border.width),
            border.style,
            border.color
        ));
    }
    if item.radius > 0.0 {
        style.push_str(&format!(";border-radius:{}pt", num(item.radius)));
    }
    if let Some(shadow) = &item.shadow {
        style.push_str(";box-shadow:");
        style.push_str(shadow);
    }

    let mut out = format!("<div class=\"item\" id=\"i{index}\" style=\"{style}\">");
    // A picture's recognised text has no paragraph to paint on: one
    // transparent overlay per line, which tints the picture while its text
    // is being spoken. (A table's whole-item spans go on its rows instead —
    // an overlay here would tint the whole table for one spoken row.)
    if matches!(item.content, Content::Picture { .. }) {
        if let Some(spans) = item_spans {
            for (start, end) in spans {
                out.push_str(&format!(
                    "<span class=\"hit\" data-s=\"{start}\" data-e=\"{end}\"></span>"
                ));
            }
        }
    }
    if let Some(svg) = &item.svg {
        out.push_str(&format!(
            "<svg class=\"shape\" viewBox=\"0 0 {} {}\" preserveAspectRatio=\"none\">\
             <path d=\"{}\" fill=\"{}\" stroke=\"{}\" stroke-width=\"{}\"/></svg>",
            num(item.w),
            num(item.h),
            escape_html(&svg.d),
            svg.fill.as_deref().unwrap_or("none"),
            svg.stroke.as_deref().unwrap_or("none"),
            num(svg.stroke_width),
        ));
    }
    out.push_str(&content_html(deck, index, item, para_spans, item_spans));
    out.push_str("</div>");
    out
}

/// The item's content: text, picture, table, chart or frame.
pub(crate) fn content_html(
    deck: &SlideDeck,
    index: usize,
    item: &Item,
    para_spans: &HashMap<(usize, usize), (usize, usize)>,
    item_spans: Option<&Vec<(usize, usize)>>,
) -> String {
    match &item.content {
        Content::Text {
            paras,
            anchor,
            wrap,
            dir,
        } => {
            let mut style: Vec<String> = Vec::new();
            style.push(
                match anchor {
                    Anchor::Top => "justify-content:flex-start",
                    Anchor::Middle => "justify-content:center",
                    Anchor::Bottom => "justify-content:flex-end",
                }
                .to_string(),
            );
            // The standing directions, approximated with the writing modes
            // WebKitGTK has: `vert270` (bottom-to-top) as a vertical run
            // turned upside down, `stacked` as upright letters.
            let mut transform: Vec<String> = Vec::new();
            match dir.as_str() {
                "vert" => style.push("writing-mode:vertical-rl".to_string()),
                "vert270" => {
                    style.push("writing-mode:vertical-rl".to_string());
                    transform.push("rotate(180deg)".to_string());
                }
                "stacked" => style
                    .push("writing-mode:vertical-rl;text-orientation:upright".to_string()),
                _ => {}
            }
            // PowerPoint keeps flipped text readable: un-mirror it. One
            // transform property, whatever the direction asked for too.
            if item.flip_h || item.flip_v {
                transform.push(format!(
                    "scale({},{})",
                    num(if item.flip_h { -1.0 } else { 1.0 }),
                    num(if item.flip_v { -1.0 } else { 1.0 })
                ));
            }
            if !transform.is_empty() {
                style.push(format!("transform:{}", transform.join(" ")));
            }
            let mut out = format!(
                "<div class=\"tb\" style=\"{};white-space:{}\">",
                style.join(";"),
                if *wrap { "pre-wrap" } else { "nowrap" }
            );
            for (para, p) in paras.iter().enumerate() {
                out.push_str(&para_html(deck, index, para, p, para_spans));
            }
            out.push_str("</div>");
            out
        }
        Content::Picture { media, crop, .. } => picture_html(deck, *media, crop, item),
        Content::Table {
            col_widths,
            row_heights,
            rows,
            bg,
        } => table_html(col_widths, row_heights, rows, bg.as_deref(), item_spans),
        Content::Chart { svg } => {
            format!("<div class=\"chart\">{svg}</div>")
        }
        Content::Frame { label, media } => {
            let mut out = String::from("<div class=\"frame\">");
            if let Some(media) = media {
                out.push_str(&poster_img(deck, *media));
            }
            if !label.trim().is_empty() {
                out.push_str(&format!(
                    "<div class=\"frame-label\">{}</div>",
                    escape_html(label)
                ));
            }
            out.push_str("</div>");
            out
        }
    }
}

/// A picture, with the deck's crop honoured: the crop fractions inset the
/// image inside its frame, so the frame shows exactly the cropped region.
pub(crate) fn picture_html(deck: &SlideDeck, media: usize, crop: &[f64; 4], item: &Item) -> String {
    let Some(src) = media_src(deck, media) else {
        return String::new();
    };
    let [l, t, r, b] = *crop;
    if l <= 0.0 && t <= 0.0 && r <= 0.0 && b <= 0.0 {
        return format!("<img class=\"pic\" src=\"{src}\" alt=\"\">");
    }
    // The visible fraction of each axis: the image is blown up by
    // 1/(1−crops) and slid so the cropped region fills the frame.
    let span_w = (1.0 - l - r).max(0.05);
    let span_h = (1.0 - t - b).max(0.05);
    format!(
        "<span class=\"picwrap\"><img class=\"pic\" src=\"{src}\" alt=\"\" \
         style=\"position:absolute;left:-{}pt;top:-{}pt;width:{}pt;height:{}pt;max-width:none\"></span>",
        num(l * item.w),
        num(t * item.h),
        num(item.w / span_w),
        num(item.h / span_h),
    )
}

/// A table: real rows and cells, the deck's column widths and row heights,
/// one highlight target per row that says something.
pub(crate) fn table_html(
    col_widths: &[f64],
    row_heights: &[f64],
    rows: &[sn_doc::slides::TableRow],
    bg: Option<&str>,
    item_spans: Option<&Vec<(usize, usize)>>,
) -> String {
    let mut out = String::from("<div class=\"tblwrap\"><table style=\"table-layout:fixed;");
    if let Some(bg) = bg {
        out.push_str("background:");
        out.push_str(bg);
    }
    out.push_str("\"><colgroup>");
    for w in col_widths {
        out.push_str(&format!("<col style=\"width:{}pt\">", num(*w)));
    }
    out.push_str("</colgroup>");
    // The speakable rows and the rendered rows line up one-to-one: the text
    // assembler skips the silent rows, and so does the row counter below.
    let mut span_i = 0usize;
    for (ri, row) in rows.iter().enumerate() {
        let mut attrs = String::new();
        let speaks = row
            .cells
            .iter()
            .any(|c| !c.covered && !c.text.trim().is_empty());
        if speaks {
            if let Some(spans) = item_spans {
                if let Some((start, end)) = spans.get(span_i) {
                    attrs = format!(" data-s=\"{start}\" data-e=\"{end}\"");
                }
            }
        }
        out.push_str(&format!(
            "<tr style=\"height:{}pt\"{attrs}>",
            row_heights.get(ri).copied().unwrap_or(0.0).max(0.0),
        ));
        for cell in &row.cells {
            if cell.covered {
                continue;
            }
            let mut attrs = String::new();
            if cell.col_span > 1 {
                attrs.push_str(&format!(" colspan=\"{}\"", cell.col_span));
            }
            if cell.row_span > 1 {
                attrs.push_str(&format!(" rowspan=\"{}\"", cell.row_span));
            }
            let mut style = String::from("text-align:");
            style.push_str(&cell.align);
            if let Some(fill) = &cell.fill {
                style.push_str(";background:");
                style.push_str(fill);
            }
            out.push_str(&format!("<td{attrs} style=\"{style}\">"));
            if cell.runs.is_empty() && !cell.text.is_empty() {
                out.push_str(&escape_html(&cell.text));
            } else {
                out.push_str(&runs_html(&cell.runs));
            }
            out.push_str("</td>");
        }
        out.push_str("</tr>");
        if speaks {
            span_i += 1;
        }
    }
    out.push_str("</table></div>");
    out
}

/// One paragraph of a text body: its own highlight target when it says
/// something, its blank-line height when it does not.
pub(crate) fn para_html(
    deck: &SlideDeck,
    item: usize,
    para: usize,
    p: &sn_doc::slides::Para,
    para_spans: &HashMap<(usize, usize), (usize, usize)>,
) -> String {
    let mut style: Vec<String> = Vec::new();
    if p.runs.iter().all(|r| r.text.trim().is_empty()) {
        // A blank line keeps its place: the height of its end mark.
        if p.empty_size > 0.0 {
            return format!(
                "<p style=\"min-height:{}pt;margin:0\"></p>",
                num(p.empty_size)
            );
        }
        return "<p style=\"margin:0\"></p>".to_string();
    }
    style.push(format!("text-align:{}", p.align));
    style.push(format!(
        "margin:{}pt 0 {}pt 0;margin-left:{}pt;text-indent:{}pt",
        num(p.space_before),
        num(p.space_after),
        num(p.margin_left),
        num(p.indent),
    ));
    if p.line_pts > 0.0 {
        style.push(format!("line-height:{}pt", num(p.line_pts)));
    } else if p.line_pct > 0.0 {
        style.push(format!("line-height:{}", num(p.line_pct)));
    }
    let (id, data) = match para_spans.get(&(item, para)) {
        Some((start, end)) => (
            format!(" id=\"i{item}p{para}\""),
            format!(" data-s=\"{start}\" data-e=\"{end}\""),
        ),
        None => (String::new(), String::new()),
    };
    let mut bullet = String::new();
    if let Some(b) = &p.bullet {
        if let Some(media) = b.media {
            bullet = match media_src(deck, media) {
                Some(src) => format!("<img class=\"blt-img\" src=\"{src}\" alt=\"\">"),
                None => String::new(),
            };
        } else {
            bullet = format!("<span class=\"blt\">{}</span> ", escape_html(&b.char));
        }
    }
    format!(
        "<p{id}{data} style=\"{}\">{bullet}{}</p>",
        style.join(";"),
        runs_html(&p.runs)
    )
}

/// The runs of a paragraph or a cell, each with its own resolved type.
pub(crate) fn runs_html(runs: &[sn_doc::slides::Run]) -> String {
    let mut out = String::new();
    for run in runs {
        if run.text.is_empty() {
            continue;
        }
        let mut style: Vec<String> = Vec::new();
        if run.size > 0.0 {
            style.push(format!("font-size:{}pt", num(run.size)));
        }
        if run.bold {
            style.push("font-weight:bold".to_string());
        }
        if run.italic {
            style.push("font-style:italic".to_string());
        }
        let mut deco: Vec<&str> = Vec::new();
        if run.underline {
            deco.push("underline");
        }
        if run.strike {
            deco.push("line-through");
        }
        if !deco.is_empty() {
            style.push(format!("text-decoration:{}", deco.join(" ")));
        }
        if let Some(color) = &run.color {
            style.push(format!("color:{color}"));
        }
        if !run.font.is_empty() {
            let family = run.font.replace('\\', "\\\\").replace('"', "\\\"");
            style.push(format!("font-family:\"{family}\",sans-serif"));
        }
        if run.spacing != 0.0 {
            style.push(format!("letter-spacing:{}pt", num(run.spacing)));
        }
        if run.baseline != 0.0 && run.size > 0.0 {
            style.push(format!("vertical-align:{}pt", num(run.baseline * run.size)));
        }
        if let Some(highlight) = &run.highlight {
            style.push(format!("background:{highlight}"));
        }
        out.push_str(&format!(
            "<span style=\"{}\">{}</span>",
            style.join(";"),
            escape_html(&run.text)
        ));
    }
    out
}

/// The URL for a deck media index, or nothing when the index is out of
/// range (the item renders empty rather than pointing at nothing).
pub(crate) fn media_src(deck: &SlideDeck, media: usize) -> Option<String> {
    deck.media.get(media).map(|m| {
        format!(
            "{SCHEME}://{HOST}/media-{media}/{}",
            crate::epubview::encode_path(&m.name)
        )
    })
}

/// An `<img>` for a media index, object-fit contained (a poster inside a
/// frame, a picture bullet).
pub(crate) fn poster_img(deck: &SlideDeck, media: usize) -> String {
    match media_src(deck, media) {
        Some(src) => format!("<img src=\"{src}\" alt=\"\">"),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

/// A number for HTML: two decimals at most, no trailing zeros.
pub(crate) fn num(v: f64) -> String {
    //  the file's own test pins num(-0.0) == "0"; normalise the signed
    // zero before formatting.
    let v = if v == 0.0 { 0.0 } else { v };
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// Escape text for HTML content and attribute values.
pub(crate) fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}
