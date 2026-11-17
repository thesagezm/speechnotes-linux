//! Split out of settings.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;

use crate::app::App;
use super::helpers::*;

// ---------------------------------------------------------------------------
// Book reader
// ---------------------------------------------------------------------------

/// The Book reader page: the book itself, as the reader shows it.
///
/// Every control here writes the pref *and* applies it to the open book
/// immediately — calibre's shape, where a settings page is a live view of the
/// book. The book's own design is the default and the starting point: the
/// "respect the book's styles" toggle is on, the colour override is "never",
/// and the font is the book's own. The reader's knobs are a layer over that,
/// never a replacement for it.
///
/// The Appearance page's spacing knobs stay where they are: they drive
/// `theme.css`, which the note preview and the label surfaces still use.
pub(crate) fn reader_page(app: &Rc<App>, page: &gtk::Box) {
    let prefs = app.prefs.get();

    // ---- theme -----------------------------------------------------------
    section(page, "Theme");
    let theme_row = row("Reader theme");
    let theme_names = ["Light", "Sepia", "Dark", "True black"];
    let theme = gtk::DropDown::from_strings(&theme_names);
    let theme_index = ["light", "sepia", "dark", "trueBlack"]
        .iter()
        .position(|t| *t == prefs.reader_theme)
        .unwrap_or(0);
    theme.set_selected(theme_index as u32);
    theme.set_hexpand(true);
    theme_row.append(&theme);
    page.append(&theme_row);

    // ---- type ------------------------------------------------------------
    // The type knobs are the reader's layer OVER the book, so they only
    // mean anything while the book's own styles are NOT respected. With
    // the toggle on they grey out — "respect" works like a reset: the book
    // renders as it shipped, and every override made to it (font, size,
    // spacing) stands down until the toggle goes off again.
    section(page, "Type");
    let font_row = row("Font");
    let font = gtk::DropDown::from_strings(&["Book's own", "Serif", "Sans-serif", "Monospace"]);
    let font_index = ["", "serif", "sans", "mono"]
        .iter()
        .position(|f| *f == prefs.reader_font)
        .unwrap_or(0);
    font.set_selected(font_index as u32);
    font.set_hexpand(true);
    font_row.append(&font);
    page.append(&font_row);

    let family_row = row("Font family");
    let family = gtk::Entry::builder()
        .text(&prefs.reader_font_family)
        .placeholder_text("A family name, overriding the font above")
        .hexpand(true)
        .build();
    family_row.append(&family);
    page.append(&family_row);

    let size_row = row("Text size");
    let size = gtk::SpinButton::with_range(12.0, 32.0, 1.0);
    size.set_digits(0);
    size.set_value(prefs.reader_font_size);
    size.set_hexpand(true);
    size_row.append(&size);
    page.append(&size_row);

    let min_size_row = row("Smallest text");
    let min_size = gtk::SpinButton::with_range(8.0, 16.0, 1.0);
    min_size.set_digits(0);
    min_size.set_value(prefs.reader_min_font_size);
    min_size.set_hexpand(true);
    min_size_row.append(&min_size);
    page.append(&min_size_row);

    let spacing_row = row("Letter spacing");
    let spacing = gtk::SpinButton::with_range(0.0, 4.0, 0.1);
    spacing.set_digits(1);
    spacing.set_value(prefs.reader_letter_spacing);
    spacing.set_hexpand(true);
    spacing_row.append(&spacing);
    page.append(&spacing_row);

    // One list, because the respect toggle drives all five together: the
    // rows (and the widgets inside them) go insensitive while the book's
    // own styles rule, and come back when it hands control back.
    let type_rows: Vec<gtk::Box> = vec![font_row, family_row, size_row, min_size_row, spacing_row];
    let type_widgets: Vec<gtk::Widget> = vec![
        font.clone().upcast::<gtk::Widget>(),
        family.clone().upcast::<gtk::Widget>(),
        size.clone().upcast::<gtk::Widget>(),
        min_size.clone().upcast::<gtk::Widget>(),
        spacing.clone().upcast::<gtk::Widget>(),
    ];
    for row_ in &type_rows {
        row_.set_sensitive(!prefs.reader_respect_styles);
    }
    for widget in &type_widgets {
        widget.set_sensitive(!prefs.reader_respect_styles);
    }

    // ---- margins ---------------------------------------------------------
    section(page, "Margins");
    for (label, value) in [
        ("Top", prefs.reader_margin_top),
        ("Bottom", prefs.reader_margin_bottom),
        ("Left", prefs.reader_margin_left),
        ("Right", prefs.reader_margin_right),
    ] {
        let margin_row = row(label);
        let margin = gtk::SpinButton::with_range(0.0, 96.0, 4.0);
        margin.set_digits(0);
        margin.set_value(value);
        margin.set_hexpand(true);
        margin_row.append(&margin);
        page.append(&margin_row);
        {
            let app = app.clone();
            let field = match label {
                "Top" => 0,
                "Bottom" => 1,
                "Left" => 2,
                _ => 3,
            };
            margin.connect_value_changed(move |spin| {
                let value = spin.value();
                app.prefs.update(move |p| match field {
                    0 => p.reader_margin_top = value,
                    1 => p.reader_margin_bottom = value,
                    2 => p.reader_margin_left = value,
                    _ => p.reader_margin_right = value,
                });
                crate::reader::apply_epub_appearance(&app);
            });
        }
    }

    let width_row = row("Text column width (0 = window)");
    let width = gtk::SpinButton::with_range(0.0, 2000.0, 40.0);
    width.set_digits(0);
    width.set_value(prefs.reader_max_width);
    width.set_hexpand(true);
    width_row.append(&width);
    page.append(&width_row);

    // ---- flow ------------------------------------------------------------
    section(page, "Flow");
    let flow_row = row("Page layout");
    let flow = gtk::DropDown::from_strings(&["Scroll", "Paged (one screen)"]);
    let flow_index = if prefs.reader_flow == "paged" { 1 } else { 0 };
    flow.set_selected(flow_index);
    flow.set_hexpand(true);
    flow_row.append(&flow);
    page.append(&flow_row);

    let columns_row = row("Columns when paged");
    let columns = gtk::SpinButton::with_range(1.0, 3.0, 1.0);
    columns.set_digits(0);
    columns.set_value(prefs.reader_columns as f64);
    columns.set_hexpand(true);
    columns_row.append(&columns);
    page.append(&columns_row);

    // ---- the book's own design ------------------------------------------
    section(page, "The book's own design");
    let respect_row = toggle_row(
        "Respect the book's styles",
        prefs.reader_respect_styles,
    );
    page.append(&respect_row.0);
    let override_row = row("Override the book's colours");
    let override_colors = gtk::DropDown::from_strings(&["Never", "In dark mode", "Always"]);
    let override_index = ["never", "dark", "always"]
        .iter()
        .position(|o| *o == prefs.reader_override_colors)
        .unwrap_or(0);
    override_colors.set_selected(override_index as u32);
    override_colors.set_hexpand(true);
    override_row.append(&override_colors);
    page.append(&override_row);

    // ---- write-through ---------------------------------------------------
    {
        let app = app.clone();
        theme.connect_selected_notify(move |dd| {
            let value = ["light", "sepia", "dark", "trueBlack"]
                [dd.selected() as usize]
                .to_string();
            app.prefs.update(move |p| p.reader_theme = value);
            crate::reader::apply_epub_appearance(&app);
        });
    }
    {
        let app = app.clone();
        font.connect_selected_notify(move |dd| {
            let value = ["", "serif", "sans", "mono"][dd.selected() as usize].to_string();
            app.prefs.update(move |p| p.reader_font = value);
            crate::reader::apply_epub_appearance(&app);
        });
    }
    {
        let app = app.clone();
        family.connect_changed(move |entry| {
            let value = entry.text().to_string();
            app.prefs.update(move |p| p.reader_font_family = value);
            crate::reader::apply_epub_appearance(&app);
        });
    }
    {
        let app = app.clone();
        size.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(move |p| p.reader_font_size = value);
            crate::reader::apply_epub_appearance(&app);
        });
    }
    {
        let app = app.clone();
        min_size.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(move |p| p.reader_min_font_size = value);
            crate::reader::apply_epub_appearance(&app);
        });
    }
    {
        let app = app.clone();
        spacing.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(move |p| p.reader_letter_spacing = value);
            crate::reader::apply_epub_appearance(&app);
        });
    }
    {
        let app = app.clone();
        width.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(move |p| p.reader_max_width = value);
            crate::reader::apply_epub_appearance(&app);
        });
    }
    {
        let app = app.clone();
        flow.connect_selected_notify(move |dd| {
            let value = if dd.selected() == 1 { "paged" } else { "flow" }.to_string();
            app.prefs.update(move |p| p.reader_flow = value);
            crate::reader::apply_epub_appearance(&app);
        });
    }
    {
        let app = app.clone();
        columns.connect_value_changed(move |spin| {
            let value = spin.value() as u32;
            app.prefs.update(move |p| p.reader_columns = value);
            crate::reader::apply_epub_appearance(&app);
        });
    }
    {
        let app = app.clone();
        override_colors.connect_selected_notify(move |dd| {
            let value = ["never", "dark", "always"][dd.selected() as usize].to_string();
            app.prefs.update(move |p| p.reader_override_colors = value);
            crate::reader::apply_epub_appearance(&app);
        });
    }
    {
        let app = app.clone();
        // "Respect the book's styles" is the reset switch: on, the book
        // renders as the publisher made it — the type layer stands down
        // (the knobs grey out below) and the open book re-dresses itself
        // immediately. Off, the reader's stored choices come back, exactly
        // as they were left.
        let respect_check = respect_row.1.clone();
        wire_reader_toggle(&app, respect_check.clone(), move |p, v| {
            p.reader_respect_styles = v;
        });
        let respect_rows: Vec<gtk::Box> = type_rows.clone();
        let respect_widgets: Vec<gtk::Widget> = type_widgets.clone();
        respect_check.connect_toggled(move |check| {
            let on = !check.is_active();
            for row_ in &respect_rows {
                row_.set_sensitive(on);
            }
            for widget in &respect_widgets {
                widget.set_sensitive(on);
            }
        });
    }
}
