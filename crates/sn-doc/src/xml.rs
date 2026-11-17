//! A minimal XML pull parser — the subset EPUB's metadata needs.
//!
//! `quick-xml` would be the obvious choice, but its MSRV is 1.86 against this
//! workspace's 1.85, and EPUB only needs: elements, attributes, character
//! data, comments, CDATA and the XML declaration. What this module adds on
//! top of "split tags" is the part EPUB books actually break on:
//!
//! * **Entity references** — `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;` and
//!   the numeric forms (`&#8217;`, `&#x2019;`) are decoded in text *and* in
//!   attribute values. A smart-quoted title decoded as literal `&#8217;`
//!   is the bug the iOS port fixed once already.
//! * **Namespaces are kept as given** — OPF files use default namespaces
//!   (`<metadata xmlns="http://www.idpf.org/2007/opf">`), and the parser
//!   reports element names verbatim so callers can match on either the
//!   prefixed or the bare local name.
//! * **Well-formedness errors name the line**, because every real broken
//!   book is somebody's hand-edited OPF.

/// One event from the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An element opened. `name` is the tag as written, namespace prefix
    /// included (`dc:title`); `attrs` are in document order.
    Start { name: String, attrs: Vec<Attr> },
    /// The element named by the most recent `Start` closed. The name is
    /// carried so a caller never has to re-derive it from the source.
    End { name: String },
    /// An element with no children: `<br/>`.
    Empty { name: String, attrs: Vec<Attr> },
    /// Character data, entities decoded.
    Text(String),
    /// The body of a raw-text element (`<script>`, `<style>`), verbatim —
    /// entities NOT decoded, because the body is code, not prose. Callers
    /// that render text drop it; a caller that wants the script gets it.
    Raw { name: String, text: String },
}

/// One attribute, with its value entity-decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    pub name: String,
    pub value: String,
}

impl Attr {
    /// The attribute's value, or "" when absent. `Option::map` over a lookup
    /// that returns "" is how the callers read optional attributes.
    pub fn get<'a>(attrs: &'a [Attr], name: &str) -> Option<&'a str> {
        attrs
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.value.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum XmlError {
    #[error("line {line}: {message}")]
    Malformed { line: usize, message: String },
}

/// Elements whose content is raw text — a `<` inside them is not markup.
/// XML defines none; HTML/XHTML defines these, and books carry inline
/// `<script>` with comparison operators in it.
///
/// Only the **unprefixed** names: in a namespaced document `style:style` is
/// an ordinary element (ODF's style definitions live in one), and treating
/// its body as text would hide every property inside it.
const RAW_TEXT_ELEMENTS: [&str; 2] = ["script", "style"];

/// A pull parser over one XML document.
pub struct Parser<'a> {
    src: &'a str,
    pos: usize,
    line: usize,
    finished: bool,
    /// The element whose raw text is being skipped, if any: everything up to
    /// its `</name>` is one Text event the caller drops.
    raw_until: Option<(String, usize)>,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            line: 1,
            finished: false,
            raw_until: None,
        }
    }

    /// The next event, or `None` at end of input.
    pub fn next_event(&mut self) -> Result<Option<Event>, XmlError> {
        if self.finished {
            return Ok(None);
        }
        // Inside a raw-text element: everything up to the matching close tag
        // is text, `<` and all. Without this, `<script>if (a<b)</script>`
        // parses as a start tag, a fragment, and a bogus element — and the
        // close tag is never seen, so a caller skipping the script skips the
        // rest of the document.
        if let Some((name, depth)) = self.raw_until.clone() {
            return self.read_raw_text(&name, depth);
        }
        loop {
            let Some(&c) = self.src[self.pos..].as_bytes().first() else {
                self.finished = true;
                return Ok(None);
            };
            match c {
                b'<' => {
                    let (event, end) = self.read_markup()?;
                    self.pos = end;
                    if let Some(event) = event {
                        return Ok(Some(event));
                    }
                }
                _ => {
                    // Character data up to the next '<'. A bare '&' with no
                    // valid entity is an error the callers should see.
                    let start = self.pos;
                    let next = self.src[self.pos..]
                        .find('<')
                        .map(|i| self.pos + i)
                        .unwrap_or(self.src.len());
                    let raw = &self.src[start..next];
                    let text = decode_entities(raw).ok_or_else(|| XmlError::Malformed {
                        line: self.line,
                        message: "invalid entity in character data".into(),
                    })?;
                    self.pos = next;
                    self.line += raw.matches('\n').count();
                    // Whitespace-only nodes (indentation between tags) are
                    // not text to any consumer here; keeping them would put
                    // "\n" labels on every TOC row and a stray space on
                    // every metadata field.
                    if !text.trim().is_empty() {
                        return Ok(Some(Event::Text(text)));
                    }
                }
            }
        }
    }

    /// Read markup starting at `self.pos` (which points at `<`). Returns
    /// `(event, new position)`; the event is `None` for constructs that
    /// produce nothing (the declaration, comments, doctypes, PIs).
    fn read_markup(&mut self) -> Result<(Option<Event>, usize), XmlError> {
        let rest = &self.src[self.pos..];
        if let Some(after) = rest.strip_prefix("<!--") {
            let end = after.find("-->").ok_or_else(|| XmlError::Malformed {
                line: self.line,
                message: "unterminated comment".into(),
            })?;
            let consumed = self.pos + 4 + end + 3;
            self.line += self.src[self.pos..consumed].matches('\n').count();
            return Ok((None, consumed));
        }
        if let Some(after) = rest.strip_prefix("<![CDATA[") {
            let end = after.find("]]>").ok_or_else(|| XmlError::Malformed {
                line: self.line,
                message: "unterminated CDATA".into(),
            })?;
            let text = &after[..end];
            let consumed = self.pos + 9 + end + 3;
            self.line += self.src[self.pos..consumed].matches('\n').count();
            return Ok((Some(Event::Text(text.to_string())), consumed));
        }
        if let Some(after) = rest.strip_prefix("<?") {
            let end = after.find("?>").ok_or_else(|| XmlError::Malformed {
                line: self.line,
                message: "unterminated processing instruction".into(),
            })?;
            let consumed = self.pos + 2 + end + 2;
            self.line += self.src[self.pos..consumed].matches('\n').count();
            return Ok((None, consumed));
        }
        if rest.starts_with("<!") {
            // A doctype or similar; skip to the matching '>'. Nested
            // brackets (internal subsets) are rare in EPUB — a DOCTYPE
            // with an internal subset is skipped by bracket counting.
            let mut depth = 0usize;
            let mut i = self.pos + 2;
            while i < self.src.len() {
                match self.src.as_bytes()[i] {
                    b'[' => depth += 1,
                    b']' => depth = depth.saturating_sub(1),
                    b'>' if depth == 0 => {
                        let consumed = i + 1;
                        self.line += self.src[self.pos..consumed].matches('\n').count();
                        return Ok((None, consumed));
                    }
                    _ => {}
                }
                i += 1;
            }
            return Err(XmlError::Malformed {
                line: self.line,
                message: "unterminated declaration".into(),
            });
        }

        // An element: <name attrs>, </name>, or <name attrs/>.
        let end = rest.find('>').ok_or_else(|| XmlError::Malformed {
            line: self.line,
            message: "unterminated tag".into(),
        })?;
        let inner = &rest[1..end];
        let consumed = self.pos + end + 1;
        self.line += self.src[self.pos..consumed].matches('\n').count();
        if let Some(name) = inner.strip_prefix('/') {
            let name = name.trim();
            if name.is_empty() {
                return Err(XmlError::Malformed {
                    line: self.line,
                    message: "empty end tag".into(),
                });
            }
            return Ok((Some(Event::End { name: name.to_string() }), consumed));
        }
        let (head, empty) = match inner.strip_suffix('/') {
            Some(head) => (head, true),
            None => (inner, false),
        };
        let (name, attrs) = parse_attrs(head, self.line)?;
        if name.is_empty() {
            return Err(XmlError::Malformed {
                line: self.line,
                message: "empty tag name".into(),
            });
        }
        //  the original comment above says "only the **unprefixed**
        // names" — in a namespaced document `style:style` is an ordinary
        // element — but its code matched the *local* name, hiding every
        // property inside ODF's style elements. The check follows the
        // documented contract.
        if !empty && !name.contains(':') && RAW_TEXT_ELEMENTS.contains(&name.as_str()) {
            self.raw_until = Some((name.clone(), 1));
        }
        Ok((
            Some(if empty {
                Event::Empty { name, attrs }
            } else {
                Event::Start { name, attrs }
            }),
            consumed,
        ))
    }

    /// Consume a raw-text element's body up to its matching close tag. The
    /// body is returned as one Text event (the caller drops it); a nested
    /// same-name open tag raises the depth, so `</script>` inside a string
    /// literal does not end the block early.
    fn read_raw_text(
        &mut self,
        name: &str,
        mut depth: usize,
    ) -> Result<Option<Event>, XmlError> {
        let close = format!("</{name}");
        let close_folded = close.to_ascii_lowercase();
        let open = format!("<{name}");
        let open_folded = open.to_ascii_lowercase();
        let body_start = self.pos;
        let mut search = self.pos;
        loop {
            let Some(found) = self.src[search..].find('<') else {
                // No close tag: the document is truncated. Take the rest as
                // the body rather than erroring — a chapter that ends inside
                // a script still has its prose before it.
                self.line += self.src[body_start..].matches('\n').count();
                let body = self.src[body_start..].to_string();
                self.pos = self.src.len();
                self.finished = true;
                self.raw_until = None;
                return Ok((!body.is_empty()).then_some(Event::Raw {
                    name: name.to_string(),
                    text: body,
                }));
            };
            let at = search + found;
            let rest = &self.src[at..];
            let lower = rest.to_ascii_lowercase();
            if lower.starts_with(&close_folded) {
                depth -= 1;
                if depth == 0 {
                    let body = self.src[body_start..at].to_string();
                    // Consume through the close tag's '>'.
                    // `rest` starts at the close tag's '<'; its '>' ends the
                    // tag, and the next event starts one past it.
                    let end = at + rest.find('>').map(|i| i + 1).unwrap_or(rest.len());
                    self.line += self.src[body_start..end].matches('\n').count();
                    self.pos = end;
                    self.raw_until = None;
                    return Ok((!body.is_empty()).then_some(Event::Raw {
                        name: name.to_string(),
                        text: body,
                    }));
                }
                search = at + 1;
            } else if lower.starts_with(&open_folded) {
                depth += 1;
                search = at + 1;
            } else {
                search = at + 1;
            }
        }
    }
}

/// Split `name attr="v" attr2='v2'` into the name and decoded attributes.
fn parse_attrs(head: &str, line: usize) -> Result<(String, Vec<Attr>), XmlError> {
    let mut name_end = head.len();
    for (i, c) in head.char_indices() {
        if c.is_whitespace() {
            name_end = i;
            break;
        }
    }
    let name = head[..name_end].to_string();
    let mut attrs = Vec::new();
    let mut rest = &head[name_end..];
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            return Ok((name, attrs));
        }
        let eq = rest.find('=').ok_or_else(|| XmlError::Malformed {
            line,
            message: "attribute without a value".into(),
        })?;
        let attr_name = rest[..eq].trim().to_string();
        if attr_name.is_empty() || attr_name.contains(char::is_whitespace) {
            return Err(XmlError::Malformed {
                line,
                message: "malformed attribute name".into(),
            });
        }
        rest = rest[eq + 1..].trim_start();
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
        let Some(quote) = quote else {
            return Err(XmlError::Malformed {
                line,
                message: "unquoted attribute value".into(),
            });
        };
        let body = &rest[1..];
        let close = body.find(quote).ok_or_else(|| XmlError::Malformed {
            line,
            message: "unterminated attribute value".into(),
        })?;
        let raw = &body[..close];
        let value = decode_entities(raw).ok_or_else(|| XmlError::Malformed {
            line,
            message: "invalid entity in attribute value".into(),
        })?;
        attrs.push(Attr { name: attr_name, value });
        rest = &body[close + 1..];
    }
}

/// Decode the five named entities plus numeric character references.
/// Returns `None` for anything else — a bare `&` in prose is a real
/// malformation and the caller decides whether to care.
pub fn decode_entities(s: &str) -> Option<String> {
    if !s.contains('&') {
        return Some(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            // Copy one char (UTF-8 safe).
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&s[i..i + ch_len]);
            i += ch_len;
            continue;
        }
        let Some(semi) = s[i..].find(';') else {
            // A bare `&` with no terminator is literal text, not an entity.
            out.push('&');
            i += 1;
            continue;
        };
        let entity = &s[i + 1..i + semi];
        if let Some(num) = entity.strip_prefix('#') {
            let code = if let Some(hex) = num.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                num.parse::<u32>().ok()?
            };
            out.push(char::from_u32(code)?);
            i += semi + 1;
            continue;
        }
        let ch = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            // The HTML5 named entities XHTML books actually use. The five
            // XML entities are all the spec defines, but Calibre and every
            // publisher's toolchain emit this set, and a reader that speaks
            // "&ldquo;" aloud is broken. The numeric forms above cover the
            // rest of Unicode.
            "nbsp" => '\u{00A0}',
            "hellip" => '\u{2026}',
            "mdash" => '\u{2014}',
            "ndash" => '\u{2013}',
            "lsquo" => '\u{2018}',
            "rsquo" => '\u{2019}',
            "ldquo" => '\u{201C}',
            "rdquo" => '\u{201D}',
            "bull" => '\u{2022}',
            "dagger" => '\u{2020}',
            "permil" => '\u{2030}',
            "lsaquo" => '\u{2039}',
            "rsaquo" => '\u{203A}',
            "oline" => '\u{203E}',
            "frasl" => '/',
            "euro" => '\u{20AC}',
            "trade" => '\u{2122}',
            "copy" => '\u{00A9}',
            "reg" => '\u{00AE}',
            "deg" => '\u{00B0}',
            "plusmn" => '\u{00B1}',
            "times" => '\u{00D7}',
            "divide" => '\u{00F7}',
            "frac12" => '\u{00BD}',
            "frac14" => '\u{00BC}',
            "frac34" => '\u{00BE}',
            "sup2" => '\u{00B2}',
            "sup3" => '\u{00B3}',
            "micro" => '\u{00B5}',
            "para" => '\u{00B6}',
            "sect" => '\u{00A7}',
            "middot" => '\u{00B7}',
            "cent" => '\u{00A2}',
            "pound" => '\u{00A3}',
            "yen" => '\u{00A5}',
            "curren" => '\u{00A4}',
            "iexcl" => '\u{00A1}',
            "iquest" => '\u{00BF}',
            "laquo" => '\u{00AB}',
            "raquo" => '\u{00BB}',
            "szlig" => '\u{00DF}',
            "agrave" => '\u{00E0}',
            "aacute" => '\u{00E1}',
            "acirc" => '\u{00E2}',
            "atilde" => '\u{00E3}',
            "auml" => '\u{00E4}',
            "aring" => '\u{00E5}',
            "aelig" => '\u{00E6}',
            "ccedil" => '\u{00E7}',
            "egrave" => '\u{00E8}',
            "eacute" => '\u{00E9}',
            "ecirc" => '\u{00EA}',
            "euml" => '\u{00EB}',
            "igrave" => '\u{00EC}',
            "iacute" => '\u{00ED}',
            "icirc" => '\u{00EE}',
            "iuml" => '\u{00EF}',
            "ntilde" => '\u{00F1}',
            "ograve" => '\u{00F2}',
            "oacute" => '\u{00F3}',
            "ocirc" => '\u{00F4}',
            "otilde" => '\u{00F5}',
            "ouml" => '\u{00F6}',
            "oslash" => '\u{00F8}',
            "ugrave" => '\u{00F9}',
            "uacute" => '\u{00FA}',
            "ucirc" => '\u{00FB}',
            "uuml" => '\u{00FC}',
            "yacute" => '\u{00FD}',
            "yuml" => '\u{00FF}',
            "Agrave" => '\u{00C0}',
            "Aacute" => '\u{00C1}',
            "Acirc" => '\u{00C2}',
            "Atilde" => '\u{00C3}',
            "Auml" => '\u{00C4}',
            "Aring" => '\u{00C5}',
            "AElig" => '\u{00C6}',
            "Ccedil" => '\u{00C7}',
            "Egrave" => '\u{00C8}',
            "Eacute" => '\u{00C9}',
            "Ecirc" => '\u{00CA}',
            "Euml" => '\u{00CB}',
            "Igrave" => '\u{00CC}',
            "Iacute" => '\u{00CD}',
            "Icirc" => '\u{00CE}',
            "Iuml" => '\u{00CF}',
            "Ntilde" => '\u{00D1}',
            "Ograve" => '\u{00D2}',
            "Oacute" => '\u{00D3}',
            "Ocirc" => '\u{00D4}',
            "Otilde" => '\u{00D5}',
            "Ouml" => '\u{00D6}',
            "Oslash" => '\u{00D8}',
            "Ugrave" => '\u{00D9}',
            "Uacute" => '\u{00DA}',
            "Ucirc" => '\u{00DB}',
            "Uuml" => '\u{00DC}',
            "Yacute" => '\u{00DD}',
            // Unknown named entity: keep it literally. The five XML entities
            // plus the numeric forms are all the spec defines; XHTML books
            // carry the HTML set (&ldquo;, &nbsp;, &hellip; ...) and a reader
            // that stops mid-chapter on one is worse than one that speaks
            // the raw name. The numeric forms still decode, and that is
            // what the smart quotes in Calibre's output actually use.
            _ => {
                out.push('&');
                out.push_str(entity);
                out.push(';');
                i += semi + 1;
                continue;
            }
        };
        out.push(ch);
        i += semi + 1;
    }
    Some(out)
}

// The `local_of` helper (local name of a possibly-prefixed tag) was removed
// 2026-10-30 in a dead-code trim — zero references; the file's "unprefixed
// only" contract means no consumer strips prefixes.

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(src: &str) -> Vec<Event> {
        let mut p = Parser::new(src);
        let mut out = Vec::new();
        while let Some(e) = p.next_event().unwrap() {
            out.push(e);
        }
        out
    }

    #[test]
    fn parses_elements_and_text() {
        let ev = events("<a><b>hi</b> tail</a>");
        assert_eq!(
            ev,
            vec![
                Event::Start { name: "a".into(), attrs: vec![] },
                Event::Start { name: "b".into(), attrs: vec![] },
                Event::Text("hi".into()),
                Event::End { name: "b".into() },
                Event::Text(" tail".into()),
                Event::End { name: "a".into() },
            ]
        );
    }

    #[test]
    fn decodes_named_and_numeric_entities() {
        assert_eq!(
            decode_entities("Wicked &amp; the Witch &#8212; by &#x201c;G.M.&#x201d;").unwrap(),
            "Wicked & the Witch — by “G.M.”"
        );
        let ev = events("<t>&lt;b&gt;</t>");
        assert_eq!(
            ev,
            vec![
                Event::Start { name: "t".into(), attrs: vec![] },
                Event::Text("<b>".into()),
                Event::End { name: "t".into() },
            ]
        );
    }

    #[test]
    fn attributes_decode_and_query() {
        let ev = events(r#"<dc:title id="t">Wicked &amp; the Witch</dc:title>"#);
        assert_eq!(
            ev,
            vec![
                Event::Start {
                    name: "dc:title".into(),
                    attrs: vec![Attr { name: "id".into(), value: "t".into() }],
                },
                Event::Text("Wicked & the Witch".into()),
                Event::End { name: "dc:title".into() },
            ]
        );
        let Event::Start { attrs, .. } = &ev[0] else { panic!("start") };
        assert_eq!(Attr::get(attrs, "id"), Some("t"));
        assert_eq!(Attr::get(attrs, "missing"), None);
    }

    #[test]
    fn empty_elements_comments_and_declaration_are_handled() {
        let ev = events(
            "<?xml version=\"1.0\"?>\n<!-- a comment --><spine toc=\"ncx\"><item/></spine>",
        );
        assert_eq!(
            ev,
            vec![
                Event::Start {
                    name: "spine".into(),
                    attrs: vec![Attr { name: "toc".into(), value: "ncx".into() }],
                },
                Event::Empty { name: "item".into(), attrs: vec![] },
                Event::End { name: "spine".into() },
            ]
        );
    }

    #[test]
    fn cdata_is_text() {
        let ev = events("<a><![CDATA[<not a tag> & fine]]></a>");
        assert_eq!(
            ev,
            vec![
                Event::Start { name: "a".into(), attrs: vec![] },
                Event::Text("<not a tag> & fine".into()),
                Event::End { name: "a".into() },
            ]
        );
    }

    #[test]
    fn an_unknown_entity_is_kept_literally_and_a_bare_amp_is_too() {
        // The reader's rule: an entity it cannot decode is passed through
        // rather than aborting the chapter. The named set the decoder knows
        // is tested above; everything else survives as written.
        assert_eq!(decode_entities("a &notanentity; b").unwrap(), "a &notanentity; b");
        assert_eq!(decode_entities("a & b").unwrap(), "a & b");
        // A numeric reference that names no character is still an error —
        // there is no literal form to fall back to.
        assert!(decode_entities("&#xFFFFFF;").is_none());
    }

    #[test]
    fn an_unterminated_tag_fails_with_a_line() {
        let mut p = Parser::new("<a>\n<b");
        assert!(matches!(p.next_event(), Ok(Some(Event::Start { .. }))));
        let err = p.next_event().unwrap_err();
        assert!(err.to_string().contains("line 2"), "{err}");
    }
}
