//! Split out of settings.rs 2026-10-31 (pure code-motion).
//!
//! 2026-11-12: the Language dropdown. A voice pack is a set of locales —
//! espeak's voice code is the language tag, phonon's catalog names
//! `en-us`/`es-es`/…, the twelve character voices are their own group — and
//! the user asked to pick a language and see only the voices that speak it.
//! The filter lives in ONE place (`sn_tts::manager::voices_for_language`);
//! this page and the playback rail both go through it, so the two lists
// cannot drift. The rail refreshes on a change from here too.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk::prelude::*;

use crate::app::App;
use super::helpers::*;

// ---------------------------------------------------------------------------
// Speech
// ---------------------------------------------------------------------------

pub(crate) fn speech_page(app: &Rc<App>, page: &gtk::Box) {
    let prefs = app.prefs.get();

    section(page, "Voice");
    let rate_row = row("Speaking rate");
    let rate = gtk::SpinButton::with_range(0.5, 2.0, 0.05);
    rate.set_digits(2);
    rate.set_value(prefs.rate_multiplier);
    rate.set_hexpand(true);
    rate_row.append(&rate);
    page.append(&rate_row);

    // Engine and voice pickers: the same widgets the player bar carries,
    // minus the live-session coupling — settings writes the pref; a running
    // session picks it up on its next start.
    let engine_row = row("Speech engine");
    let engines = sn_tts::manager::catalog();
    let mut engine_names: Vec<String> = engines
        .iter()
        .map(|e| e.engine.to_string())
        .collect();
    engine_names.push("espeak".to_string());
    let strings: Vec<&str> = engine_names.iter().map(String::as_str).collect();
    let engine = gtk::DropDown::from_strings(&strings);
    if let Some(i) = engine_names.iter().position(|e| *e == prefs.engine_kind) {
        engine.set_selected(i as u32);
    }
    engine.set_hexpand(true);
    engine_row.append(&engine);
    page.append(&engine_row);

    // The speech language. Rows: "All languages" (`""`) plus the languages
    // the engine's voices actually carry — a locale prefix (`en`), or the
    // literal characters group. An engine whose voices carry no locale
    // (supertonic's style voices, kokoro's bank) offers only All.
    let language_row = row("Language");
    let language_list = gtk4::StringList::new(&[]);
    let language =
        gtk4::DropDown::new(Some(language_list.clone()), None::<&gtk4::Expression>);
    language.set_hexpand(true);
    language_row.append(&language);
    page.append(&language_row);

    // The default voice for the engine above, filtered to the language.
    // The picker in the player bar can change the voice of a running
    // session; this is the voice a *new* session starts with, which is the
    // thing a setting is for.
    let voice_row = row("Default voice");
    let voice_list = gtk4::StringList::new(&[]);
    let voice = gtk4::DropDown::new(Some(voice_list.clone()), None::<&gtk4::Expression>);
    voice.set_hexpand(true);
    voice_row.append(&voice);
    page.append(&voice_row);

    // The codes behind the two dropdowns' display rows — a selection maps
    // back through them to what the prefs store keeps. Refilled on every
    // rebuild, always under the guard below.
    let lang_codes = Rc::new(RefCell::new(Vec::<String>::new()));
    let voice_codes = Rc::new(RefCell::new(Vec::<String>::new()));
    // The engine the two code lists were built for. The playback rail (and
    // any second window) can change the pref engine while this page is
    // open, and this page has no signal for that: without the record, a
    // voice click after a rail-side switch fired the handler with the NEW
    // pref engine but the OLD engine's codes — the write that put a phonon
    // voice id under the espeak key and beeped every chunk for a day.
    let built_engine = Rc::new(RefCell::new(prefs.engine_kind.clone()));
    // The picker's guard rule, ported: a rebuild's programmatic
    // `set_selected` fires `notify::selected` like a user's choice does,
    // and a rebuild must never count as one — above all it must never
    // rewrite the pref *voice* onto whatever row happens to be showing.
    let lang_guard = Rc::new(Cell::new(false));
    let voice_guard = Rc::new(Cell::new(false));

    fn rebuild_language_list(
        app: &Rc<App>,
        dropdown: &gtk4::DropDown,
        codes_out: &Rc<RefCell<Vec<String>>>,
        guard: &Rc<Cell<bool>>,
    ) {
        let model = dropdown
            .model()
            .and_then(|m| m.downcast::<gtk4::StringList>().ok())
            .expect("the language dropdown's model is the StringList it was built with");
        guard.set(true);
        model.splice(0, model.n_items(), &[]);
        let mut rows = vec!["All languages".to_string()];
        let mut codes = vec![String::new()];
        for (code, display) in speech_languages(&app.prefs.get().engine_kind) {
            rows.push(display);
            codes.push(code);
        }
        for r in &rows {
            model.append(r);
        }
        *codes_out.borrow_mut() = codes;
        // The pref, when it is one of the rows, stays selected. A pref
        // naming a language this engine does not carry falls back to All —
        // shown, not written: the guard is still up.
        let pref = app.prefs.get().speech_language.clone();
        let selected = codes_out
            .borrow()
            .iter()
            .position(|c| *c == pref)
            .unwrap_or(0);
        dropdown.set_selected(selected as u32);
        guard.set(false);
    }

    fn rebuild_voice_list(
        app: &Rc<App>,
        dropdown: &gtk4::DropDown,
        codes_out: &Rc<RefCell<Vec<String>>>,
        guard: &Rc<Cell<bool>>,
    ) {
        let model = dropdown
            .model()
            .and_then(|m| m.downcast::<gtk4::StringList>().ok())
            .expect("the voice dropdown's model is the StringList it was built with");
        guard.set(true);
        model.splice(0, model.n_items(), &[]);
        let prefs = app.prefs.get();
        let engine = prefs.engine_kind.clone();
        let lang = prefs.speech_language.clone();
        let voices = sn_tts::manager::voices_for_language(&engine, &lang);
        let mut rows: Vec<String> = if voices.is_empty() {
            // An engine (or language, or engine not installed yet) with
            // nothing to enumerate still gets a working row, so the
            // dropdown is never a blank hole.
            vec![prefs.voice_for_engine(&engine).clone()]
        } else {
            voices.iter().map(|v| v.name.clone()).collect()
        };
        let codes: Vec<String> = if voices.is_empty() {
            vec![prefs.voice_for_engine(&engine).clone()]
        } else {
            voices.iter().map(|v| v.code.clone()).collect()
        };
        for r in &rows {
            model.append(r);
        }
        rows.clear();
        *codes_out.borrow_mut() = codes.clone();
        // The pref voice, when the filtered list still shows it, stays
        // selected. When the language hides it, the first row shows —
        // and the guard keeps the hidden pref intact.
        let pref = prefs.voice_for_engine(&engine);
        let selected = codes
            .iter()
            .position(|c| *c == pref)
            .unwrap_or(0);
        dropdown.set_selected(selected as u32);
        guard.set(false);
    }

    rebuild_language_list(app, &language, &lang_codes, &lang_guard);
    rebuild_voice_list(app, &voice, &voice_codes, &voice_guard);

    section(page, "Playback");
    // The audiobook's own speed and sleep timer. Both were in the store and
    // reachable only while a book was open; a default belongs in settings.
    let audio_rate_row = row("Audiobook speed");
    let audio_rate = gtk::SpinButton::with_range(0.5, 3.0, 0.05);
    audio_rate.set_digits(2);
    audio_rate.set_value(prefs.audio_rate);
    audio_rate.set_hexpand(true);
    audio_rate_row.append(&audio_rate);
    page.append(&audio_rate_row);

    let sleep_row = row("Sleep timer (minutes, 0 = off)");
    let sleep = gtk::SpinButton::with_range(0.0, 120.0, 1.0);
    sleep.set_digits(0);
    sleep.set_value(prefs.sleep_timer_minutes as f64);
    sleep.set_hexpand(true);
    sleep_row.append(&sleep);
    page.append(&sleep_row);

    // ---- persistence -----------------------------------------------------

    // The handlers' shared pre-flight: if the pref engine moved since the
    // lists were filled (the playback rail's doing — this page gets no
    // signal for it), refill both lists for the engine prefs actually
    // holds and absorb the event that noticed it. The stale-list click
    // this guards against is the one that wrote a phonon voice id under
    // the espeak key (2026-11-13, the beeps-everywhere bug).
    let resync_if_stale = {
        let app = app.clone();
        let language_dd = language.clone();
        let lang_codes = lang_codes.clone();
        let lang_guard = lang_guard.clone();
        let voice_dd = voice.clone();
        let voice_codes = voice_codes.clone();
        let voice_guard = voice_guard.clone();
        let built_engine = built_engine.clone();
        move || {
            let current = app.prefs.get().engine_kind.clone();
            if *built_engine.borrow() == current {
                return false;
            }
            rebuild_language_list(&app, &language_dd, &lang_codes, &lang_guard);
            rebuild_voice_list(&app, &voice_dd, &voice_codes, &voice_guard);
            *built_engine.borrow_mut() = current;
            true
        }
    };

    {
        let app = app.clone();
        rate.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(|p| p.rate_multiplier = value);
        });
    }
    {
        let app = app.clone();
        let language_dd = language.clone();
        let lang_codes = lang_codes.clone();
        let lang_guard = lang_guard.clone();
        let voice_dd = voice.clone();
        let voice_codes = voice_codes.clone();
        let voice_guard = voice_guard.clone();
        let built_engine = built_engine.clone();
        engine.connect_selected_notify(move |dd| {
            let name = engine_names
                .get(dd.selected() as usize)
                .cloned()
                .unwrap_or_else(|| "espeak".to_string());
            app.prefs.update(|p| p.engine_kind = name);
            *built_engine.borrow_mut() = app.prefs.get().engine_kind.clone();
            // A new engine carries its own locales: the language rows come
            // from its voices, so they rebuild first and the voice list
            // rides after. Neither rebuild writes a pref (the guards).
            rebuild_language_list(&app, &language_dd, &lang_codes, &lang_guard);
            rebuild_voice_list(&app, &voice_dd, &voice_codes, &voice_guard);
            refresh_playback_rail(&app);
        });
    }
    {
        let app = app.clone();
        let lang_codes = lang_codes.clone();
        let lang_guard = lang_guard.clone();
        let voice_dd = voice.clone();
        let voice_codes = voice_codes.clone();
        let voice_guard = voice_guard.clone();
        let resync_if_stale = resync_if_stale.clone();
        language.connect_selected_notify(move |dd| {
            if lang_guard.get() || resync_if_stale() {
                return;
            }
            let code = {
                let codes = lang_codes.borrow();
                codes.get(dd.selected() as usize).cloned()
            };
            let Some(code) = code else {
                return;
            };
            app.prefs.update(|p| p.speech_language = code);
            // The rail's voice list filters through the same pref: refresh
            // it, so the bar shows the language the user just chose.
            rebuild_voice_list(&app, &voice_dd, &voice_codes, &voice_guard);
            refresh_playback_rail(&app);
        });
    }
    {
        let app = app.clone();
        let voice_codes = voice_codes.clone();
        let voice_guard = voice_guard.clone();
        let resync_if_stale = resync_if_stale.clone();
        voice.connect_selected_notify(move |dd| {
            if voice_guard.get() || resync_if_stale() {
                return;
            }
            let code = {
                let codes = voice_codes.borrow();
                codes.get(dd.selected() as usize).cloned()
            };
            let Some(code) = code else {
                return;
            };
            let engine = app.prefs.get().engine_kind.clone();
            // The code must belong to the engine it is filed under — the
            // last line of defence against a stale list. A kokoro bank
            // code under `espeak` (or any cross-engine code) made every
            // chunk fail at synthesis: beeps, with the error only in the
            // log.
            let known = sn_tts::manager::voices_for_language(&engine, "")
                .iter()
                .any(|v| v.code == code);
            if !known {
                tracing::warn!(
                    engine = %engine,
                    voice = %code,
                    "voice click ignored: the code is not one of this engine's voices"
                );
                return;
            }
            app.prefs.update(|p| p.set_voice_for_engine(&engine, &code));
        });
    }
    {
        let app = app.clone();
        audio_rate.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(|p| p.audio_rate = value);
        });
    }
    {
        let app = app.clone();
        sleep.connect_value_changed(move |spin| {
            let value = spin.value();
            app.prefs.update(|p| p.sleep_timer_minutes = value.max(0.0) as u32);
        });
    }
}

/// The playback rail's voice list follows this page: the same pref, the
/// same filter, refreshed without treating the refill as a user choice
/// (the guard inside `picker::refresh`'s rebuilds is what makes that true).
fn refresh_playback_rail(app: &Rc<App>) {
    if let Some(w) = app.widgets.borrow().clone() {
        crate::picker::refresh(&w.picker, app);
    }
}

/// The languages the engine can speak, sorted, as (code, display) pairs —
/// the code being the pref value. A voice-pack engine offers the locales
/// its voices actually carry; supertonic offers every language its model
/// knows (its voices speak them all).
fn speech_languages(engine: &str) -> Vec<(String, String)> {
    sn_tts::manager::engine_languages(engine)
        .into_iter()
        .map(|code| {
            let display = language_display(&code);
            (code, display)
        })
        .collect()
}

/// The display name for a language code: the common ones and supertonic's
/// thirty-one, and the raw code when nothing better is known.
fn language_display(code: &str) -> String {
    let display = match code {
        "ar" => "Arabic",
        "bg" => "Bulgarian",
        "cs" => "Czech",
        "da" => "Danish",
        "de" => "German",
        "el" => "Greek",
        "en" => "English",
        "es" => "Spanish",
        "et" => "Estonian",
        "fi" => "Finnish",
        "fr" => "French",
        "hi" => "Hindi",
        "hr" => "Croatian",
        "hu" => "Hungarian",
        "id" => "Indonesian",
        "it" => "Italian",
        "ja" => "Japanese",
        "ko" => "Korean",
        "lt" => "Lithuanian",
        "lv" => "Latvian",
        "nl" => "Dutch",
        "pl" => "Polish",
        "pt" => "Portuguese",
        "ro" => "Romanian",
        "ru" => "Russian",
        "sk" => "Slovak",
        "sl" => "Slovenian",
        "sv" => "Swedish",
        "tr" => "Turkish",
        "uk" => "Ukrainian",
        "vi" => "Vietnamese",
        "characters" => "Characters",
        other => other,
    };
    display.to_string()
}
