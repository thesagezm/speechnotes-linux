//! The ODF presentation reader: `.odp` → the resolved model
//! [`crate::slides::SlideDeck`] the deck viewer draws, so one renderer
//! serves both formats.
//!
//! The shape it must arrive at is the pptx adapter's, but the work is not
//! the same. PowerPoint hides everything behind the placeholder → layout →
//! master → theme chain; LibreOffice Impress writes **explicit geometry**:
//! every frame carries its own position and size, every style its own fill
//! and font. There is no inheritance chain worth chasing, so this reader
//! resolves one level — the automatic style a shape or paragraph names —
//! and takes the result as final, which for a deck Impress saved is what
//! it is.
//!
//! Read with the crate's own pull parser ([`crate::xml`]) and ZIP reader
//! ([`crate::zip`]), the same pair EPUB uses; no ODF crate is pulled in.
//! Documented degradations, all rare in the wild:
//!
//! * Charts and embedded objects render as a labelled frame (the chart part
//!   is its own XML dialect; the pptx side draws only the common chart
//!   family too).
//! * `draw:custom-shape` enhanced geometry draws as a plain box with the
//!   style's fill.
//! * List numbering styles draw plain bullets; the text is exact.

use std::collections::HashMap;

use crate::slides::{
    assemble_text, Anchor, Border, Bullet, Content, DeckMedia, Item, Para, Run, Slide, SlideDeck,
    SlideError, SvgBody, TableRow,
};
use crate::xml::{Attr, Event, Parser};
use crate::zip::Archive;

/// Whether these bytes look like an ODF presentation: a ZIP whose
/// `mimetype` entry names one, or (a stripped package) a `content.xml`
/// whose body carries an `office:presentation`.
pub fn sniffs_like_odp(bytes: &[u8]) -> bool {
    let Ok(archive) = Archive::parse(bytes.to_vec()) else {
        return false;
    };
    if let Ok(mime) = archive.read_string("mimetype") {
        if mime.starts_with("application/vnd.oasis.opendocument.presentation") {
            return true;
        }
    }
    // The mimetype entry is missing or names a sibling format; the body
    // decides. A substring scan, not a parse — this runs on every unknown
    // zip the importer meets.
    archive
        .read_string("content.xml")
        .is_ok_and(|c| c.contains("office:presentation"))
}

/// Parse an ODF presentation into the resolved deck model.
pub fn parse(bytes: &[u8]) -> Result<SlideDeck, SlideError> {
    let archive = Archive::parse(bytes.to_vec())
        .map_err(|e| SlideError::Read(format!("the ODF package: {e}")))?;
    let content = archive
        .read_string("content.xml")
        .map_err(|e| SlideError::Read(format!("content.xml: {e}")))?;
    let styles = archive.read_string("styles.xml").unwrap_or_default();

    let mut doc = Doc {
        archive: Some(archive),
        ..Default::default()
    };
    // styles.xml first: its page layout sizes the deck, its master pages
    // carry backgrounds, and its automatic styles fill in. content.xml then
    // adds its own styles and the slides.
    walk_styles_xml(&styles, &mut doc);
    walk_content_xml(&content, &mut doc)?;

    let (w, h) = doc.slide_size().unwrap_or((960.0, 540.0));
    let mut deck = SlideDeck {
        w,
        h,
        title: doc.title.clone(),
        author: String::new(), // meta.xml's dc:creator is the last *editor*, not an author
        slides: doc.slides,
        media: doc.media,
    };
    for slide in &mut deck.slides {
        assemble_text(slide);
    }
    Ok(deck)
}

// ---------------------------------------------------------------------------
// Style model
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct TextStyle {
    font: Option<String>,
    size: Option<f64>,
    bold: Option<bool>,
    italic: Option<bool>,
    color: Option<String>,
    underline: Option<bool>,
    strike: Option<bool>,
}

impl TextStyle {
    /// `over`'s set fields win.
    fn overlay(&mut self, over: &TextStyle) {
        if over.font.is_some() {
            self.font = over.font.clone();
        }
        if over.size.is_some() {
            self.size = over.size;
        }
        if over.bold.is_some() {
            self.bold = over.bold;
        }
        if over.italic.is_some() {
            self.italic = over.italic;
        }
        if over.color.is_some() {
            self.color = over.color.clone();
        }
        if over.underline.is_some() {
            self.underline = over.underline;
        }
        if over.strike.is_some() {
            self.strike = over.strike;
        }
    }

    fn apply_to(&self, run: &mut Run) {
        if let Some(f) = &self.font {
            run.font = f.clone();
        }
        if let Some(s) = self.size {
            run.size = s.max(4.0);
        }
        run.bold = self.bold.unwrap_or(false);
        run.italic = self.italic.unwrap_or(false);
        run.underline = self.underline.unwrap_or(false);
        run.strike = self.strike.unwrap_or(false);
        if let Some(c) = &self.color {
            run.color = Some(c.clone());
        }
    }
}

#[derive(Default, Clone)]
struct ParaStyle {
    align: Option<String>,
    margin_left: Option<f64>,
    indent: Option<f64>,
    space_before: Option<f64>,
    space_after: Option<f64>,
    /// Line spacing: a multiplier (>0, into `Para::line_pct`) or exact
    /// points (<0 negated, into `Para::line_pts`).
    line: Option<f64>,
    text: TextStyle,
}

#[derive(Default, Clone)]
enum Fill {
    #[default]
    None,
    Solid(String),
    Gradient(String),
}

#[derive(Default, Clone)]
struct Stroke {
    width: f64,
    color: String,
    /// CSS border-style.
    style: String,
}

#[derive(Default, Clone)]
struct GraphicStyle {
    fill: Option<Fill>,
    stroke: Option<Stroke>,
    anchor: Option<Anchor>,
    wrap: Option<bool>,
    align: Option<String>,
    text: TextStyle,
}

#[derive(Default)]
struct Styles {
    graphic: HashMap<String, GraphicStyle>,
    para: HashMap<String, ParaStyle>,
    text: HashMap<String, TextStyle>,
    /// `table:table-cell@table:style-name` → background colour.
    cell_bg: HashMap<String, Option<String>>,
    /// `draw:page@draw:style-name` → background paint.
    page_bg: HashMap<String, Option<String>>,
    /// `style:page-layout` name → (width, height) in points.
    page_layouts: HashMap<String, (f64, f64)>,
    /// master-page name → page-layout name, and → drawing-page style name.
    master_layout: HashMap<String, String>,
    master_bg: HashMap<String, Option<String>>,
    /// master-page order — the first one sizes the deck.
    master_order: Vec<String>,
    /// `draw:gradient` name → (start, end, ODF angle in degrees).
    gradients: HashMap<String, (String, String, f64)>,
    /// `table:table-column@table:style-name` → width in points.
    col_width: HashMap<String, f64>,
    /// `table:table-row@table:style-name` → height in points.
    row_height: HashMap<String, f64>,
}

impl Styles {
    fn graphic(&self, name: Option<&str>) -> GraphicStyle {
        name.and_then(|n| self.graphic.get(n)).cloned().unwrap_or_default()
    }
    fn para(&self, name: Option<&str>) -> ParaStyle {
        name.and_then(|n| self.para.get(n)).cloned().unwrap_or_default()
    }
    fn text(&self, name: Option<&str>) -> TextStyle {
        name.and_then(|n| self.text.get(n)).cloned().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Document state
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Doc {
    archive: Option<Archive>,
    styles: Styles,
    media: Vec<DeckMedia>,
    /// Normalised href (`Pictures/x.png`) → media index.
    media_by_href: HashMap<String, usize>,
    slides: Vec<Slide>,
    title: String,
}

impl Doc {
    /// The deck's slide size: the first master page's layout.
    fn slide_size(&self) -> Option<(f64, f64)> {
        let master = self.styles.master_order.first()?;
        let layout = self.styles.master_layout.get(master)?;
        self.styles.page_layouts.get(layout).copied()
    }

    /// A page's background paint: its own drawing-page style, else its
    /// master page's. `transparent` means "the style set no fill".
    fn page_background(&self, page_style: Option<&str>, master: Option<&str>) -> Option<String> {
        if let Some(name) = page_style {
            if let Some(bg) = self.styles.page_bg.get(name) {
                return bg.clone().filter(|c| c != "transparent");
            }
        }
        let style = self.styles.master_bg.get(master?)?;
        style.clone().filter(|c| c != "transparent")
    }

    /// A picture by its `xlink:href`, registered into the deck's media on
    /// first use. `None`: external, or a format no browser serves.
    fn media_for(&mut self, href: &str) -> Option<usize> {
        if href.starts_with("http://") || href.starts_with("https://") || href.starts_with("data:")
        {
            return None;
        }
        let name = normalise_href(href);
        if let Some(&idx) = self.media_by_href.get(&name) {
            return Some(idx);
        }
        let data = self.archive.as_ref()?.read(&name).ok()?;
        let file = name.rsplit('/').next().unwrap_or(&name);
        let content_type = match file.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str()
        {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "bmp" => "image/bmp",
            "tif" | "tiff" => "image/tiff",
            _ => return None,
        };
        let idx = self.media.len();
        self.media.push(DeckMedia {
            name: format!("m{idx}"),
            content_type: content_type.into(),
            data: data.into(),
        });
        self.media_by_href.insert(name, idx);
        Some(idx)
    }

    fn column_width(&self, name: &str) -> Option<f64> {
        self.styles.col_width.get(name).copied()
    }

    fn row_height(&self, name: &str) -> Option<f64> {
        self.styles.row_height.get(name).copied()
    }

    /// A cell style's text defaults (stored under `cell:<name>`).
    fn cell_text(&self, name: &str) -> TextStyle {
        self.styles.text.get(&format!("cell:{name}")).cloned().unwrap_or_default()
    }
}

/// `./Pictures/x.png` and `../Pictures/x.png` → `Pictures/x.png`; package
/// content lives at the root either way.
fn normalise_href(href: &str) -> String {
    let mut out = String::new();
    for part in href.split('/') {
        match part {
            "." | "" => {}
            ".." => out.clear(), // content.xml sits at the root: nothing above
            p => {
                if !out.is_empty() {
                    out.push('/');
                }
                out.push_str(p);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// styles.xml: page layouts, master pages, shared styles
// ---------------------------------------------------------------------------

fn walk_styles_xml(src: &str, doc: &mut Doc) {
    let mut parser = Parser::new(src);
    loop {
        let ev = match parser.next_event() {
            Ok(ev) => ev,
            Err(_) => return,
        };
        match ev {
            Some(Event::Start { name, attrs }) => match local(&name).as_str() {
                "style" => parse_style(&attrs, &mut parser, doc),
                "page-layout" => parse_page_layout(&attrs, &mut parser, doc),
                "master-page" => parse_master_page(&attrs, doc),
                //  the original last state skipped uninteresting
                // elements here, but the page layouts and master pages the
                // walk exists for live INSIDE office:automatic-styles and
                // office:master-styles — skipping strands slide_size() on
                // its default (the original own odp fixture test pins the
                // layout-derived size). Descend instead, as the content.xml
                // walk does, and consume the one gradient variant that
                // carries its payload in children.
                "gradient" => read_gradient(&attrs, doc),
                _ => {}
            },
            Some(Event::Empty { name, attrs }) => {
                // LibreOffice writes self-closing master pages (and gradients);
                // both carry state the walk needs.
                match local(&name).as_str() {
                    "gradient" => read_gradient(&attrs, doc),
                    "master-page" => parse_master_page(&attrs, doc),
                    _ => {}
                }
            }
            Some(Event::End { .. }) | Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
            None => return,
        }
    }
}

fn parse_master_page(attrs: &[Attr], doc: &mut Doc) {
    let name = Attr::get(attrs, "style:name").unwrap_or("").to_string();
    let layout = Attr::get(attrs, "style:page-layout-name").unwrap_or("").to_string();
    let bg_style = Attr::get(attrs, "draw:style-name").map(str::to_string);
    if name.is_empty() {
        return;
    }
    doc.styles.master_order.push(name.clone());
    doc.styles.master_layout.insert(name.clone(), layout);
    let bg = bg_style.and_then(|n| doc.styles.page_bg.get(&n).cloned().flatten());
    doc.styles.master_bg.insert(name, bg);
}

// ---------------------------------------------------------------------------
// content.xml: styles, then the slides
// ---------------------------------------------------------------------------

fn walk_content_xml(src: &str, doc: &mut Doc) -> Result<(), SlideError> {
    let mut parser = Parser::new(src);
    let mut in_presentation = 0usize;
    let mut page: Option<PageBuilder> = None;
    loop {
        let ev = match parser.next_event() {
            Ok(ev) => ev,
            Err(e) => return Err(SlideError::Read(format!("content.xml: {e}"))),
        };
        match ev {
            Some(Event::Start { name, attrs }) => match local(&name).as_str() {
                "style" => parse_style(&attrs, &mut parser, doc),
                "presentation" => in_presentation += 1,
                "page" if in_presentation > 0 => page = Some(PageBuilder::new(&attrs)),
                "notes" if page.is_some() => {
                    let mut notes = String::new();
                    collect_notes_text(&mut parser, &mut notes);
                    if let Some(pg) = page.as_mut() {
                        pg.add_notes(notes);
                    }
                }
                "frame" if page.is_some() => {
                    if let Some(pg) = page.as_mut() {
                        parse_frame(doc, &attrs, &mut parser, pg);
                    }
                }
                "g" if page.is_some() => {
                    if let Some(pg) = page.as_mut() {
                        parse_group(doc, &attrs, &mut parser, pg);
                    }
                }
                "rect" | "ellipse" | "line" | "polyline" | "polygon" | "connector"
                | "custom-shape" | "measure" | "open_bezier" | "closed_bezier" | "path"
                    if page.is_some() =>
                {
                    if let Some(pg) = page.as_mut() {
                        parse_bare_shape(doc, &name, &attrs, &mut parser, pg);
                    }
                }
                // An element this walk has no use for: its content is walked
                // too (the body and the automatic-styles are containers we
                // need to descend into), and its text is ignored.
                _ => {}
            },
            Some(Event::Empty { name, attrs }) => {
                // LibreOffice writes self-closing master pages (and gradients);
                // both carry state the walk needs.
                match local(&name).as_str() {
                    "gradient" => read_gradient(&attrs, doc),
                    "master-page" => parse_master_page(&attrs, doc),
                    _ => {}
                }
            }
            Some(Event::End { name }) => match local(&name).as_str() {
                "presentation" => in_presentation = in_presentation.saturating_sub(1),
                "page" => {
                    if let Some(pg) = page.take() {
                        doc.slides.push(pg.build(doc));
                    }
                }
                _ => {}
            },
            Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
            None => return Ok(()),
        }
    }
}

/// One slide under construction.
struct PageBuilder {
    style: Option<String>,
    master: Option<String>,
    items: Vec<Item>,
    title: String,
    notes: String,
}

impl PageBuilder {
    fn new(attrs: &[Attr]) -> Self {
        Self {
            style: Attr::get(attrs, "draw:style-name").map(str::to_string),
            master: Attr::get(attrs, "draw:master-page-name").map(str::to_string),
            items: Vec::new(),
            title: String::new(),
            notes: String::new(),
        }
    }

    fn add_notes(&mut self, text: String) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if !self.notes.is_empty() {
            self.notes.push('\n');
        }
        self.notes.push_str(text);
    }

    fn build(self, doc: &Doc) -> Slide {
        Slide {
            title: self.title,
            bg: doc.page_background(self.style.as_deref(), self.master.as_deref()),
            items: self.items,
            notes: self.notes,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Frames, groups and bare shapes → items
// ---------------------------------------------------------------------------

/// One `draw:frame` — the box every text box, image and table rides in.
/// Consumes events through the frame's `End`.
fn parse_frame(doc: &mut Doc, attrs: &[Attr], p: &mut Parser, page: &mut PageBuilder) {
    let gstyle = doc.styles.graphic(Attr::get(attrs, "draw:style-name"));
    let (x, y, w, h) = geometry(attrs);
    let rot = transform_rotation(Attr::get(attrs, "draw:transform"));
    let class = Attr::get(attrs, "presentation:class").unwrap_or("").to_string();

    let mut paras: Vec<Para> = Vec::new();
    let mut picture: Option<usize> = None;
    let mut table: Option<Content> = None;
    let mut frame_label: Option<String> = None;

    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Some(Event::Start { name, attrs }) => match local(&name).as_str() {
                "text-box" => {
                    parse_text_box(doc, &gstyle, p, 0, &mut paras);
                }
                "image" => {
                    if let Some(href) = Attr::get(&attrs, "xlink:href") {
                        picture = doc.media_for(href);
                    }
                    consume_to_matching_end(p, "image");
                }
                "object" | "object-ole" => {
                    frame_label.get_or_insert_with(|| {
                        if class == "chart" {
                            "chart".to_string()
                        } else {
                            "object".to_string()
                        }
                    });
                    consume_to_matching_end(p, &name);
                }
                "plugin" => {
                    frame_label.get_or_insert_with(|| "media".to_string());
                    consume_to_matching_end(p, "plugin");
                }
                "table" => table = Some(parse_table(doc, &attrs, p)),
                _ => skip_element(p),
            },
            Some(Event::Empty { name, attrs }) => {
                if local(&name) == "image" {
                    if let Some(href) = Attr::get(&attrs, "xlink:href") {
                        picture = doc.media_for(href);
                    }
                }
            }
            Some(Event::End { .. }) => break,
            Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
            None => break,
        }
    }

    if class == "notes" {
        return; // notes frames are read by the notes walker, never drawn
    }
    if w <= 0.0 && h <= 0.0 {
        return; // a placeholder with no box
    }

    let content = if let Some(t) = table {
        t
    } else if let Some(media) = picture {
        Content::Picture { media, crop: [0.0; 4], ocr: None }
    } else if let Some(label) = frame_label {
        Content::Frame { label, media: None }
    } else {
        if class == "title" && page.title.is_empty() {
            page.title = paras
                .iter()
                .flat_map(|pa| pa.runs.iter().map(|r| r.text.as_str()))
                .collect::<String>()
                .trim()
                .to_string();
        }
        Content::Text {
            paras,
            anchor: gstyle.anchor.unwrap_or(Anchor::Top),
            wrap: gstyle.wrap.unwrap_or(true),
            dir: String::new(),
        }
    };
    // A picture fills its frame itself; the style's box fill would paint
    // behind it pointlessly.
    let fill = if picture.is_some() { None } else { gstyle.fill.clone() };
    page.items.push(Item {
        x,
        y,
        w,
        h,
        rot,
        flip_h: false,
        flip_v: false,
        fill: fill_to_css(fill.as_ref()),
        border: stroke_to_border(&gstyle.stroke),
        radius: 0.0,
        svg: None, // frames are boxes: fill and border paint them
        shadow: None,
        content,
    });
}

/// A `draw:g`: children with absolute geometry of their own. Consumes
/// through the group's `End`, pushes nothing for itself.
fn parse_group(doc: &mut Doc, attrs: &[Attr], p: &mut Parser, page: &mut PageBuilder) {
    let _ = attrs;
    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Some(Event::Start { name, attrs }) => match local(&name).as_str() {
                "frame" => parse_frame(doc, &attrs, p, page),
                "g" => parse_group(doc, &attrs, p, page),
                "rect" | "ellipse" | "line" | "polyline" | "polygon" | "connector"
                | "custom-shape" | "measure" | "open_bezier" | "closed_bezier" | "path" => {
                    parse_bare_shape(doc, &name, &attrs, p, page)
                }
                _ => skip_element(p),
            },
            Some(Event::Empty { .. }) => {}
            Some(Event::End { .. }) => break,
            Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
            None => break,
        }
    }
}

/// A bare shape: `draw:rect`, `draw:ellipse`, `draw:line`, … — positioned
/// by its own attributes (a `draw:line` by x1/y1/x2/y2), text carried
/// directly. Consumes through the shape's `End`.
fn parse_bare_shape(doc: &mut Doc, tag: &str, attrs: &[Attr], p: &mut Parser, page: &mut PageBuilder) {
    let tag = local(tag);
    let gstyle = doc.styles.graphic(Attr::get(attrs, "draw:style-name"));
    let (x, y, w, h) = if tag == "line" {
        line_geometry(attrs)
    } else {
        geometry(attrs)
    };
    let rot = transform_rotation(Attr::get(attrs, "draw:transform"));

    let mut paras = Vec::new();
    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Some(Event::Start { name, attrs }) => match local(&name).as_str() {
                "p" | "h" => {
                    let ps = doc.styles.para(Attr::get(&attrs, "text:style-name"));
                    paras.push(parse_paragraph(doc, &attrs, &gstyle, &ps, 0, p));
                }
                "list" => parse_list(doc, &gstyle, p, 1, &mut paras),
                _ => skip_element(p),
            },
            Some(Event::Empty { .. }) => {}
            Some(Event::End { .. }) => break,
            Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
            None => break,
        }
    }
    if w <= 0.0 && h <= 0.0 {
        return;
    }

    // Lines and open paths draw their stroke and have no area to fill.
    let stroked_only = matches!(tag.as_str(), "line" | "connector" | "polyline" | "open_bezier");
    let fill = if stroked_only {
        Fill::None
    } else {
        gstyle.fill.clone().unwrap_or(Fill::None)
    };
    let stroke = gstyle.stroke.clone().or_else(|| {
        stroked_only.then(|| Stroke { width: 1.0, color: "#343434".into(), style: "solid".into() })
    });
    page.items.push(Item {
        x,
        y,
        w: w.max(if stroked_only { 0.0 } else { 1.0 }),
        h: h.max(if stroked_only { 0.0 } else { 1.0 }),
        rot,
        flip_h: false,
        flip_v: false,
        fill: fill_to_css(Some(&fill)),
        border: stroke_to_border(&stroke),
        radius: 0.0,
        svg: shape_svg(&tag, w, h, &fill, &stroke),
        shadow: None,
        content: Content::Text {
            paras,
            anchor: gstyle.anchor.unwrap_or(Anchor::Top),
            wrap: gstyle.wrap.unwrap_or(true),
            dir: String::new(),
        },
    });
}

/// An SVG body for the bare shapes CSS cannot draw: the path in a
/// `0 0 w h` view box, fill and stroke applied. Boxes need none.
fn shape_svg(tag: &str, w: f64, h: f64, fill: &Fill, stroke: &Option<Stroke>) -> Option<SvgBody> {
    let stroke_css = stroke.as_ref().map(|s| s.color.clone());
    let stroke_width = stroke.as_ref().map(|s| s.width).unwrap_or(0.0);
    let (d, stroked_only) = match tag {
        "ellipse" => (
            format!(
                "M{:.1} {:.1} a{:.1} {:.1} 0 1 0 {:.1} 0 a{:.1} {:.1} 0 1 0 -{:.1} 0 Z",
                0.0,
                h / 2.0,
                w / 2.0,
                h / 2.0,
                w,
                w / 2.0,
                h / 2.0,
                w
            ),
            false,
        ),
        "line" | "connector" | "measure" => (format!("M0 0 L{w:.1} {h:.1}"), true),
        _ => return None, // rect-family and the point-list shapes: CSS / no body yet
    };
    let fill_css = if stroked_only {
        None
    } else {
        Some(fill_to_css(Some(fill)).unwrap_or_else(|| "none".into()))
    };
    Some(SvgBody { d, fill: fill_css, stroke: stroke_css, stroke_width })
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// A `draw:text-box`: paragraphs and lists. Consumes through the box's `End`.
fn parse_text_box(
    doc: &mut Doc,
    gstyle: &GraphicStyle,
    p: &mut Parser,
    list_depth: usize,
    out: &mut Vec<Para>,
) {
    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Some(Event::Start { name, attrs }) => match local(&name).as_str() {
                "p" | "h" => {
                    let ps = doc.styles.para(Attr::get(&attrs, "text:style-name"));
                    out.push(parse_paragraph(doc, &attrs, gstyle, &ps, list_depth, p));
                }
                "list" => parse_list(doc, gstyle, p, list_depth + 1, out),
                _ => skip_element(p),
            },
            Some(Event::Empty { .. }) => {}
            Some(Event::End { .. }) => break,
            Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
            None => break,
        }
    }
}

/// A `text:list`: its items' paragraphs, one level deeper. Consumes through
/// the list's `End`.
fn parse_list(doc: &mut Doc, gstyle: &GraphicStyle, p: &mut Parser, list_depth: usize, out: &mut Vec<Para>) {
    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Some(Event::Start { name, attrs: _ }) => match local(&name).as_str() {
                "list-item" => parse_list_item(doc, gstyle, p, list_depth, out),
                _ => skip_element(p),
            },
            Some(Event::Empty { .. }) => {}
            Some(Event::End { .. }) => break,
            Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
            None => break,
        }
    }
}

fn parse_list_item(doc: &mut Doc, gstyle: &GraphicStyle, p: &mut Parser, list_depth: usize, out: &mut Vec<Para>) {
    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Some(Event::Start { name, attrs }) => match local(&name).as_str() {
                "p" | "h" => {
                    let ps = doc.styles.para(Attr::get(&attrs, "text:style-name"));
                    out.push(parse_paragraph(doc, &attrs, gstyle, &ps, list_depth, p));
                }
                "list" => parse_list(doc, gstyle, p, list_depth + 1, out),
                _ => skip_element(p),
            },
            Some(Event::Empty { .. }) => {}
            Some(Event::End { .. }) => break,
            Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
            None => break,
        }
    }
}

/// One `text:p` / `text:h`, resolved against the paragraph style, the
/// shape's text defaults, and the list level. Consumes through the
/// paragraph's `End`.
fn parse_paragraph(
    doc: &Doc,
    attrs: &[Attr],
    gstyle: &GraphicStyle,
    ps: &ParaStyle,
    list_depth: usize,
    p: &mut Parser,
) -> Para {
    let mut base = gstyle.text.clone();
    base.overlay(&ps.text);
    let mut runs = Vec::new();
    collect_runs(doc, p, base, &mut runs);
    let empty_size = runs.iter().map(|r| r.size).fold(0.0f64, f64::max).max(18.0);
    let in_list = list_depth > 0 || Attr::get(attrs, "text:level").is_some();
    Para {
        runs,
        align: ps
            .align
            .clone()
            .or_else(|| gstyle.align.clone())
            .unwrap_or_else(|| "left".into()),
        level: list_depth.saturating_sub(1).min(8) as u8,
        bullet: in_list.then(|| Bullet { char: "•".into(), media: None }),
        margin_left: ps.margin_left.unwrap_or(if in_list { list_depth as f64 * 14.17 } else { 0.0 }),
        indent: ps.indent.unwrap_or(0.0),
        space_before: ps.space_before.unwrap_or(0.0),
        space_after: ps.space_after.unwrap_or(0.0),
        line_pct: ps.line.filter(|l| *l > 0.0).unwrap_or(0.0),
        line_pts: ps.line.filter(|l| *l < 0.0).map(|l| -l).unwrap_or(0.0),
        empty_size,
    }
}

/// The runs of one paragraph's inline content: text, spans, links, tabs,
/// spaces, line breaks. Consumes through the paragraph's `End`.
fn collect_runs(doc: &Doc, p: &mut Parser, base: TextStyle, runs: &mut Vec<Run>) {
    let mut stack = vec![base];
    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => return,
        };
        match ev {
            Some(Event::Start { name, attrs }) => match local(&name).as_str() {
                "span" | "a" | "ruby" | "rubytext" => {
                    let parent = stack.last().cloned().unwrap_or_default();
                    let mut ts = doc.styles.text(Attr::get(&attrs, "text:style-name"));
                    ts.overlay(&parent);
                    stack.push(ts);
                }
                _ => skip_element(p),
            },
            Some(Event::Empty { name, .. }) => {
                if matches!(local(&name).as_str(), "tab" | "s" | "line-break") {
                    if let Some(last) = runs.last_mut() {
                        last.text.push(' ');
                    }
                }
            }
            Some(Event::Text(text)) => {
                let style = stack.last().cloned().unwrap_or_default();
                let mut run = Run {
                    text: text.replace('\n', " "),
                    ..Default::default()
                };
                style.apply_to(&mut run);
                if run.size == 0.0 {
                    run.size = 18.0;
                }
                // Impress emits a span per formatting change; identical
                // neighbouring runs merge into one.
                if let Some(last) = runs.last_mut() {
                    if last.font == run.font
                        && last.size == run.size
                        && last.bold == run.bold
                        && last.italic == run.italic
                        && last.underline == run.underline
                        && last.strike == run.strike
                        && last.color == run.color
                        && last.text.chars().last().is_some_and(|c| c.is_whitespace())
                            == run.text.chars().next().is_some_and(|c| c.is_whitespace())
                    {
                        last.text.push_str(&run.text);
                        continue;
                    }
                }
                runs.push(run);
            }
            Some(Event::End { name }) => match local(&name).as_str() {
                "span" | "a" | "ruby" | "rubytext" => {
                    stack.pop();
                }
                _ => return, // the paragraph's own end
            },
            Some(Event::Raw { .. }) => {}
            None => return,
        }
    }
}

/// Speaker notes: all paragraph text inside a `presentation:notes`
/// container, without building items. Consumes through the notes' `End`.
fn collect_notes_text(p: &mut Parser, out: &mut String) {
    let mut depth = 0usize;
    let mut para = String::new();
    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Some(Event::Start { name, .. }) => {
                depth += 1;
                if matches!(local(&name).as_str(), "p" | "h") {
                    para.clear();
                }
            }
            Some(Event::Empty { .. }) => {}
            Some(Event::Text(t)) => para.push_str(&t),
            Some(Event::End { name }) => {
                if matches!(local(&name).as_str(), "p" | "h") {
                    let trimmed = para.trim();
                    if !trimmed.is_empty() {
                        if !out.is_empty() {
                            out.push('\n');
                        }
                        out.push_str(trimmed);
                    }
                }
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            Some(Event::Raw { .. }) => {}
            None => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// A `table:table` inside a frame. Consumes through the table's `End`.
fn parse_table(doc: &mut Doc, attrs: &[Attr], p: &mut Parser) -> Content {
    let mut col_widths: Vec<f64> = Vec::new();
    let mut row_heights: Vec<f64> = Vec::new();
    let mut rows: Vec<TableRow> = Vec::new();
    let bg = Attr::get(attrs, "table:style-name")
        .and_then(|n| doc.styles.cell_bg.get(n).cloned().flatten());

    #[derive(PartialEq)]
    enum Where {
        Top,
        InRows,
        InRow,
        InCell,
    }
    let mut at = Where::Top;
    let mut cell = crate::slides::TableCell::default();
    let mut row_cells: Vec<crate::slides::TableCell> = Vec::new();
    let mut cell_text_base = TextStyle::default();

    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Some(Event::Start { name, attrs }) => {
                let tag = local(&name);
                match (tag.as_str(), &at) {
                    ("table-column", Where::Top) => {
                        let width = Attr::get(&attrs, "table:style-name")
                            .and_then(|n| doc.column_width(n))
                            .unwrap_or(0.0);
                        let repeat = Attr::get(&attrs, "table:number-columns-repeated")
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(1)
                            .min(256);
                        for _ in 0..repeat {
                            col_widths.push(width);
                        }
                        consume_to_matching_end(p, "table-column");
                    }
                    ("table-rows" | "table-header-rows" | "table-row-group", Where::Top) => {
                        at = Where::InRows;
                    }
                    ("table-row", Where::Top | Where::InRows) => {
                        at = Where::InRow;
                        row_cells.clear();
                        let h = Attr::get(&attrs, "table:style-name")
                            .and_then(|n| doc.row_height(n))
                            .unwrap_or(0.0);
                        row_heights.push(h);
                    }
                    ("table-cell", Where::InRow) => {
                        at = Where::InCell;
                        cell_text_base = doc
                            .cell_text(Attr::get(&attrs, "table:style-name").unwrap_or(""));
                        cell = crate::slides::TableCell {
                            col_span: Attr::get(&attrs, "table:number-columns-spanned")
                                .and_then(|v| v.parse::<u32>().ok())
                                .unwrap_or(1)
                                .max(1),
                            row_span: Attr::get(&attrs, "table:number-rows-spanned")
                                .and_then(|v| v.parse::<u32>().ok())
                                .unwrap_or(1)
                                .max(1),
                            fill: Attr::get(&attrs, "table:style-name")
                                .and_then(|n| doc.styles.cell_bg.get(n).cloned().flatten()),
                            ..Default::default()
                        };
                    }
                    ("covered-table-cell", Where::InRow) => {
                        let repeat = Attr::get(&attrs, "table:number-columns-repeated")
                            .and_then(|v| v.parse::<u32>().ok())
                            .unwrap_or(1)
                            .max(1);
                        for _ in 0..repeat {
                            row_cells.push(crate::slides::TableCell { covered: true, ..Default::default() });
                        }
                        consume_to_matching_end(p, "covered-table-cell");
                    }
                    ("p" | "h", Where::InCell) => {
                        let ps = doc.styles.para(Attr::get(&attrs, "text:style-name"));
                        let mut base = cell_text_base.clone();
                        base.overlay(&ps.text);
                        let mut runs = Vec::new();
                        collect_runs(doc, p, base, &mut runs);
                        if !cell.text.is_empty() {
                            cell.text.push('\n');
                        }
                        for r in &runs {
                            cell.text.push_str(&r.text);
                        }
                        cell.runs.extend(runs);
                        if let Some(a) = ps.align {
                            cell.align = a;
                        }
                    }
                    _ => skip_element(p),
                }
            }
            Some(Event::Empty { name, .. }) => {
                if local(&name) == "table-column" && at == Where::Top {
                    col_widths.push(0.0);
                }
            }
            Some(Event::End { name }) => {
                let tag = local(&name);
                match tag.as_str() {
                    "table-cell" if at == Where::InCell => {
                        row_cells.push(std::mem::take(&mut cell));
                        at = Where::InRow;
                    }
                    "table-row" => {
                        rows.push(TableRow { cells: std::mem::take(&mut row_cells) });
                        at = Where::InRows;
                    }
                    "table-rows" | "table-header-rows" | "table-row-group" => at = Where::Top,
                    "table" => break,
                    _ => {}
                }
            }
            Some(Event::Text(t)) => {
                if at == Where::InCell {
                    cell.text.push_str(t.trim());
                }
            }
            Some(Event::Raw { .. }) => {}
            None => break,
        }
    }
    Content::Table { col_widths, row_heights, rows, bg }
}

// ---------------------------------------------------------------------------
// The style:style walker (shared by both files)
// ---------------------------------------------------------------------------

/// One `style:style`: dispatched by family, its property children read.
/// Consumes through the style's `End`.
fn parse_style(attrs: &[Attr], p: &mut Parser, doc: &mut Doc) {
    let name = Attr::get(attrs, "style:name").unwrap_or("").to_string();
    let family = Attr::get(attrs, "style:family").unwrap_or("").to_string();
    if name.is_empty() {
        skip_element(p);
        return;
    }
    match family.as_str() {
        "graphic" => {
            let mut g = GraphicStyle::default();
            loop {
                let ev = match p.next_event() {
                    Ok(ev) => ev,
                    Err(_) => break,
                };
                match ev {
                    Some(Event::Start { name, attrs }) | Some(Event::Empty { name, attrs }) => {
                        match local(&name).as_str() {
                            "graphic-properties" => read_graphic_props(&attrs, &mut g, doc),
                            "text-properties" => read_text_props(&attrs, &mut g.text),
                            "paragraph-properties" => {
                                if let Some(a) = text_align(&attrs) {
                                    g.align = Some(a);
                                }
                            }
                            _ => {}
                        }
                    }
                    Some(Event::End { .. }) => break,
                    Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
                    None => break,
                }
            }
            doc.styles.graphic.insert(name, g);
        }
        "paragraph" => {
            let mut ps = ParaStyle::default();
            let mut text = TextStyle::default();
            loop {
                let ev = match p.next_event() {
                    Ok(ev) => ev,
                    Err(_) => break,
                };
                match ev {
                    Some(Event::Start { name, attrs }) | Some(Event::Empty { name, attrs }) => {
                        match local(&name).as_str() {
                            "paragraph-properties" => read_para_props(&attrs, &mut ps),
                            "text-properties" => read_text_props(&attrs, &mut text),
                            _ => {}
                        }
                    }
                    Some(Event::End { .. }) => break,
                    Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
                    None => break,
                }
            }
            ps.text = text;
            doc.styles.para.insert(name, ps);
        }
        "text" => {
            let mut ts = TextStyle::default();
            loop {
                let ev = match p.next_event() {
                    Ok(ev) => ev,
                    Err(_) => break,
                };
                match ev {
                    Some(Event::Start { name, attrs }) | Some(Event::Empty { name, attrs }) => {
                        if local(&name) == "text-properties" {
                            read_text_props(&attrs, &mut ts);
                        }
                    }
                    Some(Event::End { .. }) => break,
                    Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
                    None => break,
                }
            }
            doc.styles.text.insert(name, ts);
        }
        "drawing-page" => {
            let mut bg: Option<String> = None;
            loop {
                let ev = match p.next_event() {
                    Ok(ev) => ev,
                    Err(_) => break,
                };
                match ev {
                    Some(Event::Start { name, attrs }) | Some(Event::Empty { name, attrs }) => {
                        if local(&name) == "drawing-page-properties" {
                            // "unset" and "explicitly none" are distinct to
                            // the reader but both leave the page unpainted:
                            // a transparent marker the caller filters out.
                            if Attr::get(&attrs, "draw:fill") == Some("none") {
                                bg = Some("transparent".into());
                            } else if let Some(c) = Attr::get(&attrs, "draw:fill-color") {
                                bg = Some(c.to_string());
                            }
                        }
                    }
                    Some(Event::End { .. }) => break,
                    Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
                    None => break,
                }
            }
            doc.styles.page_bg.insert(name, bg);
        }
        "table-cell" => {
            let mut bg: Option<String> = None;
            let mut text = TextStyle::default();
            loop {
                let ev = match p.next_event() {
                    Ok(ev) => ev,
                    Err(_) => break,
                };
                match ev {
                    Some(Event::Start { name, attrs }) | Some(Event::Empty { name, attrs }) => {
                        match local(&name).as_str() {
                            "table-cell-properties" => {
                                if let Some(c) = Attr::get(&attrs, "fo:background-color") {
                                    bg = Some(c.to_string());
                                }
                            }
                            "text-properties" => read_text_props(&attrs, &mut text),
                            _ => {}
                        }
                    }
                    Some(Event::End { .. }) => break,
                    Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
                    None => break,
                }
            }
            doc.styles.cell_bg.insert(name.clone(), bg);
            doc.styles.text.insert(format!("cell:{name}"), text);
        }
        "table-column" => {
            let mut width: Option<f64> = None;
            loop {
                let ev = match p.next_event() {
                    Ok(ev) => ev,
                    Err(_) => break,
                };
                match ev {
                    Some(Event::Start { name, attrs }) | Some(Event::Empty { name, attrs }) => {
                        if local(&name) == "table-column-properties" {
                            width = Attr::get(&attrs, "style:column-width").and_then(parse_len);
                        }
                    }
                    Some(Event::End { .. }) => break,
                    Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
                    None => break,
                }
            }
            if let Some(w) = width {
                doc.styles.col_width.insert(name, w);
            }
        }
        "table-row" => {
            let mut height: Option<f64> = None;
            loop {
                let ev = match p.next_event() {
                    Ok(ev) => ev,
                    Err(_) => break,
                };
                match ev {
                    Some(Event::Start { name, attrs }) | Some(Event::Empty { name, attrs }) => {
                        if local(&name) == "table-row-properties" {
                            height = Attr::get(&attrs, "style:row-height").and_then(parse_len);
                        }
                    }
                    Some(Event::End { .. }) => break,
                    Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
                    None => break,
                }
            }
            if let Some(h) = height {
                doc.styles.row_height.insert(name, h);
            }
        }
        _ => skip_element(p),
    }
}

/// `draw:gradient`: start/end colours and the angle, for graphic styles
/// that reference it by name.
fn read_gradient(attrs: &[Attr], doc: &mut Doc) {
    let Some(name) = Attr::get(attrs, "draw:name").map(str::to_string) else {
        return;
    };
    let start = Attr::get(attrs, "draw:start-color").unwrap_or("#ffffff").to_string();
    let end = Attr::get(attrs, "draw:end-color").unwrap_or("#ffffff").to_string();
    // ODF stores the angle in 1/10°, counter-clockwise from +x.
    let angle = Attr::get(attrs, "draw:angle")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
        / 10.0;
    doc.styles.gradients.insert(name, (start, end, angle));
}

fn read_graphic_props(attrs: &[Attr], g: &mut GraphicStyle, doc: &Doc) {
    match Attr::get(attrs, "draw:fill") {
        Some("none") => g.fill = Some(Fill::None),
        Some("solid") => {
            if let Some(c) = Attr::get(attrs, "draw:fill-color") {
                g.fill = Some(Fill::Solid(c.to_string()));
            }
        }
        Some("gradient") => {
            let gradient = Attr::get(attrs, "draw:gradient-name").and_then(|name| {
                doc.styles.gradients.get(name).map(|(s, e, angle)| {
                    // ODF's angle runs counter-clockwise from +x; CSS runs
                    // clockwise from bottom→top — the same fix the pptx
                    // side applies to PowerPoint's angle.
                    let css_angle = (90.0 - angle).rem_euclid(360.0);
                    Fill::Gradient(format!(
                        "linear-gradient({css_angle:.1}deg, {s}, {e})"
                    ))
                })
            });
            g.fill = Some(gradient.unwrap_or(Fill::None));
        }
        // Bitmap and hatch fills have no direct CSS paint; the fill colour,
        // when the style carries one alongside, is the honest flat stand-in.
        Some(_) | None => {
            if let Some(c) = Attr::get(attrs, "draw:fill-color") {
                g.fill = Some(Fill::Solid(c.to_string()));
            }
        }
    }
    if let Some(w) = Attr::get(attrs, "svg:stroke-width") {
        let color = Attr::get(attrs, "svg:stroke-color").unwrap_or("#343434").to_string();
        let style = match Attr::get(attrs, "draw:stroke") {
            Some("dash") => "dashed",
            Some("dot") => "dotted",
            _ => "solid",
        };
        g.stroke = Some(Stroke { width: parse_len(w).unwrap_or(0.75).max(0.25), color, style: style.into() });
    } else if Attr::get(attrs, "draw:stroke") == Some("none") {
        g.stroke = None;
    }
    match Attr::get(attrs, "draw:textarea-vertical-align") {
        Some("middle" | "central") => g.anchor = Some(Anchor::Middle),
        Some("bottom") => g.anchor = Some(Anchor::Bottom),
        Some("top") => g.anchor = Some(Anchor::Top),
        _ => {}
    }
    if Attr::get(attrs, "fo:wrap-option") == Some("no-wrap") {
        g.wrap = Some(false);
    }
    if let Some(a) = text_align(attrs) {
        g.align = Some(a);
    }
}

fn read_para_props(attrs: &[Attr], ps: &mut ParaStyle) {
    if let Some(a) = text_align(attrs) {
        ps.align = Some(a);
    }
    if let Some(m) = Attr::get(attrs, "fo:margin-left") {
        ps.margin_left = parse_len(m);
    }
    if let Some(m) = Attr::get(attrs, "fo:text-indent") {
        ps.indent = parse_len(m);
    }
    if let Some(m) = Attr::get(attrs, "fo:margin-top") {
        ps.space_before = parse_len(m);
    }
    if let Some(m) = Attr::get(attrs, "fo:margin-bottom") {
        ps.space_after = parse_len(m);
    }
    if let Some(l) = Attr::get(attrs, "fo:line-height") {
        ps.line = parse_line_height(l);
    }
}

fn read_text_props(attrs: &[Attr], ts: &mut TextStyle) {
    if let Some(f) = Attr::get(attrs, "style:font-name")
        .or_else(|| Attr::get(attrs, "fo:font-family"))
    {
        ts.font = Some(f.split(',').next().unwrap_or(f).trim_matches('\'').to_string());
    }
    if let Some(s) = Attr::get(attrs, "fo:font-size") {
        ts.size = parse_len(s);
    }
    if let Some(w) = Attr::get(attrs, "fo:font-weight") {
        ts.bold = Some(w == "bold" || w == "700" || w == "800" || w == "900");
    }
    if let Some(s) = Attr::get(attrs, "fo:font-style") {
        ts.italic = Some(s == "italic" || s == "oblique");
    }
    if let Some(c) = Attr::get(attrs, "fo:color") {
        if c.starts_with('#') {
            ts.color = Some(c.to_string());
        }
    }
    if let Some(u) = Attr::get(attrs, "style:text-underline-style") {
        ts.underline = Some(u != "none");
    }
    if let Some(s) = Attr::get(attrs, "fo:text-line-through-style") {
        ts.strike = Some(s != "none");
    }
}

fn text_align(attrs: &[Attr]) -> Option<String> {
    match Attr::get(attrs, "fo:text-align")? {
        "center" => Some("center".into()),
        "right" | "end" => Some("right".into()),
        "justify" => Some("justify".into()),
        "left" | "start" => Some("left".into()),
        _ => None,
    }
}

/// `style:page-layout`: the slide size. Consumes through the layout's `End`.
fn parse_page_layout(attrs: &[Attr], p: &mut Parser, doc: &mut Doc) {
    let name = Attr::get(attrs, "style:name").unwrap_or("").to_string();
    let mut size: Option<(f64, f64)> = None;
    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Some(Event::Start { name, attrs }) | Some(Event::Empty { name, attrs }) => {
                if local(&name) == "page-layout-properties" {
                    if let (Some(w), Some(h)) = (
                        Attr::get(&attrs, "fo:page-width").and_then(parse_len),
                        Attr::get(&attrs, "fo:page-height").and_then(parse_len),
                    ) {
                        size = Some((w, h));
                    }
                }
            }
            Some(Event::End { .. }) => break,
            Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
            None => break,
        }
    }
    if !name.is_empty() {
        if let Some(size) = size {
            doc.styles.page_layouts.insert(name, size);
        }
    }
}

// ---------------------------------------------------------------------------
// Geometry and paint helpers
// ---------------------------------------------------------------------------

/// A frame's box: `svg:x/y/width/height` in points.
fn geometry(attrs: &[Attr]) -> (f64, f64, f64, f64) {
    let num = |n: &str| Attr::get(attrs, n).and_then(parse_len).unwrap_or(0.0);
    (num("svg:x"), num("svg:y"), num("svg:width"), num("svg:height"))
}

/// `draw:line`'s `svg:x1/y1/x2/y2`, normalised to a box.
fn line_geometry(attrs: &[Attr]) -> (f64, f64, f64, f64) {
    let num = |n: &str| Attr::get(attrs, n).and_then(parse_len).unwrap_or(0.0);
    let (x1, y1, x2, y2) = (num("svg:x1"), num("svg:y1"), num("svg:x2"), num("svg:y2"));
    (x1.min(x2), y1.min(y2), (x2 - x1).abs(), (y2 - y1).abs())
}

/// The rotation in `draw:transform` (`rotate (a) translate (…) scale (…)`)
/// — ODF rotates counter-clockwise, the model clockwise. The translate and
/// scale parts are rare and dropped: the box stays where the frame says.
fn transform_rotation(t: Option<&str>) -> f64 {
    let Some(t) = t else { return 0.0 };
    let Some(rest) = t.trim().strip_prefix("rotate") else {
        return 0.0;
    };
    let rad: f64 = rest
        .trim_start_matches(['(', ' '])
        .split(|c: char| c == ')' || c.is_whitespace())
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    -rad.to_degrees()
}

/// An ODF length (`28cm`, `5.1in`, `100pt`, `12mm`, `3pc`, `96px`) in
/// points. Percentages are relative — no slot for those — and read as
/// absent.
fn parse_len(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.ends_with('%') {
        return None;
    }
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let v: f64 = num.trim().parse().ok()?;
    Some(match unit {
        "cm" => v * 72.0 / 2.54,
        "mm" => v * 72.0 / 25.4,
        "in" => v * 72.0,
        "pt" => v,
        "pc" => v * 12.0,
        "px" => v * 0.75,
        "" => v,
        _ => return None,
    })
}

/// `fo:line-height`: a percentage is a multiplier, a length exact points
/// (returned negative — the caller splits the two).
fn parse_line_height(s: &str) -> Option<f64> {
    if let Some(pct) = s.trim().strip_suffix('%') {
        return pct.trim().parse::<f64>().ok().map(|v| v / 100.0);
    }
    parse_len(s).map(|v| -v)
}

fn fill_to_css(fill: Option<&Fill>) -> Option<String> {
    match fill {
        None | Some(Fill::None) => None,
        Some(Fill::Solid(c)) => Some(c.clone()),
        Some(Fill::Gradient(css)) if !css.is_empty() => Some(css.clone()),
        Some(Fill::Gradient(_)) => None,
    }
}

fn stroke_to_border(stroke: &Option<Stroke>) -> Option<Border> {
    let s = stroke.as_ref()?;
    Some(Border { width: s.width, color: s.color.clone(), style: s.style.clone() })
}

// ---------------------------------------------------------------------------
// Parser plumbing
// ---------------------------------------------------------------------------

/// The namespace prefix off an element name.
fn local(name: &str) -> String {
    match name.rsplit_once(':') {
        Some((_, l)) => l.to_string(),
        None => name.to_string(),
    }
}

/// Consume events until the matching `End` of `tag` — the caller has
/// already consumed its `Start`. Nested same-name elements counted.
fn consume_to_matching_end(p: &mut Parser, tag: &str) {
    let mut depth = 1usize;
    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => return,
        };
        match ev {
            Some(Event::Start { name, .. }) => {
                if local(&name) == tag {
                    depth += 1;
                }
            }
            Some(Event::End { name }) => {
                if local(&name) == tag {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
            }
            Some(Event::Empty { .. }) | Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
            None => return,
        }
    }
}

/// Skip one element whose `Start` the caller has consumed.
fn skip_element(p: &mut Parser) {
    let mut depth = 1usize;
    loop {
        let ev = match p.next_event() {
            Ok(ev) => ev,
            Err(_) => return,
        };
        match ev {
            Some(Event::Start { .. }) => depth += 1,
            Some(Event::End { .. }) => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            Some(Event::Empty { .. }) | Some(Event::Text(_)) | Some(Event::Raw { .. }) => {}
            None => return,
        }
    }
}
