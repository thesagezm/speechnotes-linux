//! Markdown parsing: a typed block tree plus inline runs.
//!
//! One parse, three consumers: the rendered preview, the speech text and the
//! read-along highlight all walk the same [`Document`], so what you see and
//! what the voice says cannot drift.
//!
//! The parse itself is [comrak](https://crates.io/crates/comrak) — the
//! CommonMark + GFM engine rustdoc uses, i.e. literally the GitHub flavour —
//! with the extensions a note-taker actually reaches for enabled: tables,
//! strikethrough, task lists, autolinks, footnotes, superscript, description
//! lists, alerts and multiline block quotes. It replaced a hand-rolled subset
//! parser that misrendered real notes (no autolinks, no footnotes, no setext
//! headings, table alignment parsed then discarded) — and, because it is
//! fuzzed upstream, it retires an entire class of hand-parser bugs this
//! project had already paid for once (a parser that does not advance on some
//! construct hangs the app).
//!
//! Math *is* enabled, with care: `$x$` and `$$x$$` (plus the `\(...\)` and
//! `\[...\]` spellings) parse as formulas, and the formula is shown in a
//! readable Unicode form — `x² + √(y)` — rather than as LaTeX source.
//!
//! Ordinary dollar amounts must survive that. comrak's own rules (pandoc's:
//! no space just inside the delimiters, a closing `$` not followed by a
//! digit) already keep `$5 and $10` as text; on top of that the parse checks
//! the span's content and leaves it alone when it reads as money or as a
//! price — a note app must not reinterpret its user's currency.

use std::collections::HashMap;

use crate::math;

use comrak::nodes::{AstNode, ListDelimType, ListType, NodeTaskItem, NodeValue, TableAlignment};
use comrak::{parse_document, Arena, Options};

/// Block nesting deeper than this is flattened to its text. Deeper than any
/// note anyone writes, and it keeps a hostile document from driving the
/// walker's recursion off the end of the stack.
const MAX_DEPTH: usize = 64;

/// Alignment of a table column, from the delimiter row (`:---`, `:---:`,
/// `---:`). `None` means the source did not align it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    None,
    Left,
    Center,
    Right,
}

/// The kind of a GitHub alert (`> [!NOTE]` and friends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    /// The title GitHub renders for the kind.
    pub fn title(&self) -> &'static str {
        match self {
            AlertKind::Note => "Note",
            AlertKind::Tip => "Tip",
            AlertKind::Important => "Important",
            AlertKind::Warning => "Warning",
            AlertKind::Caution => "Caution",
        }
    }
}

/// One inline span.
///
/// Styling spans carry their content as a nested run list, so `**a *b* c**`
/// keeps its shape instead of flattening to plain text the way the previous
/// parser did.
#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    /// Plain text.
    Text(String),
    /// `code`
    Code(String),
    /// **bold** — may contain nested runs.
    Bold(Vec<Inline>),
    /// *italic* — may contain nested runs.
    Italic(Vec<Inline>),
    /// ~~strike~~ — may contain nested runs.
    Strike(Vec<Inline>),
    /// ^superscript^, and footnote markers land here too.
    Superscript(Vec<Inline>),
    /// [label](target)
    Link {
        label: Vec<Inline>,
        target: String,
    },
    /// ![alt](src)
    Image {
        alt: String,
        src: String,
    },
    /// A soft break inside a paragraph. Renderers collapse it; the speech
    /// cleaner turns it into a space.
    SoftBreak,
    /// A hard break (`\` or two spaces at end of line).
    HardBreak,
    /// A formula: `$x^2$` inline, `$$...$$` displayed on its own line.
    ///
    /// The source is kept as written, so nothing is lost, and the renderer
    /// shows a readable Unicode form of it (see [`math::to_unicode`]) —
    /// `x^2 + √(y)` rather than `x^2 + \sqrt{y}`.
    Math {
        /// The LaTeX source, verbatim.
        source: String,
        /// True for `$$...$$`: a display formula on its own line.
        display: bool,
    },
}

impl Inline {
    /// The words a voice should read.
    pub fn speech(&self) -> String {
        match self {
            Inline::Text(t) | Inline::Code(t) => t.clone(),
            Inline::Bold(inner) | Inline::Italic(inner) | Inline::Strike(inner) => {
                join_speech(inner)
            }
            // A superscript is a typographic marker — a footnote number, an
            // exponent — and a voice reading "see chapter 4 [2]" out loud is
            // noise. The definition itself is still read where it stands.
            Inline::Superscript(_) => String::new(),
            Inline::Link { label, .. } => join_speech(label),
            Inline::Image { alt, .. } => {
                if alt.trim().is_empty() {
                    String::new()
                } else {
                    format!("image: {alt}")
                }
            }
            Inline::SoftBreak | Inline::HardBreak => " ".to_string(),
            // A formula is read as its readable form: "x squared plus the
            // square root of y" beats "x caret two plus backslash sqrt".
            Inline::Math { source, .. } => math::to_speech(source),
        }
    }

    /// Plain text, for search and titles.
    pub fn plain(&self) -> String {
        match self {
            Inline::Text(t) | Inline::Code(t) => t.clone(),
            Inline::Bold(inner)
            | Inline::Italic(inner)
            | Inline::Strike(inner)
            | Inline::Superscript(inner) => join_plain(inner),
            Inline::Link { label, .. } => join_plain(label),
            Inline::Image { alt, .. } => alt.clone(),
            Inline::SoftBreak | Inline::HardBreak => String::new(),
            Inline::Math { source, .. } => source.clone(),
        }
    }
}

/// One top-level block.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
    },
    Paragraph {
        inlines: Vec<Inline>,
    },
    /// A display formula (`$$…$$`) standing on its own line.
    ///
    /// A display formula is not a paragraph with a big run in it: it is
    /// centred, on its own, with space above and below. The Markdown parser
    /// hands it over inside a paragraph (a paragraph is all comrak gives a
    /// `$$…$$` block), so the paragraph is **split** around it — the text
    /// before it, the formula, the text after it, each its own block.
    DisplayMath {
        /// The formula's source, in LaTeX.
        source: String,
    },
    /// Bulleted (`-`, `*`, `+`) or numbered (`1.`), with nesting depth.
    ListItem {
        depth: usize,
        ordered: bool,
        marker: String,
        inlines: Vec<Inline>,
    },
    TaskItem {
        depth: usize,
        done: bool,
        inlines: Vec<Inline>,
    },
    Quote {
        /// Nested block content, so a quote containing a list stays structured.
        blocks: Vec<Block>,
    },
    /// A GitHub alert (`> [!NOTE]`), with its own kind for styling.
    Alert {
        kind: AlertKind,
        /// The author's title override, when the source gave one.
        title: Option<String>,
        blocks: Vec<Block>,
    },
    /// `[^label]` definition, rendered small and dim at its position.
    FootnoteDefinition {
        label: String,
        blocks: Vec<Block>,
    },
    /// Description-list entry (`Term` / `: details`).
    Definition {
        term: Vec<Inline>,
        blocks: Vec<Block>,
    },
    Code {
        language: Option<String>,
        text: String,
    },
    /// `---`, `***`, `___`
    Rule,
    Table {
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
        /// One entry per column, from the delimiter row.
        alignments: Vec<Align>,
    },
    /// Anything the parser did not recognise, kept verbatim. Never dropped.
    Raw(String),
}

impl Block {
    /// Every word in the block, for speech and for the plain-text view.
    pub fn speech(&self) -> String {
        match self {
            Block::Heading { inlines, .. } | Block::Paragraph { inlines } => {
                join_speech(inlines)
            }
            Block::ListItem { inlines, marker, .. } => {
                // The marker is a bullet or a number; a voice should not read
                // "-" out loud, but iOS reads ordered-list numbers, so this
                // keeps them and drops only the punctuation-only bullets.
                if marker.chars().all(|c| c == '-' || c == '*' || c == '+') {
                    join_speech(inlines)
                } else {
                    format!("{marker} {}", join_speech(inlines))
                }
            }
            Block::TaskItem { done, inlines, .. } => {
                let state = if *done { "done" } else { "not done" };
                format!("{state}, {}", join_speech(inlines))
            }
            Block::Quote { blocks }
            | Block::Alert { blocks, .. }
            | Block::FootnoteDefinition { blocks, .. } => blocks
                .iter()
                .map(|b| b.speech())
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            Block::Definition { term, blocks } => {
                // The term is content — a voice reading a glossary should say
                // it, then what it means.
                let term = join_speech(term);
                let body = blocks
                    .iter()
                    .map(|b| b.speech())
                    .filter(|s| !s.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                match (term.is_empty(), body.is_empty()) {
                    (false, false) => format!("{term}: {body}"),
                    (false, true) => term,
                    _ => body,
                }
            }
            Block::Code { text, .. } => text.clone(),
            Block::Rule => String::new(),
            Block::Table { header, rows, .. } => {
                // A row is read cell by cell, with a pause between cells so
                // "Name Alpha" never runs together.
                let cell_line = |cells: &[Vec<Inline>]| -> String {
                    cells
                        .iter()
                        .map(|cell| join_speech(cell))
                        .filter(|c| !c.is_empty())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let mut out: Vec<String> = Vec::new();
                if !header.is_empty() {
                    out.push(cell_line(header));
                }
                for row in rows {
                    let line = cell_line(row);
                    if !line.is_empty() {
                        out.push(line);
                    }
                }
                out.join(". ")
            }
            //  arm lost mid-edit in the original; behavior pinned by this
            // file's own test a_display_formula_keeps_its_own_line ("over")
            // and the Inline::Math arm above.
            Block::DisplayMath { source } => math::to_speech(source),
            Block::Raw(t) => t.clone(),
        }
    }

    /// A stable identifier for the block, used to anchor read-along positions.
    pub fn kind(&self) -> &'static str {
        match self {
            Block::Heading { .. } => "heading",
            Block::Paragraph { .. } => "paragraph",
            Block::ListItem { .. } => "list",
            Block::TaskItem { .. } => "task",
            Block::Quote { .. } => "quote",
            Block::Alert { .. } => "alert",
            Block::FootnoteDefinition { .. } => "footnote",
            Block::Definition { .. } => "definition",
            Block::Code { .. } => "code",
            Block::Rule => "rule",
            Block::Table { .. } => "table",
            // One lowercase word per variant, matching the eleven other arms.
            Block::DisplayMath { .. } => "math",
            Block::Raw(_) => "raw",
        }
    }
}

fn join_speech(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for i in inlines {
        let piece = i.speech();
        if piece.is_empty() {
            continue;
        }
        if !out.is_empty() && !out.ends_with(' ') && !piece.starts_with(' ') {
            out.push(' ');
        }
        out.push_str(&piece);
    }
    out.trim().to_string()
}

fn join_plain(inlines: &[Inline]) -> String {
    inlines.iter().map(|i| i.plain()).collect()
}

/// The whole document.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Document {
    pub blocks: Vec<Block>,
}

impl Document {
    /// The text a voice should read, in order.
    pub fn speech(&self) -> String {
        self.blocks
            .iter()
            .map(|b| b.speech())
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Plain text: speech with the paragraph structure flattened to newlines.
    pub fn plain(&self) -> String {
        self.blocks
            .iter()
            .map(|b| b.speech())
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

/// Parse a whole document.
pub fn parse(source: &str) -> Document {
    let arena = Arena::new();
    let root = parse_document(&arena, source, &options());
    let walker = Walker {
        foot: index_footnotes(root),
    };
    Document {
        blocks: walker.blocks_of(root, 0),
    }
}

/// The comrak option set: GFM plus the extensions notes actually use.
fn options() -> Options<'static> {
    let mut options = Options::default();
    let ext = &mut options.extension;
    ext.table = true;
    ext.strikethrough = true;
    ext.tasklist = true;
    ext.autolink = true;
    ext.footnotes = true;
    ext.superscript = true;
    ext.description_lists = true;
    ext.alerts = true;
    ext.multiline_block_quotes = true;
    // Formulas: `$x$` inline and `$$x$$` displayed, in both the dollar and
    // the LaTeX-delimiter spellings. Without this the source stays raw
    // text and a note's equations read as noise.
    ext.math_dollars = true;
    ext.math_latex = true;
    options
}

// ---------------------------------------------------------------------------
// The comrak → Document walk
// ---------------------------------------------------------------------------

struct Walker {
    /// Footnote name → its 1-based position in the document. GitHub numbers
    /// footnotes by definition order; refs and defs must agree with each
    /// other, so both look the number up here instead of trusting two
    /// different comrak indices.
    foot: HashMap<String, usize>,
}

/// Walk the tree once and number every footnote definition in document order.
fn index_footnotes<'a>(node: &'a AstNode<'a>) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    let mut counter = 0usize;
    walk_footnotes(node, &mut counter, &mut map);
    map
}

fn walk_footnotes<'a>(node: &'a AstNode<'a>, counter: &mut usize, map: &mut HashMap<String, usize>) {
    if let NodeValue::FootnoteDefinition(fd) = &node.data().value {
        if !fd.name.is_empty() {
            *counter += 1;
            map.entry(fd.name.clone()).or_insert(*counter);
        }
    }
    for child in node.children() {
        walk_footnotes(child, counter, map);
    }
}

impl Walker {
    fn footnote_label(&self, name: &str, fallback: u32) -> String {
        match self.foot.get(name) {
            Some(n) => n.to_string(),
            // A reference with no definition still shows something numbered
            // rather than the raw name, matching what GitHub prints.
            None => fallback.max(1).to_string(),
        }
    }

    /// Convert every child of `parent` into blocks appended to `out`.
    fn blocks_of<'a>(&self, parent: &'a AstNode<'a>, depth: usize) -> Vec<Block> {
        let mut out = Vec::new();
        if depth > MAX_DEPTH {
            // Absurd nesting: flatten to text so nothing a user wrote vanishes.
            let text = subtree_plain(parent);
            if !text.is_empty() {
                out.push(Block::Raw(text));
            }
            return out;
        }
        for child in parent.children() {
            self.one_block(child, depth, &mut out);
        }
        out
    }

    fn one_block<'a>(&self, node: &'a AstNode<'a>, depth: usize, out: &mut Vec<Block>) {
        match &node.data().value {
            NodeValue::Paragraph => {
                // A paragraph holding a display formula is split around it:
                // the formula gets its own block, and the text on either
                // side stays a paragraph of its own. A paragraph that is
                // *only* the formula becomes just the formula.
                let inlines = self.inlines_of(node);
                split_display_math(inlines, out);
            }
            NodeValue::Heading(h) => out.push(Block::Heading {
                level: h.level,
                inlines: self.inlines_of(node),
            }),
            NodeValue::ThematicBreak => out.push(Block::Rule),
            NodeValue::CodeBlock(cb) => out.push(Block::Code {
                // The info string is `rust ignore`-shaped; the language is its
                // first word, exactly what the old parser kept.
                language: if cb.fenced {
                    cb.info.trim().split_whitespace().next().map(str::to_string)
                } else {
                    None
                },
                text: cb.literal.trim_end().to_string(),
            }),
            NodeValue::HtmlBlock(hb) => out.push(Block::Raw(hb.literal.trim_end().to_string())),
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => out.push(Block::Quote {
                blocks: self.blocks_of(node, depth + 1),
            }),
            NodeValue::Alert(a) => out.push(Block::Alert {
                kind: match a.alert_type {
                    comrak::nodes::AlertType::Note => AlertKind::Note,
                    comrak::nodes::AlertType::Tip => AlertKind::Tip,
                    comrak::nodes::AlertType::Important => AlertKind::Important,
                    comrak::nodes::AlertType::Warning => AlertKind::Warning,
                    comrak::nodes::AlertType::Caution => AlertKind::Caution,
                },
                title: a.title.clone(),
                blocks: self.blocks_of(node, depth + 1),
            }),
            NodeValue::List(_) => self.list_blocks(node, depth, out),
            NodeValue::Table(t) => {
                let alignments: Vec<Align> = t
                    .alignments
                    .iter()
                    .map(|a| match a {
                        TableAlignment::None => Align::None,
                        TableAlignment::Left => Align::Left,
                        TableAlignment::Center => Align::Center,
                        TableAlignment::Right => Align::Right,
                    })
                    .collect();
                let mut header: Vec<Vec<Inline>> = Vec::new();
                let mut rows: Vec<Vec<Vec<Inline>>> = Vec::new();
                for row in node.children() {
                    let is_header = matches!(row.data().value, NodeValue::TableRow(true));
                    let cells: Vec<Vec<Inline>> = row.children().map(|c| self.inlines_of(c)).collect();
                    if is_header {
                        header = cells;
                    } else {
                        rows.push(cells);
                    }
                }
                out.push(Block::Table {
                    header,
                    rows,
                    alignments,
                });
            }
            NodeValue::FootnoteDefinition(fd) => out.push(Block::FootnoteDefinition {
                label: self.footnote_label(&fd.name, 0),
                blocks: self.blocks_of(node, depth + 1),
            }),
            NodeValue::DescriptionList => self.description_list(node, depth, out),
            // Front matter is disabled in `options`, so this only fires for
            // pasted YAML-ish blobs; keep them visible rather than dropped.
            NodeValue::FrontMatter(s) => out.push(Block::Raw(s.trim_end().to_string())),
            // Inline-only nodes cannot appear at block level in a comrak tree
            // (tight list content is handled inside `list_blocks`), and any
            // future variant this app has not modelled yet is skipped rather
            // than guessed at.
            _ => {}
        }
    }

    /// A `List` node becomes one [`Block::ListItem`] or [`Block::TaskItem`]
    /// per item. The model is flat — nesting is a `depth` — so items nested
    /// inside items come out as later blocks, which is exactly how the
    /// renderer draws them: indented rows under their parent.
    fn list_blocks<'a>(&self, list: &'a AstNode<'a>, depth: usize, out: &mut Vec<Block>) {
        let meta = match &list.data().value {
            NodeValue::List(nl) => *nl,
            _ => return,
        };
        for (index, item) in list.children().enumerate() {
            match &item.data().value {
                NodeValue::Item(_) => self.list_item(item, &meta, index, None, depth, out),
                NodeValue::TaskItem(t) => self.list_item(item, &meta, index, Some(t), depth, out),
                _ => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn list_item<'a>(
        &self,
        item: &'a AstNode<'a>,
        meta: &comrak::nodes::NodeList,
        index: usize,
        task: Option<&NodeTaskItem>,
        depth: usize,
        out: &mut Vec<Block>,
    ) {
        let ordered = meta.list_type == ListType::Ordered;
        let marker = if ordered {
            let delim = match meta.delimiter {
                ListDelimType::Period => '.',
                ListDelimType::Paren => ')',
            };
            // `start` is the ordinal the list begins at, so `3) …` keeps its
            // numbering instead of silently restarting at one.
            format!("{}{}", meta.start + index, delim)
        } else {
            (meta.bullet_char as char).to_string()
        };

        // A tight list hangs its content directly under the item as inlines;
        // a loose one wraps it in Paragraphs; both arrive here as the item's
        // text. Blocks inside an item (nested lists, code, quotes) are emitted
        // after the item itself, indented by depth.
        let mut inlines: Vec<Inline> = Vec::new();
        let mut extra: Vec<Block> = Vec::new();
        for child in item.children() {
            match &child.data().value {
                NodeValue::Paragraph => {
                    if !inlines.is_empty() {
                        inlines.push(Inline::SoftBreak);
                    }
                    inlines.extend(self.inlines_of(child));
                }
                _ => match self.inline_of(child) {
                    Some(inline) => inlines.push(inline),
                    None => self.one_block(child, depth + 1, &mut extra),
                },
            }
        }

        match task {
            Some(t) => out.push(Block::TaskItem {
                depth,
                done: t.symbol.is_some(),
                inlines,
            }),
            None => out.push(Block::ListItem {
                depth,
                ordered,
                marker,
                inlines,
            }),
        }
        out.extend(extra);
    }

    fn description_list<'a>(&self, node: &'a AstNode<'a>, depth: usize, out: &mut Vec<Block>) {
        for item in node.children() {
            let mut term: Vec<Inline> = Vec::new();
            let mut blocks: Vec<Block> = Vec::new();
            for part in item.children() {
                match &part.data().value {
                    // The term arrives as a moved-in Paragraph node, not as
                    // bare inlines — one level deeper than it looks.
                    NodeValue::DescriptionTerm => {
                        let mut collected: Vec<Inline> = Vec::new();
                        for inner in part.children() {
                            if matches!(inner.data().value, NodeValue::Paragraph) {
                                collected.extend(self.inlines_of(inner));
                            }
                        }
                        term = collected;
                    }
                    NodeValue::DescriptionDetails => blocks = self.blocks_of(part, depth + 1),
                    _ => {}
                }
            }
            out.push(Block::Definition { term, blocks });
        }
    }

    /// Convert `node`'s children to inline runs. Nodes that are not inlines
    /// return nothing here; block contexts handle them.
    fn inlines_of<'a>(&self, node: &'a AstNode<'a>) -> Vec<Inline> {
        node.children().filter_map(|c| self.inline_of(c)).collect()
    }

    fn inline_of<'a>(&self, node: &'a AstNode<'a>) -> Option<Inline> {
        match &node.data().value {
            NodeValue::Text(t) => Some(Inline::Text(t.to_string())),
            NodeValue::SoftBreak => Some(Inline::SoftBreak),
            NodeValue::LineBreak => Some(Inline::HardBreak),
            NodeValue::Code(nc) => Some(Inline::Code(nc.literal.clone())),
            // Raw HTML inline shows as literal text — a note cannot inject
            // markup, but nothing it says is hidden either.
            NodeValue::HtmlInline(s) => Some(Inline::Text(s.clone())),
            NodeValue::Emph => Some(Inline::Italic(self.inlines_of(node))),
            NodeValue::Strong => Some(Inline::Bold(self.inlines_of(node))),
            NodeValue::Strikethrough => Some(Inline::Strike(self.inlines_of(node))),
            NodeValue::Superscript => Some(Inline::Superscript(self.inlines_of(node))),
            NodeValue::Link(nl) => Some(Inline::Link {
                label: self.inlines_of(node),
                target: nl.url.clone(),
            }),
            NodeValue::Image(nl) => {
                let alt = join_plain(&self.inlines_of(node));
                Some(Inline::Image {
                    alt,
                    src: nl.url.clone(),
                })
            }
            NodeValue::FootnoteReference(nfr) => {
                let label = self.footnote_label(&nfr.name, nfr.ix);
                Some(Inline::Superscript(vec![Inline::Text(format!("[{label}]"))]))
            }
            // Everything else inline-capable (wikilinks, sub/superscript
            // variants this app keeps off) falls back to its text or to
            // nothing — never to a parse error.
            NodeValue::Math(m) => {
                // A span that is money stays money: `$5`, `$1,200.50`,
                // `€20`. The formula path would turn a price into an
                // italic formula, and a note app must not reinterpret
                // its user's currency.
                if math::reads_as_money(&m.literal) {
                    return Some(Inline::Text(m.literal.clone()));
                }
                Some(Inline::Math {
                    source: m.literal.clone(),
                    display: m.display_math,
                })
            }
            _ => None,
        }
    }
}

/// All text under `node`, for the depth-cap's flatten-to-text path.
/// Split a paragraph's runs around its display formulas.
///
/// A `$$…$$` formula is a block in the author's mind — centred, on its own
/// line — but the parser hands it over as a run inside a paragraph. This
/// breaks the paragraph at each display formula: what came before stays a
/// paragraph, the formula becomes its own block, and what came after is a
/// paragraph again.
///
/// A paragraph that is nothing but the formula produces one block. A
/// paragraph with no display formula is passed through unchanged.
fn split_display_math(inlines: Vec<Inline>, out: &mut Vec<Block>) {
    if !inlines
        .iter()
        .any(|i| matches!(i, Inline::Math { display: true, .. }))
    {
        out.push(Block::Paragraph { inlines });
        return;
    }
    let mut run: Vec<Inline> = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Math { source, display: true } => {
                // The text before the formula, if there is any.
                if !run.is_empty() {
                    out.push(Block::Paragraph {
                        inlines: std::mem::take(&mut run),
                    });
                }
                out.push(Block::DisplayMath { source });
            }
            other => run.push(other),
        }
    }
    // The text after the last formula, if there is any.
    if !run.is_empty() {
        out.push(Block::Paragraph { inlines: run });
    }
}

fn subtree_plain<'a>(node: &'a AstNode<'a>) -> String {
    let mut out = String::new();
    match &node.data().value {
        NodeValue::Text(t) => out.push_str(t),
        NodeValue::Code(nc) => out.push_str(&nc.literal),
        _ => {}
    }
    for child in node.children() {
        out.push_str(&subtree_plain(child));
    }
    out.push(' ');
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_block(src: &str) -> Block {
        let doc = parse(src);
        assert_eq!(doc.blocks.len(), 1, "expected one block, got {:?}", doc.blocks);
        doc.blocks.into_iter().next().unwrap()
    }

    #[test]
    fn headings_carry_their_level() {
        for (src, level) in [("# One", 1u8), ("## Two", 2), ("###### Six", 6)] {
            match one_block(src) {
                Block::Heading { level: l, inlines } => {
                    assert_eq!(l, level);
                    assert_eq!(inlines[0].plain(), src[level as usize + 1..].trim());
                }
                other => panic!("{src:?} parsed as {other:?}"),
            }
        }
    }

    #[test]
    fn a_setext_heading_is_a_heading() {
        // The old parser rendered this as a paragraph with a line of `=` in it.
        match one_block("Title\n=====") {
            Block::Heading { level, inlines } => {
                assert_eq!(level, 1);
                assert_eq!(inlines[0].plain(), "Title");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn seven_hashes_is_not_a_heading() {
        let b = one_block("####### not a heading");
        assert!(matches!(b, Block::Paragraph { .. }));
    }

    #[test]
    fn closing_hashes_are_stripped() {
        match one_block("## Title ##") {
            Block::Heading { inlines, .. } => assert_eq!(inlines[0].plain(), "Title"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn inline_styles_are_recognised() {
        match one_block("Some **bold** and *italic* and ~~gone~~ and `code`.") {
            Block::Paragraph { inlines } => {
                let kinds: Vec<&str> = inlines
                    .iter()
                    .map(|i| match i {
                        Inline::Text(_) => "text",
                        Inline::Bold(_) => "bold",
                        Inline::Italic(_) => "italic",
                        Inline::Strike(_) => "strike",
                        Inline::Code(_) => "code",
                        _ => "other",
                    })
                    .collect();
                assert_eq!(
                    kinds,
                    vec![
                        "text", "bold", "text", "italic", "text", "strike", "text", "code", "text"
                    ],
                    "the trailing period after the code span is its own run"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn nested_emphasis_keeps_its_shape() {
        // The old parser flattened nested runs to plain text inside the bold.
        match one_block("**bold *nested* end**") {
            Block::Paragraph { inlines } => match &inlines[0] {
                Inline::Bold(inner) => {
                    assert_eq!(inner.len(), 3, "{inner:?}");
                    assert!(matches!(&inner[1], Inline::Italic(_)), "{inner:?}");
                }
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn links_and_images_are_separate() {
        let doc = parse("See [the docs](https://x.example) and ![alt](img.png).");
        let inlines = match &doc.blocks[0] {
            Block::Paragraph { inlines } => inlines,
            other => panic!("{other:?}"),
        };
        let link = inlines.iter().find_map(|i| match i {
            Inline::Link { label, target } => Some((join_plain(label), target.clone())),
            _ => None,
        });
        assert_eq!(link, Some(("the docs".into(), "https://x.example".into())));

        let image = inlines.iter().find_map(|i| match i {
            Inline::Image { alt, src } => Some((alt.clone(), src.clone())),
            _ => None,
        });
        assert_eq!(image, Some(("alt".into(), "img.png".into())));
    }

    #[test]
    fn bare_urls_autolink() {
        // Off before: a pasted URL stayed dead text.
        let doc = parse("read https://example.com/page and www.example.org now");
        match &doc.blocks[0] {
            Block::Paragraph { inlines } => {
                let targets: Vec<&str> = inlines
                    .iter()
                    .filter_map(|i| match i {
                        Inline::Link { target, .. } => Some(target.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(targets, vec!["https://example.com/page", "http://www.example.org"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn footnotes_reference_their_definitions() {
        let doc = parse(
            "A claim[^1] and another[^note].\n\n[^1]: First source.\n[^note]: Second source.\n",
        );
        // Reference markers are superscripts numbered by definition order.
        let first = match &doc.blocks[0] {
            Block::Paragraph { inlines } => inlines
                .iter()
                .find_map(|i| match i {
                    Inline::Superscript(inner) => Some(join_plain(inner)),
                    _ => None,
                })
                .expect("a footnote reference"),
            other => panic!("{other:?}"),
        };
        assert_eq!(first, "[1]", "references number by definition order: {first}");

        let labels: Vec<&str> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::FootnoteDefinition { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec!["1", "2"], "both definitions carry their number");

        // And the definition's text is still in the document.
        assert!(doc.plain().contains("First source."));
    }

    #[test]
    fn superscript_works_outside_footnotes() {
        match one_block("e = mc^2^.") {
            Block::Paragraph { inlines } => {
                assert!(inlines.iter().any(|i| matches!(i, Inline::Superscript(_))));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn gfm_alerts_are_typed() {
        match one_block("> [!WARNING]\n> Mind the dose.") {
            Block::Alert { kind, title, blocks } => {
                assert_eq!(kind, AlertKind::Warning);
                assert_eq!(title.as_deref(), None);
                assert!(blocks[0].speech().contains("Mind the dose"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_table_records_its_column_alignment() {
        let doc = parse("| A | B | C |\n|:--|:-:|--:|\n| 1 | 2 | 3 |");
        match &doc.blocks[0] {
            Block::Table { alignments, .. } => {
                assert_eq!(
                    alignments,
                    &vec![Align::Left, Align::Center, Align::Right],
                    "the delimiter row's colons decide alignment"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn lists_of_every_kind() {
        let doc = parse("- one\n- two\n1. first\n1) second\n- [ ] todo\n- [x] done");
        let kinds: Vec<&str> = doc.blocks.iter().map(|b| b.kind()).collect();
        assert_eq!(kinds, vec!["list", "list", "list", "list", "task", "task"]);

        let tasks: Vec<(bool, String)> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::TaskItem { done, inlines, .. } => {
                    Some((*done, inlines[0].plain()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(tasks, vec![(false, "todo".into()), (true, "done".into())]);
    }

    #[test]
    fn an_uppercase_x_checks_a_task() {
        let doc = parse("- [X] done");
        assert!(matches!(doc.blocks[0], Block::TaskItem { done: true, .. }));
    }

    #[test]
    fn ordered_lists_keep_their_start_number() {
        let doc = parse("3) third\n4) fourth");
        let markers: Vec<String> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::ListItem { marker, .. } => Some(marker.clone()),
                _ => None,
            })
            .collect();
        // The source's delimiter is kept, not normalised: the user wrote `)`.
        assert_eq!(markers, vec!["3)", "4)"]);
    }

    #[test]
    fn nested_list_items_carry_their_depth() {
        let doc = parse("- top\n  - nested\n    - deeper");
        let depths: Vec<usize> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::ListItem { depth, .. } => Some(*depth),
                _ => None,
            })
            .collect();
        assert_eq!(depths, vec![0, 1, 2]);
    }

    #[test]
    fn quotes_nest_their_blocks() {
        let doc = parse("> quoted line\n> - a list item");
        match &doc.blocks[0] {
            Block::Quote { blocks } => {
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[1].kind(), "list");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_description_list_yields_definitions() {
        let doc = parse("Term 1\n\n: Details 1\n");
        match &doc.blocks[0] {
            Block::Definition { term, blocks } => {
                assert_eq!(join_plain(term), "Term 1");
                assert!(blocks[0].speech().contains("Details 1"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn fenced_code_is_not_parsed_as_markdown() {
        let doc = parse("```rust\n# not a heading\n- not a list\n```");
        match &doc.blocks[0] {
            Block::Code { language, text } => {
                assert_eq!(language.as_deref(), Some("rust"));
                assert!(text.contains("# not a heading"));
                assert!(text.contains("- not a list"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unclosed_fence_still_yields_a_code_block() {
        let doc = parse("```\nstill code\n");
        assert_eq!(doc.blocks[0].kind(), "code");
    }

    #[test]
    fn rules_of_three_kinds() {
        let doc = parse("---\n***\n___");
        assert_eq!(doc.blocks.len(), 3);
        assert!(doc.blocks.iter().all(|b| b.kind() == "rule"));
    }

    #[test]
    fn a_single_dash_is_not_a_rule() {
        let doc = parse("- a list item");
        assert_eq!(doc.blocks[0].kind(), "list");
    }

    #[test]
    fn tables_split_into_cells() {
        let doc = parse("| Name | Words |\n|---|---:|\n| Alpha | 12 |\n| Beta | 30 |");
        match &doc.blocks[0] {
            Block::Table { header, rows, .. } => {
                assert_eq!(header.len(), 2);
                assert_eq!(header[0][0].plain(), "Name");
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[1][1].iter().map(|i| i.plain()).collect::<String>(), "30");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_pipe_inside_a_paragraph_is_not_a_table() {
        let doc = parse("use a | b for pipes");
        assert_eq!(doc.blocks[0].kind(), "paragraph");
    }

    #[test]
    fn paragraphs_span_lines_with_soft_breaks() {
        let doc = parse("line one\nline two\n\nsecond paragraph");
        assert_eq!(doc.blocks.len(), 2);
        match &doc.blocks[0] {
            Block::Paragraph { inlines } => {
                assert!(inlines.contains(&Inline::SoftBreak));
                assert_eq!(Block::Paragraph { inlines: inlines.clone() }.speech(), "line one line two");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn speech_drops_bullets_but_keeps_numbers() {
        let doc = parse("- bullet point\n1. numbered point");
        let speech = doc.speech();
        assert!(speech.contains("bullet point"));
        assert!(speech.contains("1. numbered point"), "{speech}");
        assert!(!speech.starts_with('-'));
    }

    #[test]
    fn speech_reads_a_task_state() {
        let doc = parse("- [x] finished the thing");
        assert!(doc.speech().contains("done"), "{}", doc.speech());
    }

    #[test]
    fn speech_of_an_image_uses_its_alt_text() {
        let doc = parse("![a diagram](x.png)");
        assert!(doc.speech().contains("a diagram"));
        let bare = parse("![](x.png)");
        assert!(bare.speech().trim().is_empty(), "an empty alt says nothing");
    }

    #[test]
    fn speech_skips_footnote_markers_but_keeps_definitions() {
        // The reference is a marker, not a word; the definition is content.
        let doc = parse("A claim[^1].\n\n[^1]: The source.\n");
        let speech = doc.speech();
        assert!(!speech.contains(" [1]"), "{speech}");
        assert!(speech.contains("The source."), "{speech}");
    }

    #[test]
    fn nothing_is_ever_dropped() {
        // Every character the parser does not understand must survive. The
        // comparison is whitespace-blind because `plain()` derives from the
        // speech text, which re-spaces runs; the claim here is that no token
        // vanishes, not that spacing round-trips.
        let weird = "some <<weird>> & \"quotes\" and #hash and <b>html</b>";
        let doc = parse(weird);
        let plain: String = doc.plain().chars().filter(|c| !c.is_whitespace()).collect();
        for token in ["<<weird>>", "quotes", "#hash", "<b>html</b>"] {
            assert!(plain.contains(token), "{token:?} vanished from {plain:?}");
        }
    }

    #[test]
    fn plain_text_keeps_paragraph_structure() {
        let doc = parse("# Title\n\nFirst para.\n\nSecond para.");
        let plain = doc.plain();
        assert_eq!(plain, "Title\nFirst para.\nSecond para.");
    }

    #[test]
    fn an_empty_document_parses_to_nothing() {
        assert!(parse("").is_empty());
        assert!(parse("\n\n   \n").is_empty());
    }

    #[test]
    fn every_construct_the_parser_handles_terminates() {
        // A block parser that does not advance on some construct loops forever,
        // and a note-taking app that hangs on a bullet list is worse than one
        // that misrenders it. comrak owns termination now, but the gate stays:
        // if an upgrade or a wrapper reintroduces a hang, this fails first.
        let cases = [
            "- a bullet",
            "* a star bullet",
            "+ a plus bullet",
            "1. a number",
            "- [ ] a task",
            "- [x] a done task",
            "> a quote",
            "> - a bullet in a quote",
            ">>> multiline quote",
            "> [!NOTE] an alert",
            "# a heading",
            "Title\n====",
            "```\ncode\n```",
            "---",
            "Term\n\n: definition",
            "| a | b |\n| --- | --- |\n| 1 | 2 |",
            "A ref[^1]\n\n[^1]: the note",
            "e = mc^2^",
            "just a paragraph",
            "a paragraph\nacross two lines",
        ];
        for case in cases {
            let parsed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = parsed.clone();
            let source = case.to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let doc = parse(&source);
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                let _ = tx.send(doc.blocks.len());
            });
            // If the parser loops, the flag never lands and the channel closes
            // with nothing. Either way the thread is detached, so a genuine
            // hang fails this test rather than the whole suite.
            let got = rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or_else(|_| panic!("parsing {case:?} did not terminate"));
            assert!(
                parsed.load(std::sync::atomic::Ordering::SeqCst) || got > 0,
                "{case:?} produced no blocks"
            );
        }
    }

    #[test]
    fn a_dollar_formula_parses_as_math_and_not_as_money() {
        let doc = parse("The area is $x^2 + y^2$ here.");
        let text = doc.blocks[0].speech();
        assert!(text.contains("squared"), "got {text:?}");
        let plain = doc.blocks[0].speech();
        assert!(!plain.contains('$'), "the dollars should be gone: {plain:?}");
    }

    #[test]
    fn a_display_formula_keeps_its_own_line() {
        let doc = parse("Before\n\n$$\\frac{a}{b}$$\n\nAfter");
        assert_eq!(doc.blocks.len(), 3, "{:?}", doc.blocks);
        let middle = &doc.blocks[1];
        let text = middle.speech();
        assert!(text.contains("over"), "got {text:?}");
    }

    #[test]
    fn a_price_stays_a_price() {
        // The rule that matters: money must not become a formula.
        let doc = parse("It costs $1,200.50 in total.");
        let text = doc.blocks[0].speech();
        assert!(text.contains("1,200.50"), "got {text:?}");
        assert!(!text.contains("squared"), "a price became a formula: {text:?}");
    }

    #[test]
    fn a_latex_delimited_formula_parses() {
        let doc = parse(r"Energy is \(E = mc^2\) always.");
        let text = doc.blocks[0].speech();
        assert!(text.contains("squared"), "got {text:?}");
    }

    #[test]
    fn a_200k_character_document_parses_in_linear_time() {
        // The trailing blank line keeps the units from continuing each other:
        // without it, every paragraph after the first is a lazy continuation
        // of the previous unit's last list item — correct CommonMark, but not
        // three separate blocks per unit.
        let src = "This is a paragraph with **bold** text and a [link](https://x.example/y).\n\n- item one\n- item two\n\n"
            .repeat(2_000);
        assert!(src.len() > 100_000);
        let started = std::time::Instant::now();
        let doc = parse(&src);
        let elapsed = started.elapsed();
        assert_eq!(doc.blocks.len(), 6_000, "three blocks per repeated unit");
        let mut kinds: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for b in &doc.blocks {
            *kinds.entry(b.kind()).or_default() += 1;
        }
        assert_eq!(kinds.get("paragraph"), Some(&2_000), "{kinds:?}");
        assert_eq!(kinds.get("list"), Some(&4_000), "{kinds:?}");
        assert!(
            elapsed.as_millis() < 2_000,
            "parsed {} bytes in {elapsed:?}",
            src.len()
        );
    }
}
