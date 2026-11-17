//! The GTK4 theme: one CSS string for light and dark, driven by the accent.
//!
//! This replaces what libadwaita would give us, because libadwaita is not
//! installed on this box. The result is deliberately hand-rolled: the accent
//! colour is the app's own, applied consistently to selection, focus, checks
//! and the progress bar, which is more than a stock theme does.
//!
//! Colour rules from the Swift port's theme work, kept because they were paid
//! for: **override every named colour, not some.** A partial override leaves
//! dark panels under light text — the "black on black" bug.

/// The twelve accents, as CSS hex. Order matches `sn_store::model::ACCENTS`.
///
///  ACCENTS is thirteen entries — the phone apps' twelve plus this
/// app's terracotta (the Warm theme's own). The shipped binary's strings
/// carry the thirteenth hex (`#d97757`) right after grey, and a user whose
/// pref says terracotta panicked the app here (index 12 out of bounds) the
/// moment the settings sheet built its stylesheet.
pub const ACCENT_HEX: [&str; 13] = [
    "#3584e4", // blue
    "#9141ac", // purple
    "#e56aa2", // pink
    "#e62d42", // red
    "#ff7800", // orange
    "#e5a50a", // yellow
    "#2ec27e", // green
    "#0f9d9a", // teal
    "#00a3c4", // cyan
    "#5e5c64", // indigo
    "#986a44", // brown
    "#77767b", // grey
    "#d97757", // terracotta
];

pub fn accent_hex(name: &str) -> &'static str {
    let idx = sn_store::model::ACCENTS
        .iter()
        .position(|a| *a == name)
        .unwrap_or(0);
    ACCENT_HEX[idx]
}

/// A palette: surfaces and ink for one colour scheme.
pub struct Palette {
    pub window: &'static str,
    pub surface: &'static str,
    pub card: &'static str,
    pub ink: &'static str,
    pub ink_dim: &'static str,
    pub border: &'static str,
    pub hover: &'static str,
    pub selected: &'static str,
}

pub const LIGHT: Palette = Palette {
    window: "#faf8f5",
    surface: "#f3f0eb",
    card: "#ffffff",
    ink: "#252428",
    ink_dim: "#77716c",
    border: "#e0dad3",
    hover: "rgba(0,0,0,0.05)",
    selected: "rgba(0,0,0,0.09)",
};

pub const DARK: Palette = Palette {
    window: "#242428",
    surface: "#1d1d20",
    card: "#2e2e33",
    ink: "#f6f5f4",
    ink_dim: "#9a9590",
    border: "#3a3a40",
    hover: "rgba(255,255,255,0.06)",
    selected: "rgba(255,255,255,0.10)",
};

/// The Warm scheme: the Anthropic-style paper theme the prefs name as this
/// app's own (2026-10-31). Two anchors ground the values: the store's own
/// doc comment ("`Warm` is this app's own — the Anthropic-style paper
/// theme", `sn-store/src/model.rs`) and the terracotta accent `#d97757`
/// ("the Warm theme's own"). Parchment window, ivory card, warm near-black
/// ink — the paper family those two facts name.
pub const WARM: Palette = Palette {
    window: "#f0eee5",
    surface: "#e8e4da",
    card: "#faf9f5",
    ink: "#26241f",
    ink_dim: "#7d7668",
    border: "#dbd7cc",
    hover: "rgba(60,50,30,0.05)",
    selected: "rgba(60,50,30,0.09)",
};

/// The E-Ink scheme: paper white, black ink, square edges, hairline rules.
///
///  authored 2026-11-10 (user ask; branch `eink-theme`). An e-ink panel
/// renders grays, not colour, and dithers soft shadows, gradients and glows
/// into noise — so this palette is monochrome by definition and the accent
/// pref does not reach it (`stylesheet_for` pins the accent to black). The
/// remaining E-Ink rules — radii zeroed, shadows and animations off,
/// selection by inversion — live in `EINK_EXTRA`, appended after the shared
/// sheet only for this scheme.
pub const EINK: Palette = Palette {
    window: "#ffffff",
    surface: "#f2f2f2",
    card: "#ffffff",
    ink: "#000000",
    ink_dim: "#444444",
    border: "#1a1a1a",
    hover: "#ececec",
    selected: "#000000",
};

/// The stylesheet for a scheme and accent.
///
/// The line-spacing and paragraph-gap values come from prefs, not from the
/// constants the stylesheet used to carry: they are the knobs the Reading
/// section of the settings dialog turns, and a stylesheet rebuilt on every
/// change is how a pref becomes visible without a restart. The defaults here
/// are the values theme.css shipped with, so an example or a test that has no
/// prefs gets exactly the old look.
pub fn stylesheet(dark: bool, accent: &str) -> String {
    stylesheet_with(if dark { &DARK } else { &LIGHT }, accent, 1.55, 10.0, 1.55, 10.0, 1.0)
}

/// The stylesheet with the reading metrics spelled out.
///
/// `preview_scale` multiplies the markdown preview's font sizes (the iOS
/// `readerTextScale` knob, under Appearance → "Preview text size"). It is
/// applied in Rust, as concrete `em` values, because a `calc()` in the CSS
/// would depend on GTK's support for it; these values are plain numbers the
/// renderer cannot refuse.
pub fn stylesheet_with(
    p: &Palette,
    accent: &str,
    reader_line_height: f64,
    reader_gap: f64,
    note_line_height: f64,
    note_gap: f64,
    preview_scale: f64,
) -> String {
    stylesheet_with_hex(
        p,
        accent_hex(accent),
        reader_line_height,
        reader_gap,
        note_line_height,
        note_gap,
        preview_scale,
    )
}

/// The stylesheet with the accent as a resolved hex.
///
///  split out of `stylesheet_with` 2026-11-10 (eink-theme) so the E-Ink
/// scheme can pin the accent to black without going through `accent_hex`,
/// which only resolves names in `ACCENTS`. E-Ink is monochrome by
/// definition — the accent pref does not reach its sheet.
pub fn stylesheet_with_hex(
    p: &Palette,
    a: &str,
    reader_line_height: f64,
    reader_gap: f64,
    note_line_height: f64,
    note_gap: f64,
    preview_scale: f64,
) -> String {
    let em = |base: f64| format!("{:.2}", (base * preview_scale * 100.0).round() / 100.0);
    format!(
        include_str!("theme.css"),
        window = p.window,
        surface = p.surface,
        card = p.card,
        ink = p.ink,
        ink_dim = p.ink_dim,
        border = p.border,
        hover = p.hover,
        selected = p.selected,
        a = a,
        reader_line_height = format!("{reader_line_height:.2}"),
        reader_gap = reader_gap as i32,
        note_line_height = format!("{note_line_height:.2}"),
        note_gap = note_gap as i32,
        md_body = em(1.0),
        md_h1 = em(1.7),
        md_h2 = em(1.35),
        md_h3 = em(1.12),
        md_h4 = em(1.0),
        md_small = em(0.7),
        md_raw = em(0.85),
        //  the css template's {serif} placeholder; the original css doc
        // comment quotes this exact list ("Source Serif 4" and Georgia first,
        // DejaVu Serif next, the generic `serif` last). The final theme.rs
        // format!-arg line carrying it did not survive.
        serif = SERIF,
    )
}

/// The display face: a serif for headings and titles.
// The css template's {serif} placeholder; the list is quoted verbatim from
// the css's own doc comment.
const SERIF: &str = "\"Source Serif 4\", Georgia, \"DejaVu Serif\", serif";

/// The appearance schemes the prefs name, resolved for the stylesheet.
//  authored from the call sites (window.rs apply_theme maps
// `sn_store::model::Appearance::{Light,Dark,Warm,System}` onto
// `theme::Scheme::{Light,Dark,Warm}` and passes it to `stylesheet_for`);
// the final theme.rs region carrying them did not survive. Warm's dedicated
// palette is lost with it; Warm currently renders on the light palette.
// EInk added 2026-11-10 (eink-theme, user ask).
#[derive(Clone, Copy)]
pub enum Scheme {
    Light,
    Dark,
    Warm,
    EInk,
}

impl Scheme {
    fn code(self) -> u8 {
        match self {
            Scheme::Light => 0,
            Scheme::Dark => 1,
            Scheme::Warm => 2,
            Scheme::EInk => 3,
        }
    }

    fn from_code(code: u8) -> Option<Scheme> {
        match code {
            0 => Some(Scheme::Light),
            1 => Some(Scheme::Dark),
            2 => Some(Scheme::Warm),
            3 => Some(Scheme::EInk),
            _ => None,
        }
    }
}

/// The scheme the sheet was last built for, written by `apply_theme`.
///
/// One bit of theme state a widget's own drawing needs: GTK's CSS
/// `border-radius` rounds a widget's background, not a texture the widget
/// draws itself, so the note image's square-edges request reaches its Rust
/// clip through `image_corner_radius` rather than the stylesheet.
static ACTIVE_SCHEME: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The corner radius a note image is clipped to under the active scheme.
///
///  authored 2026-11-10 (eink-theme). The 8 px round is the value
/// `aspect_image.rs` carried (the same 8 the `.md-image` CSS uses); E-Ink
/// squares it. Deliberately nothing else about the image path changes here —
/// no filter, no border, no background: a CSS filter is how an e-ink theme
/// breaks a picture (dithered halftones, wrong alpha), so images keep their
/// own colours and only the corners follow the scheme.
pub fn image_corner_radius() -> f32 {
    match Scheme::from_code(ACTIVE_SCHEME.load(std::sync::atomic::Ordering::Relaxed)) {
        Some(Scheme::EInk) => 0.0,
        _ => 8.0,
    }
}

/// Record the scheme the sheet being applied was built for. Called by
/// `apply_theme` just before the provider lands, so a redraw that follows
/// the swap re-snapshots widgets that read this.
pub fn set_active_scheme(scheme: Scheme) {
    ACTIVE_SCHEME.store(scheme.code(), std::sync::atomic::Ordering::Relaxed);
}

/// The E-Ink override sheet, appended after the shared template's output —
/// equal-specificity rules that come later in the sheet win the cascade, so
/// these restate (rather than re-theme) the shared sheet's rounded,
/// shadowed, gradient-filled and animated paint.
///
///  authored 2026-11-10 (eink-theme, user ask: "minimalist + square
/// edges + high contrast, no other GUI features, no animations"). Plain CSS
/// on purpose: it is appended, not format!-ed, so its braces need no
/// escaping and the shared template stays untouched. The palette does most
/// of the work (window #ffffff, ink #000000, border #1a1a1a, accent pinned
/// black); this block carries what a palette cannot say — radii, shadows,
/// motion, and the inversion that stands in for selection colour.
const EINK_EXTRA: &str = r#"/* --- E-Ink overrides ------------------------------------------------------
   Appended after the shared sheet: paper white, black ink, square edges,
   hairline rules, selection by inversion — and nothing that moves, glows,
   or casts a shadow, because an e-ink panel dithers all three into noise.
   Images are squared, never filtered. */

/* Square edges: every radius the shared sheet draws, restated at zero. */
.sidebar button.flat,
.notes-list > row,
.toolbar button.flat,
.pane-rail,
.engine-picker dropdown,
entry, searchentry,
checkbutton check,
button,
.book-card,
.book-card .book-cover,
.book-cover-placeholder,
progressbar, progressbar trough, progressbar progress,
.pdf-page, .pdf-page picture, .pdf-tools button.flat,
.md-code, .md-image, .md-image-missing, .md-image-loading,
.md-alert,
.image-viewer-bar button,
.format-bar button.flat,
.slash-menu, .slash-menu > row,
.toc-menu, .toc-menu > row,
picture,
.abook-cover,
.abook-chapter-bar trough, .abook-chapter-bar highlight, .abook-chapter-bar slider,
.transport-progress trough, .transport-progress highlight, .transport-progress slider,
.abook-button, .abook-play,
.settings-nav button.flat,
.accent-swatch,
.pomodoro-card, .pomodoro-fill, .pomodoro-bar trough, .pomodoro-bar progress,
.pomodoro-phase-pill, .pomodoro-controls button, .pomodoro-segmented,
.pomodoro-segmented button.pomodoro-segment, .pomodoro-popover,
.gentle-banner, .banner-action, .banner-dismiss,
.floating-transport,
toolbar-scroll scrollbar slider, format-bar-scroll scrollbar slider,
scrollbar slider {
  border-radius: 0;
}

/* Plain buttons read as outlined rectangles: white fill, black rule. The
   flat buttons keep their own (transparent, hover-filled) paint — their
   selectors outrank this one. */
button {
  background: #ffffff;
  color: #000000;
  border: 1px solid #1a1a1a;
}

/* No depth: a soft shadow is noise on e-ink. */
.book-card .book-cover,
.pdf-page,
.abook-cover,
.pomodoro-popover,
.gentle-banner,
.floating-transport {
  box-shadow: none;
}

/* No motion: the live-progress glow is the app's one CSS animation. The
   fill is a solid black bar and stays still. */
progressbar.live progress,
.abook-chapter-bar.live highlight,
.transport-progress.live highlight {
  background: #000000;
  animation: none;
}

/* No hover brighten filters — flat colour only. */
button.suggested-action:hover,
.abook-play:hover,
.pomodoro-controls button.pomodoro-play:hover,
.pomodoro-controls button.destructive-action:hover,
progressbar.scrubbing progress,
.abook-chapter-bar.scrubbing highlight,
.transport-progress.scrubbing highlight {
  filter: none;
}
button.suggested-action,
.pomodoro-controls button.pomodoro-play,
.pomodoro-controls button.destructive-action,
button.destructive-action {
  background: #000000;
  color: #ffffff;
  border: none;
}
.pomodoro-controls button.pomodoro-claim {
  color: #000000;
}

/* Selection by inversion — the e-ink highlight: black fill, white text. */
.notes-list > row:selected {
  background: #000000;
  border-bottom-color: #000000;
  box-shadow: none;
}
.notes-list > row:selected .row-title { color: #ffffff; }
.notes-list > row:selected .row-preview,
.notes-list > row:selected .row-meta { color: #d0d0d0; }
.sidebar button.flat.active,
.settings-nav button.flat.active,
.toolbar button.flat.active {
  background: #000000;
  color: #ffffff;
  box-shadow: none;
}
.sidebar button.flat.active label,
.settings-nav button.flat.active label { color: #ffffff; }
.slash-menu > row:selected,
.slash-menu > row:selected .slash-label { color: #ffffff; }
.slash-menu > row:selected .slash-hint { color: #d0d0d0; }
.toc-menu > row.active { background: #000000; }
.toc-menu > row.active label { color: #ffffff; }
togglebutton:checked { background: #000000; color: #ffffff; }
list > row:selected,
listview > row:selected,
gridview > row:selected { background: #000000; }
list > row:selected label,
listview > row:selected label,
gridview > row:selected label { color: #ffffff; }

/* Text selection inverts too: the editor, the title entry, every entry. */
textview text selection,
textview > text > selection,
text selection {
  background-color: #000000;
  color: #ffffff;
}

/* The notes list reads as an index: one hairline between rows. */
.notes-list > row {
  border-bottom: 1px solid #c9c9c9;
}

/* Markdown structure, grayscale: alerts lose their kind colours, the
   pomodoro phases lose theirs. */
.md-alert,
.md-alert-note, .md-alert-tip, .md-alert-important,
.md-alert-warning, .md-alert-caution {
  border-left-color: #000000;
  background: #ffffff;
}
.pomodoro-bar.prep progress,
.pomodoro-bar.focus progress,
.pomodoro-bar.rest progress {
  background: #000000;
}
.pomodoro-phase-pill.prep,
.pomodoro-phase-pill.focus,
.pomodoro-phase-pill.rest {
  color: #000000;
  background: #ececec;
}
.gentle-banner.prep, .gentle-banner.focus,
.gentle-banner.rest, .gentle-banner.gold {
  border-left-color: #000000;
}
.gentle-banner.prep .banner-icon, .gentle-banner.focus .banner-icon,
.gentle-banner.rest .banner-icon, .gentle-banner.gold .banner-icon {
  color: #000000;
}
/* The reader's contents sidebar (the TOC column): square-edged like its
   neighbours, and the open chapter selected by inversion, the way the old
   popover's rows were. */
.toc-sidebar {
  border-right: 1px solid #000000;
}
.toc-sidebar button.toc-row,
.toc-sidebar button.toc-group {
  border-radius: 0;
}
.toc-sidebar button.toc-row.active,
.toc-sidebar button.toc-group.active {
  background: #000000;
}
.toc-sidebar button.toc-row.active label,
.toc-sidebar button.toc-group.active label {
  color: #ffffff;
}
"#;

/// The stylesheet for a named scheme and accent, with the reading metrics.
//  signature from window.rs apply_theme's call site; the body routes
// through `stylesheet_with`. Warm renders on the light palette; the `WARM`
// palette slots in here (2026-10-31).
// EInk routes through the `EINK` palette with the accent pinned
// black and `EINK_EXTRA` appended (2026-11-10).
pub fn stylesheet_for(
    scheme: Scheme,
    accent: &str,
    reader_line_height: f64,
    reader_paragraph_spacing: f64,
    note_line_height: f64,
    note_paragraph_spacing: f64,
    reader_text_scale: f64,
) -> String {
    let (palette, a) = match scheme {
        Scheme::Light => (&LIGHT, accent_hex(accent)),
        Scheme::Dark => (&DARK, accent_hex(accent)),
        //  the authored paper palette (see its doc comment above).
        Scheme::Warm => (&WARM, accent_hex(accent)),
        // E-Ink is monochrome: whatever accent the prefs carry, the sheet
        // draws black.
        Scheme::EInk => (&EINK, "#000000"),
    };
    let sheet = stylesheet_with_hex(
        palette,
        a,
        reader_line_height,
        reader_paragraph_spacing,
        note_line_height,
        note_paragraph_spacing,
        reader_text_scale,
    );
    match scheme {
        // Appended, not merged: the existing schemes' sheets stay
        // byte-identical, so nothing about Light/Dark/Warm can shift.
        Scheme::EInk => format!("{}\n{}", sheet, EINK_EXTRA),
        _ => sheet,
    }
}

/// Apply the theme once, ~a quarter-second after the last request lands.
///
//  authored for the reading knobs. The settings' spin steppers fire
// `value_changed` once per tick, and every re-apply forces a full-window
// restyle — with a large note's preview open one restyle measured multiple
// seconds of blocked main loop, so holding `+` queued one multi-second
// restyle per click and the reading knobs "grew slower the more they were
// used". This debounces: each request restarts a short timer, so a burst of
// ticks pays exactly one restyle, for the last prefs snapshot. A quarter
// second is under the ~300 ms the outside driver already measures per
// interaction, so a single deliberate click still feels immediate.
pub fn request_apply(app: &std::rc::Rc<crate::app::App>) {
    thread_local! {
        static TIMER: std::cell::RefCell<Option<gtk4::glib::SourceId>> =
            const { std::cell::RefCell::new(None) };
    }
    let app = app.clone();
    let id = gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
        TIMER.with(|slot| *slot.borrow_mut() = None);
        crate::window::apply_theme(&app);
    });
    TIMER.with(|slot| {
        if let Some(old) = slot.borrow_mut().replace(id) {
            old.remove();
        }
    });
}

/// Detect the system colour preference.
///
/// gsettings is the honest signal on a GNOME-family desktop; anything else
/// defaults to light. The Swift port used GTK's theme-name heuristic, which
/// misreads a dark theme named "Solarized".
pub fn system_prefers_dark() -> bool {
    //  apply_theme runs on every spin tick, and this spawn landed in
    // that path — a subprocess per reading-knob change under the System
    // appearance. A short cache keeps the honest signal (it re-reads when
    // the user actually changes the scheme) without per-tick spawns.
    static CACHE: std::sync::Mutex<Option<(std::time::Instant, bool)>> =
        std::sync::Mutex::new(None);
    if let Ok(guard) = CACHE.lock() {
        if let Some((at, dark)) = *guard {
            if at.elapsed() < std::time::Duration::from_secs(5) {
                return dark;
            }
        }
    }
    let dark = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("dark"))
        .unwrap_or(false);
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((std::time::Instant::now(), dark));
    }
    dark
}
