//! The engine and voice pickers: prefs-driven dropdowns in the mini-player
//! bar, the UI half of Batch 6.
//!
//! The rules this module exists to keep:
//!
//! * **prefs is the only writer's truth.** The combo boxes *read*
//!   `engineKind` / `voicesByEngine` and write back through
//!   `PrefsStore::update` — never a shadow copy in a widget. A pref set by
//!   another pane (or hand-edited into `prefs.json`) shows up the next time
//!   the bar is built, and nothing here can drift from what `--speak` reads.
//! * **The list is what the box can do.** Engines are offered only when
//!   installed (`ModelManager::available_engines`), so the picker can never
//!   offer a choice whose model files are missing — the load would fail
//!   mid-playback, which is the one-attempt-then-beep failure the pipeline
//!   treats as a last resort.
//! * **A change while speaking restarts from the heard frontier**, exactly
//!   like the speed control: a chunk already inside the old engine finishes
//!   in the old voice, and the new one takes over at the sentence the
//!   listener is actually on. The restart is queued on the tick, never run
//!   inside a signal handler.
//! * **Programmatic refills are not user choices.** Every `set_selected`
//!   fires `notify::selected`, and so does every model splice and append —
//!   a `GtkSingleSelection` autoselects while it is refilled, and each of
//!   those moves reads as a click. So a refill runs with the guard up for
//!   its whole body: filling the lists at build time (or from a settings
//!   refresh) once stomped the pref engine to the first row (`espeak`,
//!   the "switching engines lands on espeak" bug) and wrote first-row
//!   voices over saved ones, all without the user touching anything.
//! * **A selection that names nothing changes nothing.** A rebuild in
//!   flight, or a list that is empty, must not overwrite a pref with a
//!   default: writing `""`/first-voice just because a dropdown moved would
//!   wedge the next launch onto a choice nobody made.

use gtk4::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

use crate::app::App;

/// Build the two dropdowns. Both read prefs at build time; the voice list is
/// rebuilt whenever the engine changes, because voice codes are per-engine
/// (a kokoro bank code handed to supertonic is a load error).
///
/// The pickers ride inline at the end of the transport bar — `row` is the
/// box the window appends there, not a bar of their own. The combos are
/// also kept in `Widgets`, so a later refresh (or a test) can rebuild the
/// lists without going through a module-level handle.
pub fn build(app: &Rc<App>) -> PickerWidgets {
    let row = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["engine-picker"])
        .build();

    let engine_label = gtk4::Label::builder().label("Voice:").build();
    let engine_list = gtk4::StringList::new(&[]);
    let engine_combo = gtk4::DropDown::new(Some(engine_list), None::<&gtk4::Expression>);
    engine_combo.set_tooltip_text(Some("Speech engine"));
    let voice_list = gtk4::StringList::new(&[]);
    let voice_combo = gtk4::DropDown::new(Some(voice_list), None::<&gtk4::Expression>);
    voice_combo.set_tooltip_text(Some("Voice"));
    // The dropdowns keep their natural width — the *engine* name is worth
    // reading ("supertonic" tells the user which voice they are hearing),
    // and the transport row's surplus scrolls rather than squeezing the
    // names into ellipses.
    row.append(&engine_label);
    row.append(&engine_combo);
    row.append(&voice_combo);

    let picker = PickerWidgets {
        row: row.clone(),
        engine: engine_combo.clone(),
        voice: voice_combo.clone(),
        suppress: Rc::new(Cell::new(false)),
    };
    picker.rebuild_engine_list(app);
    {
        let app = app.clone();
        let picker = picker.clone();
        engine_combo.connect_notify_local(Some("selected"), move |_, _| {
            picker.on_user_selection(&app, Selection::Engine);
        });
    }
    {
        let app = app.clone();
        let picker = picker.clone();
        voice_combo.connect_notify_local(Some("selected"), move |_, _| {
            picker.on_user_selection(&app, Selection::Voice);
        });
    }

    picker
}

/// The dropdown pair. Cloned into signal closures and kept in `Widgets` for
/// a rebuild.
#[derive(Clone)]
pub struct PickerWidgets {
    /// The inline box the two combos ride in, appended to the transport bar.
    pub row: gtk4::Box,
    pub engine: gtk4::DropDown,
    pub voice: gtk4::DropDown,
    /// True while a programmatic `set_selected` is in flight; the
    /// `notify::selected` handlers read it and stand down. `Rc<Cell>` —
    /// shared across clones on purpose: a `PickerWidgets` clone holds the
    /// same flag (a `Widgets` clone deep-copies a bare `Cell`, and a
    /// refill through the clone would then run unsuppressed on the
    /// build-time handlers, writing prefs as if the user had chosen).
    /// `Cell` because every touch is on the UI thread.
    suppress: Rc<Cell<bool>>,
}

/// Which dropdown a selection event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
    Engine,
    Voice,
}

impl PickerWidgets {
    fn engine_string(&self) -> Option<String> {
        let item = self.engine.selected_item()?;
        item.downcast_ref::<gtk4::StringObject>()
            .map(|s| s.string().to_string())
    }

    fn voice_string(&self) -> Option<String> {
        let item = self.voice.selected_item()?;
        item.downcast_ref::<gtk4::StringObject>()
            .map(|s| s.string().to_string())
    }

    /// One guarded programmatic selection: set the flag, move the selection,
    /// clear the flag. `notify::selected` fires synchronously inside
    /// `set_selected`, so a bare flag set around the call is enough — there
    /// is no window for a re-entrant user event to land inside.
    ///
    /// Called *inside* a rebuild (whose own guard is already up) the flag is
    /// left alone: clearing it mid-rebuild would expose the rest of the
    /// refill's signal traffic as user choices.
    fn select(&self, combo: &gtk4::DropDown, position: u32) {
        if self.suppress.get() {
            combo.set_selected(position);
            return;
        }
        self.suppress.set(true);
        combo.set_selected(position);
        self.suppress.set(false);
    }

    /// The `notify::selected` fan-in for both combos. A suppressed event is
    /// a rebuild's own `set_selected` and stops here.
    fn on_user_selection(&self, app: &Rc<App>, which: Selection) {
        if self.suppress.get() {
            return;
        }
        match which {
            Selection::Engine => self.engine_chosen(app),
            Selection::Voice => self.voice_chosen(app),
        }
    }

    /// Fill the engine list from what this box can run, selecting the pref.
    ///
    /// The guard is up for the WHOLE body, not only the final selection:
    /// splicing the StringList empties it and every append autoselects a
    /// row, and each of those moves fires `notify::selected` exactly like a
    /// user's click. With the guard down during the refill, the append of
    /// the first engine row ran `engine_chosen` on that row — and every
    /// rebuild stomped `engineKind` to the first entry (`espeak`) while the
    /// combo went on *showing* the engine the user had picked. That stomp
    /// was the "switching engines lands me on espeak" bug, and it is also
    /// how a voice picked for one engine came to be written under another
    /// engine's key (the settings page reads the pref engine its handlers
    /// fire with): beeps forever from an engine that was never chosen.
    fn rebuild_engine_list(&self, app: &Rc<App>) {
        self.suppress.set(true);
        let model = self
            .engine
            .model()
            .and_then(|m| m.downcast::<gtk4::StringList>().ok())
            .expect("the engine combo's model is the StringList it was built with");
        model.splice(0, model.n_items(), &[]);
        let engines = sn_tts::manager::ModelManager::available_engines();
        let prefs = app.prefs.get();
        let mut selected = 0u32;
        for (i, engine) in engines.iter().enumerate() {
            model.append(engine);
            if *engine == prefs.engine_kind {
                selected = i as u32;
            }
        }
        // A pref naming an unavailable engine (model files removed under the
        // app) falls back to the first entry; the *pref* is not rewritten —
        // the guard stopped this fill from counting as a choice, and
        // `start_session` reports a load failure rather than silently
        // migrating a choice the user made.
        self.select(&self.engine, selected);
        self.rebuild_voice_list(app);
        self.suppress.set(false);
    }

    /// Fill the voice list for the selected engine, selecting the pref.
    /// Guarded for the whole body, for the same reason its engine sibling
    /// is: the refill's own splice/append traffic must never read as a
    /// user's voice choice (a rebuild used to write the first row's voice
    /// over the engine's saved one).
    fn rebuild_voice_list(&self, app: &Rc<App>) {
        self.suppress.set(true);
        let Some(engine) = self.engine_string() else {
            self.suppress.set(false);
            return;
        };
        let model = self
            .voice
            .model()
            .and_then(|m| m.downcast::<gtk4::StringList>().ok())
            .expect("the voice combo's model is the StringList it was built with");
        model.splice(0, model.n_items(), &[]);
        // The speech-language pref filters here too — the ONE helper the
        // settings page uses, so the rail and settings can never offer two
        // different lists.
        let lang = app.prefs.get().speech_language.clone();
        let voices = sn_tts::manager::voices_for_language(&engine, &lang);
        let pref = app.prefs.get().voice_for_engine(&engine);
        let mut selected = 0u32;
        for (i, voice) in voices.iter().enumerate() {
            model.append(&voice.name);
            if voice.code == pref {
                selected = i as u32;
            }
        }
        if voices.is_empty() {
            // An engine with nothing to enumerate still gets a working row,
            // so the dropdown is never a blank hole; the code maps to ""
            // (default), and the guard means this fill writes nothing.
            model.append("(default)");
        }
        self.select(&self.voice, selected);
        self.suppress.set(false);
    }

    /// A user's engine choice: persist it, rebuild the voice list, and
    /// queue a restart of a live session at the heard frontier.
    fn engine_chosen(&self, app: &Rc<App>) {
        let Some(engine) = self.engine_string() else {
            return;
        };
        // Only a change writes: `notify::selected` also fires when the user
        // re-picks the row that was already selected.
        let current = app.prefs.get().engine_kind;
        if current != engine {
            app.prefs.update(|p| p.engine_kind = engine.clone());
        }
        // Rebuild even when the pref already matched: the row only *shows*
        // a voice list for the engine now on screen.
        self.rebuild_voice_list(app);
        restart_live_session(app);
    }

    /// A user's voice choice: persist it under the engine it belongs to.
    /// The restart rule is the same as an engine change — a voice only takes
    /// effect from the sentence the listener is on.
    fn voice_chosen(&self, app: &Rc<App>) {
        let Some(engine) = self.engine_string() else {
            return;
        };
        // The shown row is the voice's `name`; every engine shows readable
        // names. Map back to the code via the same
        // list the box enumerated. No match — a rebuild in flight, or the
        // "(default)" placeholder — writes nothing: a default nobody chose
        // is the wedge the module's rules exist to avoid.
        let lang = app.prefs.get().speech_language.clone();
        let voices = sn_tts::manager::voices_for_language(&engine, &lang);
        let Some(shown) = self.voice_string() else {
            return;
        };
        let Some(voice) = voices.iter().find(|v| v.name == shown) else {
            return;
        };
        if app.prefs.get().voice_for_engine(&engine) != voice.code {
            let code = voice.code.clone();
            app.prefs.update(|p| p.set_voice_for_engine(&engine, &code));
        }
        restart_live_session(app);
    }
}

/// Queue a restart of a live session at the heard frontier. The tick serves
/// it once the old session is terminal — the same path the speed control
/// uses, for the same reason: no session is replaced from a signal handler.
fn restart_live_session(app: &Rc<App>) {
    let session = app.tts.borrow().clone();
    let Some(session) = session else {
        return;
    };
    let progress = session.progress();
    if matches!(
        progress.state,
        sn_tts::PlayState::Playing | sn_tts::PlayState::Paused
    ) {
        app.tts_restart.set(Some(progress.frontier));
        session.stop();
    }
}

/// Refresh the pickers from prefs without treating the refill as a user
/// choice (the guard inside `rebuild_engine_list` is what makes that true).
/// Called when the bar is rebuilt.
pub fn refresh(picker: &PickerWidgets, app: &Rc<App>) {
    picker.rebuild_engine_list(app);
}
