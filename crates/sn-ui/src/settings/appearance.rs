//! Split out of settings.rs 2026-10-31 (pure code-motion).

use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;

use crate::app::App;
use super::helpers::*;
use crate::theme;

// ---------------------------------------------------------------------------
// Appearance
// ---------------------------------------------------------------------------

pub(crate) fn appearance_page(app: &Rc<App>, page: &gtk::Box) {
    let prefs = app.prefs.get();

    section(page, "Theme");
    let appearance_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    appearance_row.append(&gtk::Label::new(Some("Theme")));
    //  the original dropdown predates the Warm appearance; appended
    // last so the original indices for System/Light/Dark stay put. E-Ink
    // appended after Warm for the same reason (eink-theme, 2026-11-10).
    let appearance = gtk::DropDown::from_strings(&["System", "Light", "Dark", "Warm", "E-Ink"]);
    let appearance_index = match prefs.appearance() {
        sn_store::model::Appearance::Light => 1,
        sn_store::model::Appearance::Dark => 2,
        sn_store::model::Appearance::Warm => 3,
        sn_store::model::Appearance::EInk => 4,
        sn_store::model::Appearance::System => 0,
    };
    appearance.set_selected(appearance_index as u32);
    appearance.set_hexpand(true);
    appearance_row.append(&appearance);
    page.append(&appearance_row);

    // The twelve accents as named swatches, one toggle per row of buttons.
    let accent_grid = gtk::FlowBox::builder()
        .max_children_per_line(6)
        .min_children_per_line(6)
        .column_spacing(6)
        .row_spacing(6)
        .selection_mode(gtk::SelectionMode::None)
        .build();
    let accent = prefs.accent().name().to_string();
    for name in sn_store::model::ACCENTS {
        let button = gtk::ToggleButton::new();
        button.set_size_request(36, 28);
        button.set_tooltip_text(Some(name));
        // A colored disc, drawn with the accent's hex: the swatch IS the
        // data, so a wrong name cannot reach the CSS unseen.
        let css_provider = gtk::CssProvider::new();
        let accent_class = format!("accent-{}", name);
        css_provider.load_from_string(
            &format!(".accent-swatch.{} {{ background: {}; }}", accent_class, theme::accent_hex(name)),
        );
        let display = gdk4_display();
        gtk::style_context_add_provider_for_display(
            &display,
            &css_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        button.add_css_class("accent-swatch");
        button.add_css_class(&accent_class);
        if name == accent {
            button.set_active(true);
        }
        {
            let app = app.clone();
            let name = name.to_string();
            button.connect_toggled(move |btn| {
                // A programmatic set from the picker's own refill must not
                // write; only the user's click (button is active) does.
                if !btn.is_active() {
                    return;
                }
                app.prefs.update(|p| p.accent_choice = name.clone());
                // The theme re-applies immediately: an accent change that
                // waits for a restart looks like the control is broken.
                crate::window::apply_theme(&app);
            });
        }
        accent_grid.append(&button);
    }
    page.append(&accent_grid);

    // ---- reading ---------------------------------------------------------
    section(page, "Reading");
    let scale_row = row("Reader text scale");
    let scale = gtk::SpinButton::with_range(0.75, 1.5, 0.05);
    scale.set_digits(2);
    scale.set_value(prefs.reader_text_scale);
    scale.set_hexpand(true);
    scale_row.append(&scale);
    page.append(&scale_row);

    // The spacing knobs. The reader's line height and paragraph gap, and the
    // note preview's own pair — the preview and the book reader are different
    // surfaces with different defaults, and one pair of numbers cannot serve
    // both.
    let reader_lh_row = row("Book line height");
    let reader_lh = gtk::SpinButton::with_range(1.0, 2.2, 0.05);
    reader_lh.set_digits(2);
    reader_lh.set_value(prefs.reader_line_height);
    reader_lh.set_hexpand(true);
    reader_lh_row.append(&reader_lh);
    page.append(&reader_lh_row);

    let reader_gap_row = row("Book paragraph gap");
    let reader_gap = gtk::SpinButton::with_range(0.0, 28.0, 1.0);
    reader_gap.set_digits(0);
    reader_gap.set_value(prefs.reader_paragraph_spacing);
    reader_gap.set_hexpand(true);
    reader_gap_row.append(&reader_gap);
    page.append(&reader_gap_row);

    let note_lh_row = row("Note line height");
    let note_lh = gtk::SpinButton::with_range(1.0, 2.2, 0.05);
    note_lh.set_digits(2);
    note_lh.set_value(prefs.note_line_height);
    note_lh.set_hexpand(true);
    note_lh_row.append(&note_lh);
    page.append(&note_lh_row);

    let note_gap_row = row("Note paragraph gap");
    let note_gap = gtk::SpinButton::with_range(0.0, 28.0, 1.0);
    note_gap.set_digits(0);
    note_gap.set_value(prefs.note_paragraph_spacing);
    note_gap.set_hexpand(true);
    note_gap_row.append(&note_gap);
    page.append(&note_gap_row);

    let render_row = toggle_row("Render markdown in the editor", prefs.render_markdown);
    page.append(&render_row.0);
    let readalong_row = toggle_row("Highlight sentences while speaking", prefs.read_along_enabled);
    page.append(&readalong_row.0);

    // ---- persistence -----------------------------------------------------
    {
        let app = app.clone();
        appearance.connect_selected_notify(move |dd| {
            // FIX (eink-theme, 2026-11-10): Warm (index 3) used to fall into
            // the `_` arm, so picking Warm wrote "system" — the dropdown
            // predated the Warm appearance and the persistence side never
            // grew the arm. E-Ink rides the same table at index 4.
            let value = match dd.selected() {
                1 => "light",
                2 => "dark",
                3 => "warm",
                4 => "eink",
                _ => "system",
            };
            app.prefs.update(|p| p.set_appearance(
                sn_store::model::Appearance::from_pref(value),
            ));
            crate::window::apply_theme(&app);
        });
    }
    {
        let app = app.clone();
        scale.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(|p| p.reader_text_scale = value);
            //  the original stored the pref and stopped — but the scale
            // reaches the screen only through the stylesheet's md_* em
            // values, so without a re-apply the knob did nothing until
            // another reading knob (or a restart) happened to rebuild the
            // sheet. Coalesced for the same reason as the spacing knobs.
            crate::theme::request_apply(&app);
        });
    }
    // The spacing knobs are part of the stylesheet, so a change is a theme
    // re-apply: the same call the appearance and accent changes make. Without
    // it the numbers would be stored and nothing on screen would move.
    //  these go through `request_apply`, not `apply_theme`: the spin
    // steppers fire `value_changed` once per tick, and each re-apply forces
    // a full-window restyle — with a large note's preview open that measured
    // ~5.6 s of blocked main loop per tick, so holding `+` queued one
    // multi-second restyle per click. Coalescing to one apply per main-loop
    // idle keeps the last value and pays the restyle once.
    {
        let app = app.clone();
        reader_lh.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(|p| p.reader_line_height = value);
            crate::theme::request_apply(&app);
        });
    }
    {
        let app = app.clone();
        reader_gap.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(|p| p.reader_paragraph_spacing = value);
            crate::theme::request_apply(&app);
        });
    }
    {
        let app = app.clone();
        note_lh.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(|p| p.note_line_height = value);
            crate::theme::request_apply(&app);
        });
    }
    {
        let app = app.clone();
        note_gap.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(|p| p.note_paragraph_spacing = value);
            crate::theme::request_apply(&app);
        });
    }
    wire_toggle(&app, render_row.1, |p, v| p.render_markdown = v);
    wire_toggle(&app, readalong_row.1, |p, v| p.read_along_enabled = v);
}
