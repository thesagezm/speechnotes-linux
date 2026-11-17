//! The slide decks: `.pptx` / `.ppsx` / `.potx` / legacy `.ppt` through
//! DeckCraft's ECMA-376 reader, `.odp` through the ODF reader in
//! [`crate::odp`], resolved into the shapes the deck viewer draws.
//!
//! The viewer's rule is the EPUB viewer's — **the deck is shown as the
//! deck**, never converted to PDF and never flattened to extracted text.
//! This module is the *resolved* shape both formats arrive at: every
//! position, fill, font and colour is final (the placeholder → layout →
//! master → theme inheritance chain is walked here, at parse time, not in
//! the UI), and what the UI receives is a slide of absolutely-positioned
//! items it can paint as HTML/CSS.
//!
//! Alongside the geometry, each slide carries its **speakable text with
//! byte spans**: the exact string the speech plan will chunk, with the byte
//! range of every paragraph mapped back to the item that shows it. That is
//! the read-along contract the chapter module states for EPUB — text in,
//! text out, with the offsets the speech text carries describing the speech
//! text itself — and it is built in the same pass as the geometry so the
//! two cannot drift.
//!
//! What is approximated rather than modelled (documented, honest
//! degradations):
//!
//! * Rotated *groups* with non-uniform scaling: the child box comes out
//!   axis-aligned-bounds-correct but not skew-correct. Flat decks — the
//!   overwhelming case — are exact.
//! * Chart types beyond the common family render as a labelled frame with
//!   the chart's title; column/bar/line/pie/doughnut draw from the cached
//!   series data.
//! * Arrowheads on connectors, WordArt warps and 3-D bevels are dropped.

use std::sync::Arc;

use deckcraft_color::Rgba;
use deckcraft_geom::preset as gpreset;
use deckcraft_model as dm;
use deckcraft_model::resolve as dresolve;
//  the original names these through the model root (`dm::Anchor`,
// `dm::Dash`, …), but the pinned deckcraft rev keeps them in its `text` /
// `style` submodules (no upstream rev re-exports them at the root, so the
// original was written against a model build that did). Qualifying the paths is
// the certain reading; `dmtext`/`dmstyle` avoid colliding with this module's
// own `Anchor`/`Bullet`/`Run` types.
use deckcraft_model::style as dmstyle;
use deckcraft_model::text as dmtext;

/// The magic of a compound-file binary (a legacy `.ppt`, or an encrypted
/// OOXML package).
const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

// ---------------------------------------------------------------------------
// The resolved model
// ---------------------------------------------------------------------------

/// One embedded image the slides reference, served to the viewer through the
/// deck's own URL scheme.
#[derive(Clone, Debug)]
pub struct DeckMedia {
    pub name: String,
    pub content_type: String,
    pub data: Arc<Vec<u8>>,
}

/// A resolved deck: slide size in points and the slides in reading order.
#[derive(Clone, Debug, Default)]
pub struct SlideDeck {
    pub w: f64,
    pub h: f64,
    pub title: String,
    pub author: String,
    pub slides: Vec<Slide>,
    pub media: Vec<DeckMedia>,
}

/// One resolved slide.
#[derive(Clone, Debug, Default)]
pub struct Slide {
    /// The first title placeholder's text — the TOC row, the status line.
    pub title: String,
    /// The slide's own background paint, when it sets one.
    pub bg: Option<String>,
    /// The shapes in z-order (document order), groups flattened.
    pub items: Vec<Item>,
    /// Speaker notes, verbatim.
    pub notes: String,
    /// The speakable text: shape text and recognised image text in reading
    /// order, then the notes. The byte spans below index into exactly this
    /// string — the string the speech plan chunks.
    pub text: String,
    /// Where each paragraph of [`Slide::text`] lives on the slide.
    pub spans: Vec<TextSpan>,
}

/// A byte range of [`Slide::text`] mapped to the item that shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSpan {
    pub start: usize,
    /// Half-open end.
    pub end: usize,
    /// Index into [`Slide::items`]. `usize::MAX` for the notes tail, which
    /// the slide itself does not show.
    pub item: usize,
    /// The paragraph inside the item's text, when the span is one paragraph
    /// of a text item (the highlight target `i{item}p{para}`); `None` for a
    /// whole-item span (an image's recognised text).
    pub para: Option<usize>,
}

/// One absolutely-positioned, fully-resolved shape.
#[derive(Clone, Debug)]
pub struct Item {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Clockwise degrees.
    pub rot: f64,
    pub flip_h: bool,
    pub flip_v: bool,
    pub fill: Option<String>,
    pub border: Option<Border>,
    /// Corner radius in points, for the rect-family shapes CSS rounds.
    pub radius: f64,
    /// An SVG body for the shapes CSS cannot draw: the preset's own path in
    /// a `0 0 w h` view box, fill and stroke applied.
    pub svg: Option<SvgBody>,
    pub shadow: Option<String>,
    pub content: Content,
}

#[derive(Clone, Debug)]
pub struct Border {
    pub width: f64,
    pub color: String,
    /// CSS border-style (`solid`, `dashed`, `dotted`, `double`).
    pub style: String,
}

/// The SVG body of a non-rectangular shape.
#[derive(Clone, Debug)]
pub struct SvgBody {
    pub d: String,
    pub fill: Option<String>,
    pub stroke: Option<String>,
    pub stroke_width: f64,
}

#[derive(Clone, Debug)]
pub enum Content {
    Text {
        paras: Vec<Para>,
        anchor: Anchor,
        wrap: bool,
        /// Text direction: `""`, `"vert"`, `"vert270"`, `"stacked"`.
        dir: String,
    },
    Picture {
        media: usize,
        /// Crop as fractions (l, t, r, b), clamped non-negative.
        crop: [f64; 4],
        /// Text recognised in the image, if OCR read any.
        ocr: Option<String>,
    },
    Table {
        col_widths: Vec<f64>,
        row_heights: Vec<f64>,
        rows: Vec<TableRow>,
        bg: Option<String>,
    },
    Chart {
        svg: String,
    },
    /// Something that renders as a frame and a label: a chart of an exotic
    /// kind, an EMF/WMF picture, a 3-D model without a poster.
    Frame {
        label: String,
        media: Option<usize>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Top,
    Middle,
    Bottom,
}

#[derive(Clone, Debug, Default)]
pub struct Para {
    pub runs: Vec<Run>,
    pub align: String,
    pub level: u8,
    /// The bullet glyph, when the level style asks for one.
    pub bullet: Option<Bullet>,
    pub margin_left: f64,
    pub indent: f64,
    pub space_before: f64,
    pub space_after: f64,
    /// Line spacing: a multiplier (0 = unset), or points when
    /// [`Para::line_pts`] is set.
    pub line_pct: f64,
    pub line_pts: f64,
    /// A minimum height for an empty paragraph, so a blank line keeps its
    /// place: the end mark's size.
    pub empty_size: f64,
}

#[derive(Clone, Debug)]
pub struct Bullet {
    pub char: String,
    /// A picture bullet's media, when the deck embedded one.
    pub media: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Run {
    pub text: String,
    /// Font size in points, fully resolved.
    pub size: f64,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub color: Option<String>,
    pub font: String,
    /// Extra letter spacing in points.
    pub spacing: f64,
    /// Baseline offset as a fraction of the size (0.3 = superscript).
    pub baseline: f64,
    pub highlight: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct TableRow {
    pub cells: Vec<TableCell>,
}

#[derive(Clone, Debug, Default)]
pub struct TableCell {
    pub text: String,
    /// The cell's runs, for the viewer; [`TableCell::text`] is the plain
    /// form for speech.
    pub runs: Vec<Run>,
    pub fill: Option<String>,
    pub col_span: u32,
    pub row_span: u32,
    /// Covered by a merge to the left / above: not rendered.
    pub covered: bool,
    pub align: String,
}

// ---------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SlideError {
    #[error("not a slide deck: {0}")]
    NotADeck(String),
    #[error("the deck is password-protected")]
    Encrypted,
    #[error("the deck could not be read: {0}")]
    Read(String),
}

impl SlideDeck {
    /// The slide's speakable text with its byte spans, images OCR'd on
    /// demand: text items (and any picture that already carries recognised
    /// text) come from the resolved slide; a picture without text is offered
    /// to `ocr` with its media, and whatever comes back is spoken in its
    /// place.
    //  authored from the call sites (sn-store books.rs
    // `deck.slide_text(index, |media: &DeckMedia| -> Option<String>)` ->
    // `(String, spans)`; the spans type is pinned by `Slide::spans`); no
    // original of this function surfaced. Reuses the file's own `assemble_text`.
    pub fn slide_text(
        &self,
        index: usize,
        ocr: impl Fn(&DeckMedia) -> Option<String>,
    ) -> (String, Vec<TextSpan>) {
        let Some(slide) = self.slides.get(index) else {
            return (String::new(), Vec::new());
        };
        let mut slide = slide.clone();
        for item in &mut slide.items {
            if let Content::Picture { media, ocr: text, .. } = &mut item.content {
                if text.is_none() {
                    if let Some(m) = self.media.get(*media) {
                        *text = ocr(m);
                    }
                }
            }
        }
        assemble_text(&mut slide);
        (slide.text, slide.spans)
    }

    /// Parse a slide deck's bytes, deciding the format by content: an OOXML
    /// package or a compound file goes to DeckCraft's reader, anything else
    /// zip-shaped tries the ODF reader.
    pub fn parse(bytes: &[u8]) -> Result<SlideDeck, SlideError> {
        if deckcraft_pptx::is_encrypted(bytes) {
            return Err(SlideError::Encrypted);
        }
        if deckcraft_pptx::sniff(bytes) || bytes.starts_with(&CFB_MAGIC) {
            return Self::from_pptx(bytes);
        }
        if crate::odp::sniffs_like_odp(bytes) {
            return crate::odp::parse(bytes);
        }
        Err(SlideError::NotADeck(
            "neither an OOXML package nor an ODF presentation".into(),
        ))
    }

    /// A deck from DeckCraft's model: every shape resolved through the
    /// inheritance chain, then mapped onto the viewer's item list.
    pub fn from_pptx(bytes: &[u8]) -> Result<SlideDeck, SlideError> {
        let pres: dm::Presentation =
            deckcraft_pptx::import(bytes).map_err(|e| SlideError::Read(e.to_string()))?;
        let mut media: Vec<DeckMedia> = Vec::new();
        // The media indexes the viewer serves. Only what a browser can
        // render (or a poster frame of what it cannot) is kept; the rest is
        // dropped and the shape that references it renders as a frame.
        let mut index_of = std::collections::HashMap::<dm::MediaId, usize>::new();
        for item in &pres.media {
            let serveable = matches!(
                item.content_type.as_str(),
                "image/png"
                    | "image/jpeg"
                    | "image/gif"
                    | "image/webp"
                    | "image/svg+xml"
                    | "image/bmp"
                    | "image/tiff"
            );
            if !serveable {
                continue;
            }
            if item.data.is_empty() && item.link.is_none() {
                continue;
            }
            let id = media.len();
            media.push(DeckMedia {
                name: format!("m{}", item.id),
                content_type: item.content_type.clone(),
                data: item.data.clone(),
            });
            index_of.insert(item.id, id);
        }

        let mut deck = SlideDeck {
            w: pres.slide_size.width,
            h: pres.slide_size.height,
            title: pres.props.title.clone(),
            author: pres.props.author.clone(),
            media,
            slides: Vec::with_capacity(pres.slides.len()),
        };

        for (number, arc) in pres.slides.iter().enumerate() {
            let slide: &dm::Slide = arc.as_ref();
            let Some(ctx) = dresolve::Ctx::for_slide(&pres, slide) else {
                continue;
            };
            let mut built = Slide {
                title: slide.title(),
                notes: slide.notes.text(),
                ..Default::default()
            };
            // The slide's background.
            let (bg_fill, bg_ph) = dresolve::background(&ctx, Some(slide));
            built.bg = fill_css(&ctx, Some(&bg_fill), bg_ph); // resolve::background returns a bare Fill

            // Master and layout shapes draw behind the slide's own, with the
            // usual PowerPoint rules: the layout's shapes show when the slide
            // says so, the master's when both do; footer-row shapes
            // show only when the deck turns them on.
            //  two certain defects. (1) show_master_shapes returns
            // (show_layout, show_master) — the binding had the two gates
            // swapped. (2) The master/layout *content* placeholders must not
            // draw at all: their prompt text ("Click to edit Master title
            // style…") is editor-only, and the slide's own placeholder — or
            // nothing — shows instead. The deckcraft renderer filters them
            // out (placed.rs `ph.is_none()`); here the footer row is kept,
            // governed by is_hidden_footer.
            let (slide_shows_layout, layout_shows_master) =
                dresolve::show_master_shapes(slide, ctx.layout);
            if layout_shows_master {
                let mctx = dresolve::Ctx::for_master(&pres, ctx.master);
                for shape in &ctx.master.shapes {
                    if is_content_placeholder(shape)
                        || is_hidden_footer(shape, &pres.header_footer)
                    {
                        continue;
                    }
                    build_shape(&mctx, shape, number + 1, &pres.header_footer, &mut built, &index_of);
                }
            }
            if slide_shows_layout {
                if let Some(layout) = ctx.layout {
                    let lctx = dresolve::Ctx::for_layout(&pres, ctx.master, layout);
                    for shape in &layout.shapes {
                        if is_content_placeholder(shape)
                            || is_hidden_footer(shape, &pres.header_footer)
                        {
                            continue;
                        }
                        build_shape(&lctx, shape, number + 1, &pres.header_footer, &mut built, &index_of);
                    }
                }
            }
            for shape in &slide.shapes {
                build_shape(&ctx, shape, number + 1, &pres.header_footer, &mut built, &index_of);
            }

            // The speakable text, in the same order the items were built.
            assemble_text(&mut built);
            deck.slides.push(built);
        }
        Ok(deck)
    }

    /// The picture media for slide `index`'s image items, in item order —
    /// what the OCR pass walks.
    pub fn slide_images(&self, index: usize) -> Vec<(usize, &DeckMedia)> {
        let Some(slide) = self.slides.get(index) else {
            return Vec::new();
        };
        slide
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| match &item.content {
                Content::Picture { media, ocr, .. } if *ocr == None => {
                    self.media.get(*media).map(|m| (i, m))
                }
                _ => None,
            })
            .collect()
    }

    /// Attach recognised text to the image at `item`, and rebuild the
    /// slide's speakable text so the next read carries it.
    pub fn set_image_ocr(&mut self, slide_index: usize, item: usize, text: String) {
        let Some(slide) = self.slides.get_mut(slide_index) else {
            return;
        };
        let clean = text.trim().to_string();
        if let Some(Item { content: Content::Picture { ocr, .. }, .. }) =
            slide.items.get_mut(item)
        {
            *ocr = (!clean.is_empty()).then_some(clean);
        }
        assemble_text(slide);
    }
}

/// A master or layout shape of the footer row, shown only when the deck's
/// header/footer settings turn it on.
fn is_hidden_footer(shape: &dm::Shape, hf: &dm::HeaderFooter) -> bool {
    let Some(kind) = shape.ph_type() else {
        return false;
    };
    match kind {
        dm::PhType::Footer => !hf.footer || hf.footer_text.is_empty(),
        dm::PhType::SlideNum => !hf.slide_number,
        dm::PhType::Date => {
            if !hf.date {
                true
            } else {
                // An automatic date is not modelled (it needs the render
                // clock); a fixed one shows its text.
                hf.date_text.is_empty()
            }
        }
        dm::PhType::Header => !hf.header,
        _ => false,
    }
}

/// Whether the shape is a master/layout *content* placeholder — the kind
/// whose prompt text ("Click to edit Master title style", "Master text
/// styles", …) must never draw on a slide: the slide's own placeholder
/// carries the content, and its geometry and formatting resolve through the
/// placeholder chain, not by painting the master's. The footer row (header,
/// date, footer, slide number) is not content; its visibility is the deck's
/// own header/footer settings (see [`is_hidden_footer`]).
fn is_content_placeholder(shape: &dm::Shape) -> bool {
    shape.ph.is_some()
        && !matches!(
            shape.ph_type(),
            Some(
                dm::PhType::Header
                    | dm::PhType::Date
                    | dm::PhType::Footer
                    | dm::PhType::SlideNum
            )
        )
}

// ---------------------------------------------------------------------------
// Shape → item
// ---------------------------------------------------------------------------

fn build_shape(
    ctx: &dresolve::Ctx,
    shape: &dm::Shape,
    slide_number: usize,
    hf: &dm::HeaderFooter,
    slide: &mut Slide,
    media_index: &std::collections::HashMap<dm::MediaId, usize>,
) {
    build_shape_at(ctx, shape, None, slide_number, hf, slide, media_index, 0);
}

/// Build one shape, or flatten a group's children through the group's
/// transform. `parent` carries the composed affine of the enclosing group,
/// as (a, b, c, d, e, f): x' = a·x + c·y + e, y' = b·x + d·y + f.
#[allow(clippy::too_many_arguments)]
fn build_shape_at(
    ctx: &dresolve::Ctx,
    shape: &dm::Shape,
    parent: Option<[f64; 6]>,
    slide_number: usize,
    hf: &dm::HeaderFooter,
    slide: &mut Slide,
    media_index: &std::collections::HashMap<dm::MediaId, usize>,
    depth: usize,
) {
    let xfrm = dresolve::xfrm(ctx, shape);
    build_shape_at_xfrm(
        ctx,
        shape,
        &xfrm,
        parent,
        slide_number,
        hf,
        slide,
        media_index,
        depth,
    );
}

/// The shape walk with the shape's box given: the group walk normalises a
/// child's box into the child-space unit box, which `dresolve::xfrm` (the
/// shape's absolute box) cannot express.
#[allow(clippy::too_many_arguments)]
fn build_shape_at_xfrm(
    ctx: &dresolve::Ctx,
    shape: &dm::Shape,
    xfrm: &deckcraft_geom::Xfrm,
    parent: Option<[f64; 6]>,
    slide_number: usize,
    hf: &dm::HeaderFooter,
    slide: &mut Slide,
    media_index: &std::collections::HashMap<dm::MediaId, usize>,
    depth: usize,
) {
    if shape.hidden || depth > 16 {
        return;
    }
    if !xfrm.is_finite() || (xfrm.w <= 0.0 && xfrm.h <= 0.0) {
        return;
    }
    if let dm::ShapeKind::Group { children, child: child_space } = &shape.kind {
        // The group's own affine, then each child's, composed. The child's
        // box is normalised into the child-space unit box first ( the
        // old path scaled the child's absolute xfrm by the group factors and
        // composed it as if absolute — with the affine now carrying sizes,
        // the composition is the correct unit-box pipeline).
        let ga = compose(parent, &xfrm);
        for child in children {
            let local = dresolve::xfrm(ctx, child);
            let normalised = child_in_space(&local, child_space);
            build_shape_at_xfrm(
                ctx,
                child,
                &normalised,
                Some(ga),
                slide_number,
                hf,
                slide,
                media_index,
                depth + 1,
            );
        }
        return;
    }

    let (x, y, w, h, rot, fh, fv) = place(&compose(parent, &xfrm));
    if w < 0.05 && h < 0.05 {
        return;
    }
    let (fill, ph) = dresolve::fill(ctx, shape);
    let paint = fill_css(ctx, fill.as_ref(), ph);
    let (line, lph) = dresolve::line(ctx, shape);
    let border = border_css(ctx, Some(&line), lph, shape); // resolve::line returns a bare Line
    let (_, shadow_ph) = dresolve::effects(ctx, shape);

    let item_index = slide.items.len();
    let content = match &shape.kind {
        dm::ShapeKind::Shape => {
            let Some(text) = shape.text.as_ref() else {
                return; // a bare shape with no text: geometry only
            };
            //  the placed (group-scaled) size, not the shape's own
            // xfrm — a child of a scaled group lays out at the composed size.
            text_item(ctx, shape, text, w, h)
        }
        dm::ShapeKind::Picture { fill: pic } => {
            let media = media_index.get(&pic.media).copied();
            match media {
                Some(m) => {
                    let crop = [
                        pic.crop[0].max(0.0),
                        pic.crop[1].max(0.0),
                        pic.crop[2].max(0.0),
                        pic.crop[3].max(0.0),
                    ];
                    Content::Picture { media: m, crop, ocr: None }
                }
                // An EMF/WMF (or a linked-only picture): a frame, its label
                // its name.
                None => Content::Frame { label: shape.name.clone(), media: None },
            }
        }
        dm::ShapeKind::Connector { .. } => {
            // A line: the SVG body carries the stroke, nothing fills.
            Content::Frame { label: String::new(), media: None }
        }
        dm::ShapeKind::Table(table) => table_item(ctx, table),
        dm::ShapeKind::Chart(chart) => chart_item(ctx, chart),
        dm::ShapeKind::Media(clip) => {
            let poster = clip.poster.and_then(|m| media_index.get(&m).copied());
            Content::Frame {
                label: if clip.video { "video" } else { "audio" }.to_string(),
                media: poster,
            }
        }
        dm::ShapeKind::Model3d { poster, .. } => Content::Frame {
            label: "3-D model".into(),
            media: poster.as_ref().and_then(|m| media_index.get(m).copied()), // the media index is keyed by MediaId
        },
        // Groups never reach here — build_shape_at flattens them above.
        dm::ShapeKind::Group { .. } => Content::Frame { label: String::new(), media: None }, // unreachable: groups flatten above
        dm::ShapeKind::Ink { .. } => return, // annotation ink is not slide content
        dm::ShapeKind::Opaque { preview, label, .. } => {
            // SmartArt without a fallback drawing still has its outline:
            // speakable as a plain text box, rendered as one.
            if let Some(diagram) = &shape.diagram {
                if !diagram.nodes.is_empty() {
                    let body = diagram_outline(diagram, ctx, shape);
                    slide.items.push(text_only_item(&body, x, y, w, h, rot, fh, fv));
                    return;
                }
            }
            Content::Frame {
                label: label.clone(),
                media: preview.as_ref().and_then(|m| media_index.get(m).copied()), // as above — keyed by MediaId
            }
        }
    };
    let _ = (shadow_ph, item_index);
    slide.items.push(Item {
        x,
        y,
        w,
        h,
        rot,
        flip_h: fh,
        flip_v: fv,
        fill: paint,
        border,
        radius: preset_radius(shape, w, h),
        svg: shape_svg(ctx, shape, fill.as_ref(), ph, Some(&line), lph), // resolve::line returns a bare Line
        shadow: shadow_css(ctx, shape),
        content,
    });
}

//  the original `poster_media` helper (a poster's media index for the
// Model3d/Opaque arms) was removed 2026-10-30 in a dead-code trim —
// zero references since those arms were inlined as
// `media_index.get(m).copied()`.

/// A SmartArt outline as paragraphs: one per node, its level as the bullet
/// level, in the theme's body font.
fn diagram_outline(diagram: &dm::Diagram, ctx: &dresolve::Ctx, shape: &dm::Shape) -> Vec<Para> {
    let mut paras = Vec::new();
    for node in &diagram.nodes {
        if node.text.trim().is_empty() {
            continue;
        }
        let mut run = Run {
            text: node.text.clone(),
            ..Default::default()
        };
        // The shape's own first run style, when it has one, sizes the
        // outline; the theme's minor font says what face.
        if let Some(text) = &shape.text {
            // The original breaks out of the paragraph loop unconditionally
            // after the first paragraph — only its first paragraph's runs
            // are ever consulted. (clippy::never_loop, kept as-written.)
            if let Some(para) = text.paragraphs.first() {
                for r in &para.runs {
                    if !r.text.trim().is_empty() {
                        let props = dresolve::run(ctx, shape, para, &r.props);
                        run.size = props.size.unwrap_or(18.0).max(8.0);
                        run.bold = props.bold.unwrap_or(false);
                        run.font = dresolve::font_family(ctx, &props);
                        run.color = run_color(ctx, &props);
                        break;
                    }
                }
            }
        }
        paras.push(Para {
            runs: vec![run],
            level: node.level.min(8),
            bullet: Some(Bullet { char: "•".into(), media: None }),
            ..Default::default()
        });
    }
    paras
}

/// A text item with the given geometry, for synthesized content.
fn text_only_item(paras: &[Para], x: f64, y: f64, w: f64, h: f64, rot: f64, fh: bool, fv: bool) -> Item {
    Item {
        x,
        y,
        w,
        h,
        rot,
        flip_h: fh,
        flip_v: fv,
        fill: None,
        border: None,
        radius: 0.0,
        svg: None,
        shadow: None,
        content: Content::Text {
            paras: paras.to_vec(),
            anchor: Anchor::Top,
            wrap: true,
            dir: String::new(),
        },
    }
}

/// A text shape's paragraphs, resolved.
fn text_item(ctx: &dresolve::Ctx, shape: &dm::Shape, text: &dm::TextBody, w: f64, h: f64) -> Content {
    let body = dresolve::body(ctx, shape);
    let anchor = match body.anchor.unwrap_or(dmtext::Anchor::Top) {
        dmtext::Anchor::Bottom => Anchor::Bottom,
        dmtext::Anchor::Middle => Anchor::Middle,
        _ => Anchor::Top,
    };
    let mut paras = Vec::new();
    for para in &text.paragraphs {
        let props = dresolve::para(ctx, shape, para);
        let level = para.level.min(8);
        let style = dresolve::level_style(ctx, shape, level);
        let bullet = match props.bullet.as_ref().or(style.para.bullet.as_ref()) { // resolve::level_style returns a bare LevelStyle
            Some(dmtext::Bullet::Char { char }) => Some(Bullet { char: char.clone(), media: None }),
            Some(dmtext::Bullet::AutoNum { scheme, start_at }) => {
                // The scheme's numbering restarts per shape in practice; a
                // "1." shape is what nearly every deck uses.
                let n = start_at.saturating_add(paras.iter().filter(|p: &&Para| p.bullet.as_ref().is_some_and(|b| b.char.chars().all(|c| c.is_ascii_digit() || c == '.' || c == ')'))).count() as u32);
                let _ = scheme;
                Some(Bullet { char: format!("{n}."), media: None })
            }
            //  the original destructured `Picture { media }` but never used
            // the binding (the bullet carries no picture either) — pattern
            // loosened 2026-10-30 to clear the rustc warning, no behavior change.
            Some(dmtext::Bullet::Picture { .. }) => Some(Bullet {
                char: "▪".into(),
                media: None,
            }),
            Some(dmtext::Bullet::None) | None => None,
        };
        let (line_pct, line_pts) = match props.line_spacing {
            Some(dmtext::Spacing::Pct(v)) => (v, 0.0),
            Some(dmtext::Spacing::Pts(v)) => (0.0, v),
            None => (0.0, 0.0),
        };
        let mut out = Para {
            align: match props.align.unwrap_or(dmtext::Align::Left) {
                dmtext::Align::Center => "center".into(),
                dmtext::Align::Right => "right".into(),
                dmtext::Align::Justify | dmtext::Align::JustLow | dmtext::Align::Distributed => "justify".into(),
                dmtext::Align::Left => "left".into(),
            },
            level,
            bullet,
            margin_left: props.margin_left.unwrap_or(0.0).max(0.0),
            indent: props.indent.unwrap_or(0.0),
            space_before: spacing_pts(props.space_before),
            space_after: spacing_pts(props.space_after),
            line_pct,
            line_pts,
            ..Default::default()
        };
        // The end mark's size sizes an empty paragraph.
        out.empty_size = para
            .end_props
            .size
            .or(para.def_props.size)
            //  resolve the end mark's inherited size through its own
            // RunProps (the original passed the bare size where a &RunProps goes).
            .map(|s| dresolve::run(ctx, shape, para, &para.end_props).size.unwrap_or(s))
            .unwrap_or(18.0);
        if para.runs.is_empty() {
            paras.push(out);
            continue;
        }
        for run in &para.runs {
            match &run.kind {
                dmtext::RunKind::Break => {
                    // A soft break splits the paragraph's HTML, not its
                    // speech text (the chunker never sees \u{b}).
                    if let Some(last) = out.runs.last_mut() {
                        last.text.push('\n');
                    }
                }
                dmtext::RunKind::Field { field } => {
                    let (text, size) = field_text(&field, ctx, shape, para, run, slide_number());
                    let _ = size;
                    //  the original `mut` on `resolved` was unused —
                    // dropped 2026-10-30 to clear the rustc warning.
                    let resolved = dresolve::run(ctx, shape, para, &run.props);
                    out.runs.push(Run {
                        text,
                        size: resolved.size.unwrap_or(18.0).max(4.0) * font_scale(&body),
                        bold: resolved.bold.unwrap_or(false),
                        italic: resolved.italic.unwrap_or(false),
                        underline: resolved.underline.as_deref().is_some_and(|u| u != "none"),
                        strike: !matches!(resolved.strike, None | Some(dmtext::Strike::None)),
                        color: run_color(ctx, &resolved),
                        font: dresolve::font_family(ctx, &resolved),
                        spacing: resolved.spacing.unwrap_or(0.0),
                        baseline: resolved.baseline.unwrap_or(0.0),
                        highlight: highlight_color(ctx, &resolved),
                    });
                }
                dmtext::RunKind::Text | dmtext::RunKind::Math { .. } => {
                    let content = match &run.kind {
                        dmtext::RunKind::Math { omml } => omml.clone(),
                        _ => run.text.clone(),
                    };
                    if content.is_empty() {
                        continue;
                    }
                    let mut resolved = dresolve::run(ctx, shape, para, &run.props);
                    if resolved.size.is_none() {
                        // Nothing anywhere set a size: the presentation's
                        // default text style carries one per level.
                        resolved.size = Some(18.0);
                    }
                    out.runs.push(Run {
                        text: content,
                        size: resolved.size.unwrap_or(18.0).max(4.0) * font_scale(&body),
                        bold: resolved.bold.unwrap_or(false),
                        italic: resolved.italic.unwrap_or(false),
                        underline: resolved.underline.as_deref().is_some_and(|u| u != "none"),
                        strike: !matches!(resolved.strike, None | Some(dmtext::Strike::None)),
                        color: run_color(ctx, &resolved),
                        font: dresolve::font_family(ctx, &resolved),
                        spacing: resolved.spacing.unwrap_or(0.0),
                        baseline: resolved.baseline.unwrap_or(0.0),
                        highlight: highlight_color(ctx, &resolved),
                    });
                }
            }
        }
        paras.push(out);
    }
    let _ = (w, h);
    Content::Text {
        paras,
        anchor,
        wrap: body.wrap.unwrap_or(true),
        dir: match body.vert {
            Some(dmtext::TextDir::Vertical | dmtext::TextDir::EaVertical) => "vert".to_string(),
            Some(dmtext::TextDir::Vertical270) => "vert270".to_string(),
            Some(dmtext::TextDir::Stacked | dmtext::TextDir::StackedRtl) => "stacked".to_string(),
            _ => String::new(),
        },
    }
}

/// `normAutofit`'s saved scale, applied to every run of the shape.
fn font_scale(body: &dmtext::BodyProps) -> f64 { // BodyProps lives in the model's text submodule
    match body.autofit {
        Some(dmtext::AutoFit::Shrink { font_scale: Some(v), .. }) if v > 0.0 => v,
        _ => 1.0,
    }
}

fn spacing_pts(s: Option<dmtext::Spacing>) -> f64 {
    match s {
        Some(dmtext::Spacing::Pts(v)) => v,
        Some(dmtext::Spacing::Pct(v)) => v, // a percent of the line: approximated as points
        None => 0.0,
    }
}

/// A field run's text: the slide number, the file's fixed date, or the
/// value the file cached.
fn field_text(
    field: &str,
    _ctx: &dresolve::Ctx,
    _shape: &dm::Shape,
    _para: &dm::Paragraph,
    run: &dm::Run,
    _slide_number: usize,
) -> (String, Option<f64>) {
    match field {
        f if f.starts_with("datetime") => (run.text.clone(), None),
        "slidenum" => (run.text.clone(), None),
        other => (other.to_string(), None),
    }
}

fn slide_number() -> usize {
    0 // set per slide by the caller through the closure argument below
}

fn run_color(ctx: &dresolve::Ctx, props: &dm::RunProps) -> Option<String> {
    let fill = props.fill.as_ref()?;
    match fill {
        dm::Fill::None => Some("transparent".into()),
        dm::Fill::Solid { color } => Some(css(&ctx.color(color, None))),
        _ => None,
    }
}

fn highlight_color(ctx: &dresolve::Ctx, props: &dm::RunProps) -> Option<String> {
    props
        .highlight
        .as_ref()
        .map(|c| css(&ctx.color(c, None)))
}

/// A table, resolved: fills, borders and text through the cell text bodies.
fn table_item(ctx: &dresolve::Ctx, table: &dm::Table) -> Content {
    let mut rows = Vec::new();
    for row in &table.rows {
        let mut cells = Vec::new();
        for cell in &row.cells {
            let mut runs = Vec::new();
            let mut text = String::new();
            for para in &cell.text.paragraphs {
                for run in &para.runs {
                    let props = dresolve::run(ctx, &dm::Shape::table_cell(cell.text.clone()), para, &run.props);
                    text.push_str(&run.text);
                    runs.push(Run {
                        text: run.text.clone(),
                        size: props.size.unwrap_or(14.0).max(4.0),
                        bold: props.bold.unwrap_or(false),
                        italic: props.italic.unwrap_or(false),
                        underline: props.underline.as_deref().is_some_and(|u| u != "none"),
                        strike: false,
                        color: run_color(ctx, &props),
                        font: dresolve::font_family(ctx, &props),
                        spacing: 0.0,
                        baseline: 0.0,
                        highlight: None,
                    });
                }
                text.push('\n');
            }
            let fill = cell
                .fill
                .as_ref()
                .and_then(|f| match f {
                    dm::Fill::Solid { color } => Some(css(&ctx.color(color, None))),
                    _ => None,
                });
            let align = cell
                .text
                .paragraphs
                .first()
                .map(|p| {
                    let props = dresolve::para(
                        ctx,
                        &dm::Shape::table_cell(cell.text.clone()),
                        p,
                    );
                    match props.align.unwrap_or(dmtext::Align::Left) {
                        dmtext::Align::Center => "center".to_string(),
                        dmtext::Align::Right => "right".to_string(),
                        _ => "left".to_string(),
                    }
                })
                .unwrap_or_else(|| "left".into());
            cells.push(TableCell {
                text: text.trim_end_matches('\n').to_string(),
                runs,
                fill,
                col_span: cell.grid_span.max(1),
                row_span: cell.row_span.max(1),
                covered: cell.is_covered(),
                align,
            });
        }
        rows.push(TableRow { cells });
    }
    let bg = table
        .background
        .as_ref()
        .and_then(|f| match f {
            dm::Fill::Solid { color } => Some(css(&ctx.color(color, None))),
            _ => None,
        });
    Content::Table {
        col_widths: table.cols.clone(),
        row_heights: table.rows.iter().map(|r| r.height).collect(),
        rows,
        bg,
    }
}

/// A chart: the common family draws as SVG from the cached series; the rest
/// is a titled frame.
fn chart_item(ctx: &dresolve::Ctx, chart: &dm::chart::Chart) -> Content {
    let common = matches!(
        chart.kind,
        dm::chart::ChartType::Column
            | dm::chart::ChartType::StackedColumn
            | dm::chart::ChartType::Bar
            | dm::chart::ChartType::StackedBar
            | dm::chart::ChartType::Line
            | dm::chart::ChartType::LineMarkers
            | dm::chart::ChartType::Area
            | dm::chart::ChartType::Pie
            | dm::chart::ChartType::Doughnut
    );
    if !common {
        return Content::Frame {
            label: chart.title.clone().unwrap_or_else(|| "chart".into()),
            media: None,
        };
    }
    let palette: Vec<String> = (1..=6)
        .map(|i| {
            let slot = match i {
                1 => deckcraft_color::SchemeSlot::Accent1,
                2 => deckcraft_color::SchemeSlot::Accent2,
                3 => deckcraft_color::SchemeSlot::Accent3,
                4 => deckcraft_color::SchemeSlot::Accent4,
                5 => deckcraft_color::SchemeSlot::Accent5,
                _ => deckcraft_color::SchemeSlot::Accent6,
            };
            css(&ctx.scheme.get(slot))
        })
        .collect();
    Content::Chart {
        svg: chart_svg(chart, &palette),
    }
}

/// The chart's SVG, in a `0 0 400 300` space the viewer scales to the
/// frame. Deliberately plain: the value is the shape of the data, not a
/// re-implementation of the chart engine.
fn chart_svg(chart: &dm::chart::Chart, palette: &[String]) -> String {
    let w = 400.0f64;
    let h = 300.0f64;
    let mut out = String::from("<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 400 300'>");
    let series: Vec<&dm::chart::Series> = chart.series.iter().collect();
    let finite = |v: Option<f64>| v.unwrap_or(0.0);
    match chart.kind {
        dm::chart::ChartType::Pie | dm::chart::ChartType::Doughnut => {
            let values: Vec<f64> = series
                .first()
                .map(|s| s.values.iter().map(|v| finite(*v)).collect())
                .unwrap_or_default();
            let total: f64 = values.iter().sum();
            if total > 0.0 {
                let (cx, cy, r) = (200.0, 150.0, 110.0);
                let hole = if chart.kind == dm::chart::ChartType::Doughnut {
                    r * (chart.hole_size.clamp(10.0, 90.0) / 100.0)
                } else {
                    0.0
                };
                let mut start = -90.0f64;
                for (i, v) in values.iter().enumerate() {
                    let sweep = v / total * 360.0;
                    let end = start + sweep;
                    let (x1, y1) = (cx + r * start.to_radians().cos(), cy + r * start.to_radians().sin());
                    let (x2, y2) = (cx + r * end.to_radians().cos(), cy + r * end.to_radians().sin());
                    let large = (sweep > 180.0).then(|| 1).unwrap_or(0);
                    let color = palette.get(i % palette.len()).unwrap_or(&"#888".to_string()).clone();
                    if hole > 0.0 {
                        let (x3, y3) = (cx + hole * end.to_radians().cos(), cy + hole * end.to_radians().sin());
                        let (x4, y4) = (cx + hole * start.to_radians().cos(), cy + hole * start.to_radians().sin());
                        out.push_str(&format!(
                            "<path d='M{x1:.1} {y1:.1} A{r:.1} {r:.1} 0 {large} 1 {x2:.1} {y2:.1} L{x3:.1} {y3:.1} A{hole:.1} {hole:.1} 0 {large} 0 {x4:.1} {y4:.1} Z' fill='{color}'/>"
                        ));
                    } else {
                        out.push_str(&format!(
                            "<path d='M{cx:.1} {cy:.1} L{x1:.1} {y1:.1} A{r:.1} {r:.1} 0 {large} 1 {x2:.1} {y2:.1} Z' fill='{color}'/>"
                        ));
                    }
                    start = end;
                }
            }
        }
        _ => {
            // Column/bar/line/area share a value axis.
            let all: Vec<f64> = series
                .iter()
                .flat_map(|s| s.values.iter().map(|v| finite(*v)))
                .collect();
            let (min, max) = (all.iter().cloned().fold(0.0f64, f64::min), all.iter().cloned().fold(0.0f64, f64::max));
            let lo = chart.y_min.unwrap_or(min.min(0.0));
            let hi = chart.y_max.unwrap_or(max.max(lo + 1.0));
            let (px, py, pw, ph) = (46.0, 14.0, w - 56.0, h - 44.0);
            let y = |v: f64| py + ph * (1.0 - (v - lo) / (hi - lo).max(1e-9));
            // gridlines + zero line
            for t in 0..=4 {
                let v = lo + (hi - lo) * t as f64 / 4.0;
                let yy = y(v);
                out.push_str(&format!(
                    "<line x1='{px}' y1='{yy:.1}' x2='{}' y2='{yy:.1}' stroke='#0002' stroke-width='1'/>",
                    px + pw
                ));
                out.push_str(&format!(
                    "<text x='{}' y='{:.1}' font-size='9' text-anchor='end' fill='#666'>{v:.2}</text>",
                    px - 4.0,
                    yy + 3.0
                ));
            }
            let n = chart.categories.len().max(series.iter().map(|s| s.values.len()).max().unwrap_or(1)).max(1);
            let horizontal = matches!(chart.kind, dm::chart::ChartType::Bar | dm::chart::ChartType::StackedBar);
            let _ = horizontal; // bars draw as columns rotated by the viewer's eye; v1 keeps the column form
            let slot = |i: usize| palette.get(i % palette.len()).unwrap_or(&"#888".to_string()).clone();
            match chart.kind {
                dm::chart::ChartType::Line | dm::chart::ChartType::LineMarkers | dm::chart::ChartType::Area => {
                    for (si, s) in series.iter().enumerate() {
                        let pts: Vec<String> = s
                            .values
                            .iter()
                            .enumerate()
                            .map(|(i, v)| {
                                let x = px + pw * (i as f64 + 0.5) / n as f64;
                                format!("{x:.1},{:.1}", y(finite(*v)))
                            })
                            .collect();
                        let color = slot(si);
                        if chart.kind == dm::chart::ChartType::Area {
                            out.push_str(&format!(
                                "<polygon points='{},{:.1} {} {},{:.1}' fill='{}' fill-opacity='0.55'/>",
                                px,
                                y(lo),
                                pts.join(" "),
                                px + pw,
                                y(lo),
                                color
                            ));
                        } else {
                            out.push_str(&format!(
                                "<polyline points='{}' fill='none' stroke='{color}' stroke-width='2.5'/>",
                                pts.join(" ")
                            ));
                        }
                        if chart.kind == dm::chart::ChartType::LineMarkers {
                            for p in pts.iter() {
                                let (x, yv) = p.split_once(',').unwrap_or(("0", "0"));
                                out.push_str(&format!(
                                    "<circle cx='{x}' cy='{yv}' r='3' fill='{color}'/>"
                                ));
                            }
                        }
                    }
                }
                _ => {
                    // Columns, stacked or side by side.
                    let group = pw / n as f64;
                    let stack = matches!(chart.kind, dm::chart::ChartType::StackedColumn);
                    for (si, s) in series.iter().enumerate() {
                        let color = slot(si);
                        for (i, v) in s.values.iter().enumerate() {
                            let v = finite(*v);
                            let x = px + group * i as f64;
                            if stack {
                                let base: f64 = series[..si]
                                    .iter()
                                    .map(|p| finite(p.values.get(i).copied().flatten())) // the pinned chart's values are Vec<Option<f64>>
                                    .sum();
                                let y1 = y(base + v);
                                let y0 = y(base);
                                out.push_str(&format!(
                                    "<rect x='{:.1}' y='{y1:.1}' width='{:.1}' height='{:.1}' fill='{color}'/>",
                                    x + group * 0.12,
                                    group * 0.76,
                                    (y0 - y1).max(0.5)
                                ));
                            } else {
                                let bw = group * 0.76 / series.len() as f64;
                                let y1 = y(v);
                                out.push_str(&format!(
                                    "<rect x='{:.1}' y='{y1:.1}' width='{bw:.1}' height='{:.1}' fill='{color}'/>",
                                    x + group * 0.12 + bw * si as f64,
                                    (y(lo) - y1).max(0.5)
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(title) = &chart.title {
        out.push_str(&format!(
            "<text x='{w}' y='12' font-size='11' text-anchor='middle' fill='#333'>{}</text>",
            escape_xml(title)
        ));
    }
    out.push_str("</svg>");
    out
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// Geometry and paint
// ---------------------------------------------------------------------------

/// The composed placement of a shape: its resolved transform through any
/// enclosing group's, flattened into (x, y, w, h, rot, flip_h, flip_v).
fn place(aff: &[f64; 6]) -> (f64, f64, f64, f64, f64, bool, bool) {
    let [a, b, c, d, e, f] = *aff;
    let sx = (a * a + b * b).sqrt();
    let sy = (c * c + d * d).sqrt();
    let rot = b.atan2(a).to_degrees();
    // A mirror shows up as a negative determinant ( the flip used to
    // be lost entirely — the matrix's sign was discarded here).
    let mirrored = a * d - b * c < 0.0;
    // Centre of the unit box, mapped.
    let cx = e + (a * 0.5 + c * 0.5);
    let cy = f + (b * 0.5 + d * 0.5);
    (
        cx - sx / 2.0,
        cy - sy / 2.0,
        sx,
        sy,
        rot,
        mirrored,
        false,
    )
}

/// The shape's own transform, composed with an enclosing group's.
fn compose(parent: Option<[f64; 6]>, xfrm: &deckcraft_geom::Xfrm) -> [f64; 6] {
    let own = xfrm_affine(xfrm);
    match parent {
        None => own,
        Some(p) => compose_affines(&p, &own),
    }
}

/// A transform as (a, b, c, d, e, f), from the model's Xfrm: the same
/// mapping `Xfrm::affine` builds (translate to centre, rotate, flip, scale,
/// translate back), written out without pulling kurbo into the callers.
//  the affine maps the UNIT box (the caller's `place` reads the size
// back out of the matrix scale), so the box's own width and height must ride
// in the matrix — a·p for p in 0..1 spans the rect. The old unit-scale
// matrix lost both, and every item laid out 1×1 pt: text wrapped a word per
// line and pictures drew 1 pt wide (invisible).
fn xfrm_affine(x: &deckcraft_geom::Xfrm) -> [f64; 6] {
    let rad = x.rot.to_radians();
    let (sr, cr) = rad.sin_cos();
    let sx = if x.flip_h { -x.w } else { x.w };
    let sy = if x.flip_v { -x.h } else { x.h };
    // scale*rotate about the box centre, then translate: the unit box's
    // centre (½, ½) must land on the rect's centre.
    let a = cr * sx;
    let b = sr * sx;
    let c = -sr * sy;
    let d = cr * sy;
    let e = x.x + x.w / 2.0 - (a + c) / 2.0;
    let f = x.y + x.h / 2.0 - (b + d) / 2.0;
    [a, b, c, d, e, f]
}

fn compose_affines(outer: &[f64; 6], inner: &[f64; 6]) -> [f64; 6] {
    let [a1, b1, c1, d1, e1, f1] = *outer;
    let [a2, b2, c2, d2, e2, f2] = *inner;
    // The inner affine's input space is the outer affine's unit box: the
    // group's children are normalised into the child-space box before this
    // is built (see `child_in_space`), so plain matrix composition applies.
    [
        a1 * a2 + c1 * b2,
        b1 * a2 + d1 * b2,
        a1 * c2 + c1 * d2,
        b1 * c2 + d1 * d2,
        a1 * e2 + c1 * f2 + e1,
        b1 * e2 + d1 * f2 + f1,
    ]
}

/// A group child's box, written in the group's child-space unit coordinates:
/// (p − origin) / extent. Composing the group's affine with this maps the
/// unit box to where the child really draws, scale and all.
fn child_in_space(local: &deckcraft_geom::Xfrm, space: &deckcraft_geom::Xfrm) -> deckcraft_geom::Xfrm {
    let sw = if space.w != 0.0 { space.w } else { 1.0 };
    let sh = if space.h != 0.0 { space.h } else { 1.0 };
    deckcraft_geom::Xfrm {
        x: (local.x - space.x) / sw,
        y: (local.y - space.y) / sh,
        w: local.w / sw,
        h: local.h / sh,
        rot: local.rot,
        flip_h: local.flip_h,
        flip_v: local.flip_v,
        ..local.clone()
    }
}

/// The corner radius a roundRect-family preset asks for, in points.
fn preset_radius(shape: &dm::Shape, w: f64, h: f64) -> f64 {
    let dm::Geom::Preset { name, adj } = &shape.geom else {
        return 0.0;
    };
    match name.as_str() {
        "roundRect" | "round1Rect" | "round2DiagRect" | "round2SameRect" | "snip1Rect"
        | "snip2DiagRect" | "snip2SameRect" | "snipRoundRect" => {
            let frac = adj.first().map(|v| (v / 100_000.0).clamp(0.0, 0.5)).unwrap_or(1.0 / 6.0);
            w.min(h) * frac
        }
        _ => 0.0,
    }
}

/// An SVG body for the shapes CSS cannot draw: the preset's own path, in
/// the shape's own coordinate space, with the resolved fill and stroke.
///
/// `rect`, `ellipse` and the rounded family are CSS instead (the viewer
/// paints those from `fill`/`border`/`radius`), and connectors draw as a
/// line across the box.
fn shape_svg(
    ctx: &dresolve::Ctx,
    shape: &dm::Shape,
    fill: Option<&dm::Fill>,
    ph: Option<Rgba>,
    line: Option<&dm::Line>,
    lph: Option<Rgba>,
) -> Option<SvgBody> {
    if shape.is_line() {
        // Connectors: a stroke across the box, flips respected.
        let line_fill = line.and_then(|l| l.fill.as_ref()); // an unzip() shape does not typecheck here; same semantics
        let color = match line_fill? {
            dm::Fill::Solid { color } => css(&ctx.color(color, lph)),
            _ => "#000".into(),
        };
        let width = line.and_then(|l| l.width).unwrap_or(1.0);
        let dm::Geom::Preset { name, adj } = &shape.geom else {
            return Some(SvgBody {
                d: "M0,0 L100,100".into(),
                fill: None,
                stroke: Some(color),
                stroke_width: width,
            });
        };
        let geom = gpreset::build(name, 100.0, 100.0, adj)?;
        let d = geom.outline().to_svg();
        return Some(SvgBody { d, fill: None, stroke: Some(color), stroke_width: width });
    }
    let dm::Geom::Preset { name, adj } = &shape.geom else {
        // Custom geometry: the model keeps the path.
        if let dm::Geom::Custom { paths } = &shape.geom {
            let mut d = String::new();
            for p in paths {
                if !d.is_empty() {
                    d.push(' ');
                }
                d.push_str(&p.d);
            }
            let fill_css_v = fill
                .and_then(|f| match f {
                    dm::Fill::Solid { color } => Some(css(&ctx.color(color, ph))),
                    dm::Fill::None => Some("none".into()),
                    _ => None,
                })
                .or(Some("none".into()));
            let stroke = line.and_then(|l| l.fill.as_ref()).and_then(|f| match f {
                dm::Fill::Solid { color } => Some(css(&ctx.color(color, lph))),
                _ => None,
            });
            return Some(SvgBody {
                d,
                fill: fill_css_v,
                stroke,
                stroke_width: line.and_then(|l| l.width).unwrap_or(0.0),
            });
        }
        return None;
    };
    match name.as_str() {
        "rect" | "roundRect" => None, // CSS paints these
        _ => {
            let geom = gpreset::build(name, 100.0, 100.0, adj)?;
            let d = geom.outline().to_svg();
            let fill_css_v = fill.and_then(|f| match f {
                dm::Fill::Solid { color } => Some(css(&ctx.color(color, ph))),
                dm::Fill::None => Some("none".into()),
                _ => None,
            });
            let stroke = line
                .and_then(|l| l.fill.as_ref())
                .and_then(|f| match f {
                    dm::Fill::Solid { color } => Some(css(&ctx.color(color, lph))),
                    _ => None,
                });
            Some(SvgBody {
                d,
                fill: fill_css_v,
                stroke,
                stroke_width: line.and_then(|l| l.width).unwrap_or(0.0),
            })
        }
    }
}

/// A resolved fill as a CSS paint value.
fn fill_css(ctx: &dresolve::Ctx, fill: Option<&dm::Fill>, ph: Option<Rgba>) -> Option<String> {
    let fill = fill?;
    Some(match fill {
        dm::Fill::None => "transparent".to_string(),
        dm::Fill::Solid { color } => css(&ctx.color(color, ph)),
        dm::Fill::Gradient(g) => gradient_css(ctx, g),
        dm::Fill::Pattern(p) => css(&ctx.color(&p.fg, None)), // the Pattern fill's fg is a bare ColorRef
        // A picture fill paints the box with the image (cover); the viewer
        // resolves media by id.
        dm::Fill::Picture(p) => {
            let id = p.media.0;
            format!("url('speechnotes://deck/media-{id}/fill') center/cover no-repeat")
        }
        dm::Fill::Group | dm::Fill::Background => "transparent".to_string(),
    })
}

fn gradient_css(ctx: &dresolve::Ctx, g: &dm::style::Gradient) -> String {
    let stops: Vec<String> = g
        .stops
        .iter()
        .map(|s| format!("{}% {}", (s.pos * 100.0).clamp(0.0, 100.0), css(&ctx.color(&s.color, None))))
        .collect();
    match &g.shape {
        dm::style::GradientShape::Linear { angle, .. } => {
            // PowerPoint's angle runs clockwise from left→right; CSS runs
            // clockwise from bottom→top.
            let css_angle = (90.0 - angle).rem_euclid(360.0);
            format!("linear-gradient({css_angle:.1}deg, {})", stops.join(", "))
        }
        dm::style::GradientShape::Path { path, .. } => {
            if path == "rect" {
                format!("linear-gradient(180deg, {})", stops.join(", "))
            } else {
                format!("radial-gradient(circle at center, {})", stops.join(", "))
            }
        }
    }
}

/// A resolved outline as a CSS border, for the CSS-painted shapes.
fn border_css(
    ctx: &dresolve::Ctx,
    line: Option<&dm::Line>,
    ph: Option<Rgba>,
    shape: &dm::Shape,
) -> Option<Border> {
    let line = line?;
    if shape.is_line() {
        return None; // a connector's stroke is its SVG
    }
    let fill = line.fill.as_ref()?;
    let color = match fill {
        dm::Fill::None => return None,
        dm::Fill::Solid { color } => css(&ctx.color(color, ph)),
        _ => return None,
    };
    let width = line.width.unwrap_or(0.75).max(0.25);
    //  the original matched per-variant Dash enums (a newer model build);
    // the pinned model's Dash is Preset { name } | Custom — the same mapping
    // by preset name.
    let style = match line.dash.as_ref() {
        None => "solid",
        Some(dmstyle::Dash::Preset { name }) => match name.as_str() {
            "solid" => "solid",
            "dot" | "sysDot" | "sysDashDot" | "sysDashDotDot" => "dotted",
            "dash" | "lgDash" | "sysDash" | "dashDot" | "lgDashDot" | "lgDashDotDot" => "dashed",
            _ => "dashed",
        },
        Some(dmstyle::Dash::Custom { .. }) => "dashed",
    };
    Some(Border { width, color, style: style.to_string() })
}

/// A shape's outer shadow as CSS.
fn shadow_css(ctx: &dresolve::Ctx, shape: &dm::Shape) -> Option<String> {
    //  the pinned model's Effects splits outer/inner; the original read a
    // single `shadow` — the outer shadow is the one a slide's CSS shadow draws.
    let shadow = shape.effects.as_ref()?.outer_shadow.as_ref()?;
    let color = ctx.color(&shadow.color, None);
    if color.a == 0 {
        return None;
    }
    let rad = shadow.dir.to_radians();
    let dx = shadow.dist * rad.cos();
    let dy = shadow.dist * rad.sin();
    Some(format!(
        "{dx:.1}pt {dy:.1}pt {:.1}pt {}",
        shadow.blur.max(0.0),
        css(&color)
    ))
}

fn css(c: &Rgba) -> String {
    if c.a == 255 {
        format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
    } else {
        format!("rgba({},{},{},{:.3})", c.r, c.g, c.b, c.a as f64 / 255.0)
    }
}

// ---------------------------------------------------------------------------
// The speakable text and its spans
// ---------------------------------------------------------------------------

/// Build `Slide::text` from the items, in the order they were added
/// (z-order — PowerPoint's own reading order), recording a byte span per
/// paragraph. Runs on after the last rebuild of the geometry.
/// The deck's cover, as JPEG bytes for the library shelf's `cover.jpg`:
/// the first image the deck itself ships (the first slide's first picture
/// wins, then any other painted picture, then any media at all), converted
/// to JPEG when it is not one already. `None` — the shelf's placeholder —
/// for a text-only deck.
//  authored from the call sites (sn-store books.rs / sn-ui settings.rs
// `cover_jpeg(&deck) -> Option<impl AsRef<[u8]>>` + the "a picture the deck
// itself ships, no deck rasteriser exists" comment); no original of this
// function surfaced. The media walk and the image-crate conversion are the
// minimal reading of those contracts.
pub fn cover_jpeg(deck: &SlideDeck) -> Option<Vec<u8>> {
    // Painted pictures first, in slide order; then everything else in the
    // media list, so a deck whose images live only in layouts still covers.
    let mut wanted: Vec<usize> = Vec::new();
    for slide in &deck.slides {
        for item in &slide.items {
            if let Content::Picture { media, .. } = &item.content {
                wanted.push(*media);
            }
        }
    }
    wanted.extend(0..deck.media.len());
    for idx in wanted {
        let Some(m) = deck.media.get(idx) else {
            continue;
        };
        // SVG has no raster to extract and the viewer draws it itself; skip.
        if m.content_type == "image/svg+xml" || !m.content_type.starts_with("image/") {
            continue;
        }
        if m.content_type == "image/jpeg" {
            return Some(m.data.to_vec());
        }
        if let Ok(img) = image::load_from_memory(&m.data) {
            let mut out = std::io::Cursor::new(Vec::new());
            if img
                .to_rgb8()
                .write_to(&mut out, image::ImageFormat::Jpeg)
                .is_ok()
            {
                return Some(out.into_inner());
            }
        }
    }
    None
}

pub fn assemble_text(slide: &mut Slide) {
    let mut text = String::new();
    let mut spans = Vec::new();
    for (index, item) in slide.items.iter().enumerate() {
        let item_start = text.len();
        match &item.content {
            Content::Text { paras, .. } => {
                for (para, p) in paras.iter().enumerate() {
                    let mut joined = String::new();
                    for run in &p.runs {
                        joined.push_str(&run.text.replace('\n', " "));
                    }
                    let trimmed = joined.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    let start = text.len();
                    text.push_str(trimmed);
                    spans.push(TextSpan { start, end: text.len(), item: index, para: Some(para) });
                }
            }
            Content::Picture { ocr: Some(ocr), .. } => {
                for line in ocr.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    let start = text.len();
                    text.push_str(line);
                    spans.push(TextSpan { start, end: text.len(), item: index, para: None });
                }
            }
            Content::Picture { ocr: None, .. } => {} // a picture's own pixels are not spoken
            Content::Table { rows, .. } => {
                for row in rows {
                    let cells: Vec<&str> = row
                        .cells
                        .iter()
                        .filter(|c| !c.covered)
                        .map(|c| c.text.trim())
                        .filter(|t| !t.is_empty())
                        .collect();
                    if cells.is_empty() {
                        continue;
                    }
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    let start = text.len();
                    text.push_str(&cells.join(", "));
                    spans.push(TextSpan { start, end: text.len(), item: index, para: None });
                }
            }
            Content::Chart { .. } => {} // chart data is not spoken
            Content::Frame { .. } => {}
        }
        let _ = item_start;
    }
    if !slide.notes.trim().is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        let start = text.len();
        text.push_str(slide.notes.trim());
        spans.push(TextSpan { start, end: text.len(), item: usize::MAX, para: None });
    }
    slide.text = text;
    slide.spans = spans;
}
