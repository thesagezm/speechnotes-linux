//! The slash menu: typing `/` on an empty line offers the block types.
//!
//! The trigger rule and the insertion are pure functions here, tested
//! headlessly; the GTK layer only reads the caret, asks [`should_open`], shows
//! a popover, and applies whatever comes back.
//!
//! The trigger is deliberately narrow — an empty line, or one holding only
//! spaces. A `/` mid-sentence is a division sign to the person who typed it,
//! and a menu that appears over their prose would be a bug they could not work
//! around.

use gtk4::prelude::*;
use gtk4 as gtk;

use crate::format::{self, Edit, Selection};

/// A block the menu can insert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    pub label: &'static str,
    pub hint: &'static str,
    /// What the menu writes. `None` is a block that needs a dialog or a picker
    /// rather than a line of markdown.
    pub body: Option<&'static str>,
    /// A line prefix, for blocks that are one marker deep.
    pub prefix: Option<&'static str>,
}

pub const ITEMS: &[Item] = &[
    Item { label: "Heading 1", hint: "# ", body: None, prefix: Some("# ") },
    Item { label: "Heading 2", hint: "## ", body: None, prefix: Some("## ") },
    Item { label: "Heading 3", hint: "### ", body: None, prefix: Some("### ") },
    Item { label: "Bullet list", hint: "- ", body: None, prefix: Some("- ") },
    Item { label: "Numbered list", hint: "1. ", body: None, prefix: Some("1. ") },
    Item { label: "Task", hint: "- [ ] ", body: None, prefix: Some("- [ ] ") },
    Item { label: "Quote", hint: "> ", body: None, prefix: Some("> ") },
    Item { label: "Table", hint: "3 columns", body: None, prefix: None },
    Item { label: "Divider", hint: "---", body: Some("---"), prefix: None },
];

/// Should typing `/` here open the menu?
///
/// `line_before_caret` is the text from the start of the line to the caret.
pub fn should_open(line_before_caret: &str) -> bool {
    line_before_caret.trim().is_empty()
}

/// The edit that replaces a typed `/` with `item`.
pub fn insertion_for(text: &str, sel: Selection, item: &Item) -> Edit {
    // The `/` is the last character before the caret; replace it.
    let (a, b) = sel.ordered();
    let start = if b > a { a } else { a.saturating_sub(1) };
    if let Some(prefix) = item.prefix {
        Edit {
            start: start as i32,
            end: b as i32,
            text: prefix.to_string(),
            caret: (start + prefix.chars().count()) as i32,
        }
    } else if let Some(body) = item.body {
        format::insert_block(text, Selection::new(start as i32, b as i32), body)
    } else {
        // "Table" and anything else that is not a single marker.
        format::insert_block(
            text,
            Selection::new(start as i32, b as i32),
            &format::table_skeleton().trim_end().to_string(),
        )
    }
}

/// The popover list, anchored to `text_view` at its caret.
pub fn build_popover(
    text_view: &gtk::TextView,
    //  the original window.rs hands an `Rc`-wrapped closure and this
    // function clones it per row; the parameter is the owned `Rc` form.
    on_pick: std::rc::Rc<dyn Fn(&Item)>,
) -> gtk::Popover {
    let list = gtk::ListBox::builder()
        .css_classes(["slash-menu"])
        .selection_mode(gtk::SelectionMode::Single)
        .build();

    for (index, item) in ITEMS.iter().enumerate() {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .build();
        let label = gtk::Label::builder()
            .label(item.label)
            .xalign(0.0)
            .build();
        label.add_css_class("slash-label");
        let hint = gtk::Label::builder()
            .label(item.hint)
            .xalign(1.0)
            .build();
        hint.add_css_class("slash-hint");
        row.append(&label);
        row.append(&hint);
        let r = gtk::ListBoxRow::builder().child(&row).build();

        let on_pick = on_pick.clone();
        r.connect_activate(move |_| on_pick(item));
        list.append(&r);
        let _ = index;
    }

    let popover = gtk::Popover::builder()
        .child(&list)
        .autohide(true)
        .has_arrow(false)
        .build();
    popover.set_parent(text_view);
    position_at_caret(&popover, text_view);
    list.grab_focus();
    popover
}

/// Put the popover under the caret.
fn position_at_caret(popover: &gtk::Popover, text_view: &gtk::TextView) {
    // (start, end) is the caret's rectangle in widget coordinates; GTK returns
    // zeros before the first allocation, and a zero rect shows the popover in
    // the top-left corner, which is worse than not showing it at all.
    let (start, _) = text_view.cursor_locations(None);
    if start.width() == 0 && start.height() == 0 {
        return;
    }
    //  gtk4 0.9's compute_bounds takes no flag and returns a Rect, not
    // the pair the original expression reads.
    let origin = text_view
        .compute_bounds(text_view)
        .unwrap_or_default();
    let rect = gtk::gdk::Rectangle::new(
        origin.x() as i32 + start.x(),
        origin.y() as i32 + start.y() + start.height(),
        1,
        1,
    );
    popover.set_pointing_to(Some(&rect));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(_text: &str, at: usize) -> Selection {
        Selection::caret(at as i32)
    }

    #[test]
    fn the_menu_opens_only_on_a_blank_line() {
        assert!(should_open(""));
        assert!(should_open("   "));
        assert!(should_open("\t"));
        // Anything with a word on the line is prose, and a `/` there is a
        // division sign.
        assert!(!should_open("a"));
        assert!(!should_open("12 / 4"));
        assert!(!should_open("see /usr/bin"));
    }

    #[test]
    fn picking_a_list_item_replaces_the_typed_slash() {
        let text = "/";
        let item = ITEMS.iter().find(|i| i.label == "Bullet list").unwrap();
        let e = insertion_for(text, caret(text, 1), item);
        assert_eq!(e.text, "- ");
        assert_eq!((e.start, e.end), (0, 1), "the slash is consumed");
        assert_eq!(e.caret, 2, "the caret lands after the marker");
    }

    #[test]
    fn picking_a_heading_swaps_the_slash_for_the_marker() {
        let text = "/";
        let item = ITEMS.iter().find(|i| i.label == "Heading 2").unwrap();
        let e = insertion_for(text, caret(text, 1), item);
        assert_eq!(e.text, "## ");
    }

    #[test]
    fn picking_the_table_inserts_a_whole_skeleton() {
        let text = "/";
        let item = ITEMS.iter().find(|i| i.label == "Table").unwrap();
        let e = insertion_for(text, caret(text, 1), item);
        let doc = sn_format::parse_markdown(&e.text);
        assert_eq!(
            doc.blocks[0].kind(),
            "table",
            "the inserted text must render as a table straight away"
        );
        assert!(!e.text.contains('/'), "the slash is consumed: {}", e.text);
    }

    #[test]
    fn picking_on_an_indented_line_keeps_the_indent() {
        // The selection starts after the indent, so the marker goes after it
        // and the line stays a list inside a list rather than jumping back out.
        let text = "  /";
        let item = ITEMS.iter().find(|i| i.label == "Bullet list").unwrap();
        let e = insertion_for(text, caret(text, 3), item);
        assert_eq!((e.start, e.end), (2, 3));
        assert_eq!(e.text, "- ");
    }

    #[test]
    fn every_item_produces_something_parseable() {
        for item in ITEMS {
            let text = "/";
            let e = insertion_for(text, caret(text, 1), item);
            assert!(!e.text.is_empty(), "{} inserted nothing", item.label);
            assert!(!e.text.contains('/'), "{} left the slash", item.label);
            // A divider is a rule; the rest are blocks the parser knows.
            let doc = sn_format::parse_markdown(&e.text);
            assert!(!doc.blocks.is_empty(), "{} parsed to nothing", item.label);
        }
    }
}
