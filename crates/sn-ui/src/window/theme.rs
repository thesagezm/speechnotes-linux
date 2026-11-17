//! Theme application for the main window (split out of `window.rs`,
//! Round 4, pure code-motion).

use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gdk;

use crate::app::App;
use crate::theme;

/// Apply the theme to the default display.
///
/// The reading metrics ride in the stylesheet, so a change to the line height
/// or the paragraph gap is a re-apply of the theme — the same call the
/// appearance and accent changes already make. That is why the settings
/// dialog's Reading section needs no separate wiring: the knobs are prefs,
/// the prefs are in the stylesheet, and the stylesheet is reloaded.
pub fn apply_theme(app: &Rc<App>) {
    let prefs = app.prefs.get();
    let scheme = match prefs.appearance() {
        sn_store::model::Appearance::Light => theme::Scheme::Light,
        sn_store::model::Appearance::Dark => theme::Scheme::Dark,
        sn_store::model::Appearance::Warm => theme::Scheme::Warm,
        sn_store::model::Appearance::EInk => theme::Scheme::EInk,
        sn_store::model::Appearance::System => {
            if theme::system_prefers_dark() {
                theme::Scheme::Dark
            } else {
                theme::Scheme::Light
            }
        }
    };
    // Widgets that draw their own paint (the note image's corner clip) read
    // the scheme from here, not from the CSS — record it before the provider
    // swap so the redraw it triggers re-snapshots with the fresh answer.
    theme::set_active_scheme(scheme);
    let css = theme::stylesheet_for(
        scheme,
        prefs.accent().name(),
        prefs.reader_line_height,
        prefs.reader_paragraph_spacing,
        prefs.note_line_height,
        prefs.note_paragraph_spacing,
        prefs.reader_text_scale,
    );
    //  the original code created a fresh CssProvider on every call and
    // never removed it — every spin tick registered one more provider for
    // the display, so each restyle consulted a longer list and the reading
    // knobs grew slower the more they were used. This swaps exactly one
    // provider per apply: the previous sheet is removed before the new one
    // is added, so the display's provider list stays one long and the
    // remove/add transition is what triggers the restyle.
    thread_local! {
        static SHEET: std::cell::RefCell<Option<gtk::CssProvider>> =
            const { std::cell::RefCell::new(None) };
    }
    let previous = SHEET.with(|slot| slot.borrow_mut().take());
    if let (Some(previous), Some(display)) = (previous, gdk::Display::default()) {
        gtk::style_context_remove_provider_for_display(&display, &previous);
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    SHEET.with(|slot| *slot.borrow_mut() = Some(provider));
    register_swatches();
}

/// The accent swatches' paint, registered once for the display.
///
/// The Appearance page's thirteen swatches used to each build their own
/// CssProvider **on every settings open** — thirteen providers per open,
/// registered at APPLICATION priority and never removed. The rules are
/// constants (the swatch IS the data), so one provider carrying all
/// thirteen rules, added the first time the theme applies, is the same
/// paint with no growth and no per-open work. The rules outrank the sheet
/// by specificity (two classes vs one), exactly as the per-button
/// providers did.
pub(crate) fn register_swatches() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    let mut css = String::new();
    for (name, hex) in sn_store::model::ACCENTS
        .iter()
        .zip(theme::ACCENT_HEX.iter())
    {
        css.push_str(&format!(
            ".accent-swatch.accent-{name} {{ background: {hex}; }}\n"
        ));
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}