//! Editor text formatting over a `gtk::TextBuffer`.
//!
//!  no original copy of this module surfaced (the only module in the
//! original `lib.rs` with zero older copies). The whole file is authored
//! from call sites — `window.rs` (`FormatAction` handling, the slash-menu
//! hook, image insertion), `slash.rs` (`Edit` / `Selection` field-for-field)
//! and `export.rs`'s "each reference becomes `images/<name>`" doc comment —
//! with the public surface pinned by `slash.rs`'s own tests.

use gtk4::prelude::*;
use gtk4 as gtk;

/// A replacement to apply to the note's source buffer: replace the range
/// `[start, end)` (character offsets) with `text`, then leave the caret at
/// `caret`.
///
/// The shape is pinned field-for-field by `slash.rs` (`Edit { start, end,
/// text, caret }`, applied with `edit.apply(&buffer)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: i32,
    pub end: i32,
    pub text: String,
    pub caret: i32,
}

impl Edit {
    /// Apply the replacement to `buffer` as one undoable user action.
    pub fn apply(&self, buffer: &gtk::TextBuffer) {
        buffer.begin_user_action();
        let mut start = buffer.iter_at_offset(self.start);
        let mut end = buffer.iter_at_offset(self.end);
        buffer.delete(&mut start, &mut end);
        buffer.insert(&mut start, &self.text);
        let caret = buffer.iter_at_offset(self.caret);
        buffer.place_cursor(&caret);
        buffer.end_user_action();
    }
}

/// A caret / selection position pair in character offsets.
///
/// `ordered()` returns `(start, end)` with `start <= end`; the raw pair may
/// be reversed (`start` is the anchor, `end` the caret — `slash.rs` relies on
/// `a > b` meaning "empty selection anchored after the caret").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: i32,
    pub end: i32,
}

impl Selection {
    pub fn new(start: i32, end: i32) -> Self {
        Self { start, end }
    }

    /// An empty selection with the caret at `at`.
    pub fn caret(at: i32) -> Self {
        Self { start: at, end: at }
    }

    /// `(first_ordered, last_ordered)`, both as `usize`.
    pub fn ordered(&self) -> (usize, usize) {
        if self.start <= self.end {
            (self.start as usize, self.end as usize)
        } else {
            (self.end as usize, self.start as usize)
        }
    }
}

/// The buffer's whole text plus the current selection (character offsets).
pub fn text_and_selection(buffer: &gtk::TextBuffer) -> (String, Selection) {
    let text = buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .to_string();
    let (start, end) = match buffer.selection_bounds() {
        Some((a, b)) => (a.offset(), b.offset()),
        None => {
            let c = buffer.iter_at_offset(buffer.cursor_position());
            (c.offset(), c.offset())
        }
    };
    (text, Selection::new(start, end))
}

/// The text from the start of its line up to the caret position `at`.
pub fn line_before_caret(text: &str, at: usize) -> String {
    let before = char_slice(text, at);
    match before.rfind('\n') {
        Some(i) => before[i + 1..].to_string(),
        None => before.to_string(),
    }
}

/// The currently selected text (`""` when the selection is empty).
pub fn selected_text(text: &str, sel: Selection) -> String {
    let (a, b) = sel.ordered();
    text.chars().skip(a).take(b.saturating_sub(a)).collect()
}

/// Wrap (or unwrap) the selection in `marker` (bold/italic/code/…).
pub fn toggle_wrap(text: &str, sel: Selection, marker: &str) -> Edit {
    let (a, b) = sel.ordered();
    let selected = selected_text(text, sel);
    if selected.starts_with(marker) && selected.ends_with(marker) {
        let inner = &selected[marker.len()..selected.len() - marker.len()];
        return Edit {
            start: a as i32,
            end: b as i32,
            text: inner.to_string(),
            caret: a as i32 + inner.chars().count() as i32,
        };
    }
    let wrapped = format!("{marker}{selected}{marker}");
    let caret = if selected.is_empty() {
        a + marker.chars().count()
    } else {
        a + wrapped.chars().count()
    };
    Edit {
        start: a as i32,
        end: b as i32,
        text: wrapped,
        caret: caret as i32,
    }
}

/// Toggle `prefix` on every line the selection touches (headings, lists…).
pub fn toggle_line_prefix(text: &str, sel: Selection, prefix: &str) -> Edit {
    let (a, b) = sel.ordered();
    // Expand to whole lines: the edit replaces the line range wholesale.
    let line_start = text[..char_off(text, a)]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let line_end = text[char_off(text, b)..]
        .find('\n')
        .map(|i| char_off(text, b) + i)
        .unwrap_or(text.len());
    let lines = text[line_start..line_end].split('\n');
    let mut out = String::new();
    let all_prefixed = lines
        .clone()
        .filter(|l| !l.trim().is_empty())
        .all(|l| l.starts_with(prefix));
    for (i, line) in lines.enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if all_prefixed {
            out.push_str(line.strip_prefix(prefix).unwrap_or(line));
        } else if line.trim().is_empty() {
            out.push_str(line);
        } else {
            out.push_str(prefix);
            out.push_str(line);
        }
    }
    Edit {
        start: byte_to_char(text, line_start) as i32,
        end: byte_to_char(text, line_end) as i32,
        caret: a as i32,
        text: out,
    }
}

/// Replace the selection with a block (a table skeleton, a divider, …).
pub fn insert_block(_text: &str, sel: Selection, block: &str) -> Edit {
    let (a, b) = sel.ordered();
    let block = block.trim_end().to_string();
    let caret = a + block.chars().count();
    Edit {
        start: a as i32,
        end: b as i32,
        text: block,
        caret: caret as i32,
    }
}

/// Turn the selection into `[text](url)`; with an empty selection, insert a
/// link skeleton and leave the caret on the url.
pub fn insert_link(text: &str, sel: Selection) -> Edit {
    let (a, b) = sel.ordered();
    let selected = selected_text(text, sel);
    let label = if selected.is_empty() { "link" } else { selected.as_str() };
    let full = format!("[{label}](url)");
    let caret = (a + 1 + label.chars().count() + 3) as i32; // just after `](`
    Edit {
        start: a as i32,
        end: b as i32,
        text: full,
        caret,
    }
}

/// Insert a stored image reference: `![alt](images/<name>)` (the `images/`
/// prefix is what `export.rs` documents for references another app can read).
pub fn insert_image(_text: &str, sel: Selection, name: &str, alt: &str) -> Edit {
    let (a, b) = sel.ordered();
    let full = format!("![{alt}](images/{name})");
    Edit {
        start: a as i32,
        end: b as i32,
        caret: (a + full.chars().count()) as i32,
        text: full,
    }
}

/// A three-column GFM table a fresh note can start typing into.
pub fn table_skeleton() -> String {
    "|  |  |  |\n| --- | --- | --- |\n|  |  |  |\n".to_string()
}

// -- helpers ----------------------------------------------------------------

/// `text` truncated to `n` characters.
fn char_slice(text: &str, n: usize) -> &str {
    &text[..char_off(text, n)]
}

/// Byte offset of the `n`th character.
fn char_off(text: &str, n: usize) -> usize {
    text.char_indices()
        .nth(n)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

/// Character offset of a byte position.
fn byte_to_char(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

#[cfg(test)]
mod tests {
    //  authored with the module above; pinned to the same contracts the
    // call sites use (slash.rs's tests cover the Edit/Selection shape).
    use super::*;

    #[test]
    fn wrap_toggles_off_an_existing_marker() {
        let e = toggle_wrap("a **b** c", Selection::new(2, 7), "**");
        assert_eq!(e.text, "b");
        let e = toggle_wrap("a b c", Selection::new(2, 3), "**");
        assert_eq!(e.text, "**b**");
    }

    #[test]
    fn line_prefix_applies_to_whole_lines_and_back() {
        // The edit replaces the whole touched line range.
        let e = toggle_line_prefix("one\ntwo\nthree", Selection::new(5, 5), "- ");
        assert_eq!(e.text, "- two");
        assert_eq!((e.start, e.end), (4, 7));
        let e2 = toggle_line_prefix("one\n- two\nthree", Selection::new(5, 5), "- ");
        assert_eq!(e2.text, "two");
    }

    #[test]
    fn the_table_skeleton_parses_as_a_table() {
        let doc = sn_format::parse_markdown(&table_skeleton());
        assert_eq!(doc.blocks[0].kind(), "table");
    }

    #[test]
    fn line_before_caret_stops_at_the_line_start() {
        assert_eq!(line_before_caret("abc\ndef", 4), "");
        assert_eq!(line_before_caret("abc\ndef", 7), "def");
        assert_eq!(line_before_caret("abc", 3), "abc");
    }
}
