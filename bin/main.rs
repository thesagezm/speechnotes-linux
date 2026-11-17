//! `speechnotes` — the binary.
//!
//! `--selftest` is the batch gate: every batch must keep the core provably
//! working headlessly. Without arguments the window opens.

mod selftest;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None | Some("") => {
            let ring = sn_core::LogRing::new();
            sn_core::logging::init(ring);
            tracing::info!(version = VERSION, "speechnotes starting");
            let code = sn_ui::window::run_entry();
            tracing::info!(code, "speechnotes exited");
            if code == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("--selftest") => {
            let scratch = args
                .get(1)
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("speechnotes-selftest"));
            let result = selftest::run(&scratch);
            println!("{}", result.text);
            match result.failed {
                0 => ExitCode::SUCCESS,
                n => {
                    eprintln!("selftest FAILED: {n} check(s) did not pass");
                    ExitCode::FAILURE
                }
            }
        }
        Some("--speak") => match SpeakOptions::parse(&args[1..]) {
            Ok(options) => speak_headless(&options),
            Err(why) => {
                eprintln!("speechnotes: {why}");
                ExitCode::FAILURE
            }
        },
        Some("--engines") => list_engines(),
        Some("--receive") => serve_bookdrop_headless(),
        Some("--import-book") => match args.get(1) {
            Some(path) => import_book(std::path::Path::new(path)),
            None => {
                eprintln!("speechnotes: --import-book needs a path to a book file");
                ExitCode::FAILURE
            }
        },
        Some("--import-note") => match args.get(1) {
            Some(path) => import_note(std::path::Path::new(path)),
            None => {
                eprintln!("speechnotes: --import-note needs a path to a .docx or .jex");
                ExitCode::FAILURE
            }
        },
        Some("--books") => {
            // The library first, repaired the same way the app's start-up
            // sweep repairs it: a manifest written before the m4b title
            // sanitiser carries container bytes as chapter titles, and a PDF
            // imported before poppler rendering existed has no cover drawn.
            if let Ok(repaired) = sn_store::books::BooksStore::repair_library() {
                for id in repaired {
                    println!("repaired {id}");
                }
            }
            list_books()
        }
        Some("--import-audiobook") => match args.get(1) {
            Some(path) => import_audiobook(std::path::Path::new(path)),
            None => {
                eprintln!("speechnotes: --import-audiobook needs a path to an .m4b/.opus/.mp3");
                ExitCode::FAILURE
            }
        },
        Some("--play-audiobook") => match args.get(1) {
            Some(prefix) => {
                sn_ui::abook::set_pending_open(prefix);
                let ring = sn_core::LogRing::new();
                sn_core::logging::init(ring);
                tracing::info!(prefix, "audiobook open requested");
                match sn_ui::window::run_entry() {
                    0 => ExitCode::SUCCESS,
                    _ => ExitCode::FAILURE,
                }
            }
            None => {
                eprintln!("speechnotes: --play-audiobook needs a book-id prefix");
                ExitCode::FAILURE
            }
        },
        Some("--read-book") => match args.get(1) {
            Some(prefix) => read_book(prefix, args.get(2).and_then(|p| p.parse::<usize>().ok())),
            None => {
                eprintln!("speechnotes: --read-book needs a book-id prefix");
                ExitCode::FAILURE
            }
        },
        Some("--open-book") => match args.get(1) {
            Some(prefix) => {
                sn_ui::reader::set_pending_open(prefix);
                // An optional chapter, and for a PDF that is a page:
                // `--open-book F010852C 233` opens page 233 of that document.
                if let Some(chapter) = args.get(2).and_then(|c| c.parse::<usize>().ok()) {
                    sn_ui::reader::set_pending_chapter(chapter);
                }
                let ring = sn_core::LogRing::new();
                sn_core::logging::init(ring);
                tracing::info!(prefix, "book open requested");
                match sn_ui::window::run_entry() {
                    0 => ExitCode::SUCCESS,
                    _ => ExitCode::FAILURE,
                }
            }
            None => {
                eprintln!("speechnotes: --open-book needs a book-id prefix");
                ExitCode::FAILURE
            }
        },
        Some("--play-note") => match args.get(1) {
            Some(prefix) => {
                sn_ui::player::set_autoplay(prefix);
                let ring = sn_core::LogRing::new();
                sn_core::logging::init(ring);
                tracing::info!(prefix, "autoplay requested");
                match sn_ui::window::run_entry() {
                    0 => ExitCode::SUCCESS,
                    _ => ExitCode::FAILURE,
                }
            }
            None => {
                eprintln!("speechnotes: --play-note needs a note-id prefix");
                ExitCode::FAILURE
            }
        },
        Some("--version") | Some("-V") => {
            println!("speechnotes {VERSION}");
            ExitCode::SUCCESS
        }
        Some("--help") | Some("-h") => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("speechnotes: unknown option {other:?}");
            print_usage();
            ExitCode::FAILURE
        }
    }
}

/// What `--speak` was asked to do. The engine and voice are flags rather
/// than prefs reads so the batch gate can drive every engine from one
/// command line without rewriting `prefs.json` between runs.
struct SpeakOptions {
    text: String,
    /// `None` reads `prefs.engineKind`, which is what the window does.
    engine: Option<String>,
    /// The voice of the engine named by the `--engine` that preceded it, or
    /// of `--switch` when it came after one. `--engine kokoro --voice
    /// af_bella --switch supertonic --voice F3` therefore gives each engine
    /// its own voice, which is what a voice comparison needs.
    voice: Option<String>,
    /// The voice `--switch` names, when `--voice` followed `--switch`.
    switch_voice: Option<String>,
    /// Switch to this engine once the first sentence has been heard — the
    /// gate for "switching engines mid-session does not leak or crash".
    switch_to: Option<String>,
    rate: Option<f64>,
}

impl SpeakOptions {
    /// `--speak [flags] TEXT...`. Flags may appear anywhere; the remainder is
    /// the text, so `--speak --engine kokoro hello there` does what it says.
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Self {
            text: String::new(),
            engine: None,
            voice: None,
            switch_voice: None,
            switch_to: None,
            rate: None,
        };
        let mut rest: Vec<String> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let arg = args[i].as_str();
            let value_for = |name: &str, i: &mut usize| -> Result<String, String> {
                *i += 1;
                args.get(*i)
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match arg {
                "--engine" => {
                    options.engine = Some(value_for("--engine", &mut i)?);
                    options.switch_voice = None;
                }
                // Attaches to whichever engine was named last, so the two
                // halves of a switch can each carry their own voice.
                "--voice" => {
                    let value = value_for("--voice", &mut i)?;
                    if options.switch_to.is_some() && options.engine.is_some() {
                        options.switch_voice = Some(value);
                    } else {
                        options.voice = Some(value);
                    }
                }
                "--switch" => {
                    options.switch_to = Some(value_for("--switch", &mut i)?);
                    options.switch_voice = None;
                }
                "--rate" => {
                    let raw = value_for("--rate", &mut i)?;
                    let value: f64 = raw
                        .parse()
                        .map_err(|_| format!("--rate wants a number, got {raw:?}"))?;
                    options.rate = Some(value);
                }
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag {other}"));
                }
                _ => rest.push(arg.to_string()),
            }
            i += 1;
        }
        options.text = rest.join(" ");
        if options.text.trim().is_empty() {
            return Err("--speak needs text to speak".to_string());
        }
        Ok(options)
    }
}

/// `--engines`: what this box can actually speak, with each engine's voices.
fn list_engines() -> ExitCode {
    use sn_tts::manager::{ModelManager, InstallState};
    println!("speechnotes engines");
    for engine in sn_tts::manager::catalog() {
        let state = ModelManager::state(engine.engine);
        let mark = if state == InstallState::Installed {
            "installed"
        } else {
            "not installed"
        };
        let voices = ModelManager::voices(engine.engine);
        println!(
            "  {:<11} {:<14} {:>6.0} MB  {} voice(s)",
            engine.engine,
            mark,
            engine.approx_bytes() as f64 / 1_000_000.0,
            voices.len()
        );
    }
    let espeak = sn_tts::manager::ModelManager::voices("espeak").len();
    println!("  {:<11} {:<14} {:>6}      {} voice(s)", "espeak", "built in", "-", espeak);
    println!("\npreferred: {}", ModelManager::preferred_engine());
    ExitCode::SUCCESS
}

/// `--receive`: the BookDrop receiver without a window. The same service the
/// app runs, wired to the same importers, reporting to stdout instead of the
/// settings page's history list. Ctrl+C is the stop: the process is the
/// listener's lifetime.
fn serve_bookdrop_headless() -> ExitCode {
    let ring = sn_core::LogRing::new();
    sn_core::logging::init(ring);
    let notes = match sn_store::NotesStore::open() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("speechnotes: cannot open the notes library: {e}");
            return ExitCode::FAILURE;
        }
    };
    let notebooks = match sn_store::NotebooksStore::open() {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("speechnotes: cannot open the notebooks: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Headless has nobody watching a toggle, so the receiver serves
    // unconditionally: the process IS the opt-in, and auto-accept is what
    // the pref says (the service reads it while building the router).
    let service = sn_ui::bookdrop::Service::with_stores(
        sn_store::PrefsStore::open().ok().as_ref(),
        notes,
        notebooks,
    );
    let port = service.port();
    //  the original bookdrop Service::start() takes no port (the older
            // call shape passed it).
            match service.start() {
        Ok(()) => {
            println!("BookDrop listening on port {port} — LocalSend sends land in the library");
        }
        Err(e) => {
            eprintln!("speechnotes: BookDrop cannot listen on port {port}: {e}");
            return ExitCode::FAILURE;
        }
    }
    // The receive loop is the service's own thread; this one only prints
    // what arrives, newest first, until the process is interrupted.
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        for record in service.drain_events() {
            use sn_net::bookdrop::TransferOutcome;
            match record.outcome {
                TransferOutcome::Imported(where_) => {
                    println!("landed: {} -> {where_}", record.name)
                }
                TransferOutcome::Failed(why) => println!("FAILED: {} — {why}", record.name),
                TransferOutcome::Rejected => println!("rejected: {}", record.name),
            }
        }
    }
}

/// `--import-book PATH`: copy a book into the library and print its manifest.
fn import_book(path: &std::path::Path) -> ExitCode {
    // The extension decides the importer, exactly as the GUI chooser does:
    // a .pdf the PDF parser, a headless document (the Mobipocket/Kindle
    // family, FB2, RTF, HTML, plain text) the document parser, anything
    // else the EPUB parser, which refuses non-EPUBs with the reason.
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let imported = if extension == "pdf" {
        sn_store::books::BooksStore::import_pdf(path)
    } else if sn_doc::bookdoc::DocFamily::is_document_extension(&extension) {
        sn_store::books::BooksStore::import_document(path)
    } else {
        sn_store::books::BooksStore::import(path)
    };
    match imported {
        Ok(book) => {
            println!(
                "imported {} — {} chapter(s), {} TOC row(s), cover {}",
                book.manifest.id,
                book.manifest.spine_count,
                book.manifest.toc.len(),
                if book.manifest.has_cover { "yes" } else { "no" }
            );
            println!("  title:  {}", book.manifest.title);
            println!("  author: {}", book.manifest.author);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("speechnotes: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--books`: the library, as the shelf shows it.
fn list_books() -> ExitCode {
    match sn_store::books::BooksStore::all() {
        Ok(books) if books.is_empty() => {
            println!("no books imported yet");
            ExitCode::SUCCESS
        }
        Ok(books) => {
            println!("speechnotes books");
            for book in &books {
                let chapters = if book.is_audiobook() {
                    book.manifest.chapters.len()
                } else {
                    book.manifest.spine_count
                };
                // The header first, then its rows: a book's contents belong to
                // the book named above them. Printing the rows first made the
                // last book's contents appear to belong to the next one.
                println!(
                    "  {}  {:<40} {:>3} ch  {}",
                    &book.manifest.id[..8.min(book.manifest.id.len())],
                    truncate(&book.manifest.title, 40),
                    chapters,
                    book.manifest.author
                );
                // The rows the contents popover shows: the label with the
                // outline's nesting indented, and the href a PDF row points
                // at. Garbage titles were repaired on the way in, so what
                // prints here is what the shelf shows.
                for row in &book.manifest.toc {
                    println!(
                        "      {}{:<48} {}",
                        "  ".repeat(row.depth.min(4)),
                        truncate(&row.label, 48),
                        row.href
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("speechnotes: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--import-audiobook PATH`: import an audiobook and print its manifest.
fn import_audiobook(path: &std::path::Path) -> ExitCode {
    match sn_store::books::BooksStore::import_audiobook(path) {
        Ok(book) => {
            println!(
                "imported {} — {} chapter(s), codec {}, {:.1} min",
                book.manifest.id,
                book.manifest.chapters.len(),
                book.manifest.audio_codec.as_deref().unwrap_or("unknown"),
                book.manifest.duration_seconds.unwrap_or(0.0) / 60.0,
            );
            println!("  title: {}", book.manifest.title);
            for (i, c) in book.manifest.chapters.iter().take(6).enumerate() {
                println!("  [{i}] {:.2}–{:.2}s  {}", c.start_seconds, c.end_seconds, c.title);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("speechnotes: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--read-book PREFIX`: the first chapter's text, for the batch gate — the
/// headless proof that import → read works without a window.
fn read_book(prefix: &str, page: Option<usize>) -> ExitCode {
    let book = match sn_store::books::BooksStore::find(prefix) {
        Ok(Some(book)) => book,
        Ok(None) => {
            eprintln!("speechnotes: no book matches {prefix:?}");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("speechnotes: {e}");
            return ExitCode::FAILURE;
        }
    };
    // An audiobook has no spine; its chapter list is the manifest's own.
    if book.is_audiobook() {
        println!(
            "{} — {} ({} audio chapter(s), {:.1} min, codec {})",
            book.manifest.title,
            book.manifest.author,
            book.manifest.chapters.len(),
            book.manifest.duration_seconds.unwrap_or(0.0) / 60.0,
            book.manifest.audio_codec.as_deref().unwrap_or("unknown"),
        );
        for (i, c) in book.manifest.chapters.iter().take(6).enumerate() {
            println!("  [{i}] {:.2}–{:.2}s  {}", c.start_seconds, c.end_seconds, c.title);
        }
        return ExitCode::SUCCESS;
    }
    println!(
        "{} — {} ({} chapter(s), {} TOC row(s))",
        book.manifest.title,
        book.manifest.author,
        book.manifest.spine_count,
        book.manifest.toc.len()
    );
    // The position's chapter when one is saved, otherwise the first chapter
    // with prose in it — front matter is not a reading position.
    let index = page.unwrap_or(if book.manifest.position.chapter_index > 0 {
        book.manifest.position.chapter_index
    } else {
        book.first_speakable_chapter()
    });
    match book.chapter_text(index) {
        Ok(text) if text.trim().is_empty() => {
            // Front matter (a cover page, a title page) has no prose. Say
            // so rather than printing a blank line and looking broken.
            println!(
                "chapter [{index}] has no speakable text (front matter)"
            );
            ExitCode::SUCCESS
        }
        Ok(text) => {
            let preview: String = text.chars().take(400).collect();
            println!("chapter [{index}] {} bytes:\n{preview}", text.len());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("speechnotes: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--import-note PATH`: bring a `.docx` (office) or `.jex` (Joplin export)
/// into the library, printing what landed. The headless gate for Batch 10's
/// import work.
fn import_note(path: &std::path::Path) -> ExitCode {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let result = match extension.as_str() {
        "docx" => {
            let notes = match sn_store::NotesStore::open() {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("speechnotes: could not open the notes store: {e}");
                    return ExitCode::FAILURE;
                }
            };
            sn_store::imports::import_docx(&notes, path, None)
        }
        "jex" => {
            let notes = match sn_store::NotesStore::open() {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("speechnotes: could not open the notes store: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let notebooks = match sn_store::NotebooksStore::open() {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("speechnotes: could not open the notebooks store: {e}");
                    return ExitCode::FAILURE;
                }
            };
            sn_store::imports::import_jex(&notes, &notebooks, path)
        }
        other => {
            eprintln!(
                "speechnotes: --import-note reads .docx and .jex, not .{other}"
            );
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(report) => {
            println!(
                "imported: {} note(s), {} notebook(s), {} image(s) stored{}",
                report.notes_imported,
                report.notebooks_created,
                report.images_stored,
                if report.errors.is_empty() {
                    String::new()
                } else {
                    format!(", {} error(s)", report.errors.len())
                }
            );
            for error in &report.errors {
                eprintln!("  {error}");
            }
            // A notebook-only archive is a success that imported no notes;
            // the exit code follows the operation, not the tally.
            if report.is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("speechnotes: {e}");
            ExitCode::FAILURE
        }
    }
}

fn truncate(s: &str, width: usize) -> String {
    let mut out: String = s.chars().take(width).collect();
    if s.chars().count() > width {
        out.push('…');
    }
    out
}

/// `--speak`: one headless pass through the real pipeline — plan, engine,
/// threads, device — ending with the metrics the batch gate reports.
fn speak_headless(options: &SpeakOptions) -> ExitCode {

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .init();

    let prefs = sn_store::stores::Prefs::load();
    let engine_name = options
        .engine
        .clone()
        .unwrap_or_else(|| prefs.engine_kind.clone());
    // `--voice` is the voice of the engine `--engine` names. With `--switch`
    // that is the *second* engine, so the flag is only applied to the first
    // when there is no switch — otherwise a kokoro voice code would be handed
    // to espeak and every chunk would fail with a voice-not-found error.
    let voice = options
        .voice
        .clone()
        .unwrap_or_else(|| prefs.voice_for_engine(&engine_name));
    let rate = options.rate.unwrap_or(prefs.rate_multiplier);

    // espeak's availability is the CLI's; the neural engines' is their model
    // files, and the loader's error already names which one is missing.
    if engine_name == "espeak" {
        if let Err(e) = sn_tts::EspeakEngine::available() {
            eprintln!("speechnotes: {e}");
            return ExitCode::FAILURE;
        }
    }
    println!("engine: {engine_name}  voice: {}  rate: {rate:.2}", if voice.is_empty() { "(default)" } else { &voice });

    let lang = prefs.speech_language.clone();
    let engine: Arc<dyn sn_tts::Engine> =
        match sn_tts::manager::build_engine(&engine_name, &voice, rate, &lang) {
        Ok(engine) => Arc::from(engine),
        Err(e) => {
            eprintln!("speechnotes: {e}");
            return ExitCode::FAILURE;
        }
    };
    let session = match sn_tts::SessionCore::start(
        &options.text,
        0,
        engine,
        sn_tts::SessionConfig::default(),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("speechnotes: could not start playback: {e}");
            return ExitCode::FAILURE;
        }
    };

    let Some(switch_to) = options.switch_to.as_deref() else {
        let metrics = session.join();
        report(&metrics, "");
        return match metrics.state {
            sn_tts::PlayState::Finished => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        };
    };

    // The mid-session switch, which is the batch's real gate. The first
    // session is stopped once it has been *heard* — not once it has
    // synthesised — and the second starts from the frontier the first one's
    // played-frame accounting reached. Reading the frontier from the
    // session, never from the plan, is the Batch 5 write-ahead rule: a
    // frontier guessed from progress would replay or skip a sentence.
    let frontier = wait_for_heard_frontier(&session);
    session.stop();
    let first = session.join();
    println!(
        "switched at byte {frontier} after {:.1}s of {} audio",
        first.audio_total().as_secs_f64(),
        engine_name
    );
    report(&first, &format!("engine 1 ({engine_name})"));

    let voice_b = options
        .switch_voice
        .clone()
        .unwrap_or_else(|| prefs.voice_for_engine(switch_to));
    let engine_b: Arc<dyn sn_tts::Engine> =
        match sn_tts::manager::build_engine(switch_to, &voice_b, rate, &lang) {
            Ok(engine) => Arc::from(engine),
            Err(e) => {
                eprintln!("speechnotes: could not load {switch_to}: {e}");
                return ExitCode::FAILURE;
            }
        };
    println!("engine: {switch_to}  voice: {}", if voice_b.is_empty() { "(default)" } else { &voice_b });
    let second = match sn_tts::SessionCore::start(
        &options.text,
        frontier,
        engine_b,
        sn_tts::SessionConfig::default(),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("speechnotes: could not resume on {switch_to}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let second_metrics = second.join();
    report(&second_metrics, &format!("engine 2 ({switch_to})"));
    // The first half is *supposed* to be `Stopped` — that is the switch. What
    // must hold is that it stopped at a heard position inside the text, and
    // that the second half carried the rest to the end. A switch that
    // dropped the tail, or that resumed from zero and re-spoke the opening,
    // is as broken as one that crashed.
    let ok = first.state == sn_tts::PlayState::Stopped
        && first.frontier > 0
        && first.frontier < options.text.len()
        && second_metrics.state == sn_tts::PlayState::Finished
        && second_metrics.errors == 0;
    if !ok {
        eprintln!(
            "speechnotes: switch gate failed — first {:?} at byte {} of {}, second {:?} with {} error(s)",
            first.state,
            first.frontier,
            options.text.len(),
            second_metrics.state,
            second_metrics.errors
        );
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Wait until the session has actually *played* past its first sentence, or
/// the timeout expires. Returns the frontier it reached.
///
/// Polling rather than sleeping a fixed time: the frontier is measured in
/// played frames, and only that says what the listener heard. The cap is
/// generous — a slow first engine on a cold ORT session is still a normal
/// run, and a fixed sleep would switch mid-synthesis.
fn wait_for_heard_frontier(session: &Arc<sn_tts::SessionCore>) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let progress = session.progress();
        if progress.state.is_terminal() {
            return progress.frontier;
        }
        // One sentence's worth of audio is the smallest useful switch point:
        // before it, the switch proves nothing; after it, the first engine
        // has a result worth reporting.
        if progress.frontier > 0 && progress.audio_total.as_secs_f64() > 0.5 {
            return progress.frontier;
        }
        if std::time::Instant::now() > deadline {
            return progress.frontier;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Print one session's metrics, under a heading when there are two.
fn report(metrics: &sn_tts::Metrics, heading: &str) {
    if !heading.is_empty() {
        println!("\n--- {heading} ---");
    }
    println!("state: {:?}", metrics.state);
    if let Some(ttfa) = metrics.ttfa {
        println!("TTFA: {:.0} ms", ttfa.as_secs_f64() * 1000.0);
    }
    println!(
        "audio: {:.2}s total, synthesis: {:.3}s, mean RTF: {:.4}, errors: {}",
        metrics.audio_total().as_secs_f64(),
        metrics.synth_total().as_secs_f64(),
        metrics.mean_rtf(),
        metrics.errors
    );
    println!("  chunk  chars   synth-ms   audio-s     RTF");
    for c in &metrics.chunks {
        println!(
            "  {index:>5}  {chars:>5}  {synth:>8.1}  {audio:>7.2}  {rtf:>7.4}{beep}",
            index = c.index,
            chars = c.chars,
            synth = c.synth.as_secs_f64() * 1000.0,
            audio = c.audio.as_secs_f64(),
            rtf = c.rtf(),
            beep = if c.beeped { "  (beeped)" } else { "" }
        );
    }
}

fn print_usage() {
    println!(
        "speechnotes {VERSION} — TTS notes, books and audiobooks for Linux

USAGE:
    speechnotes                 launch the app
    speechnotes --selftest [DIR] run the core checks headlessly (no display)
    speechnotes --engines       list the engines this box can speak, and
                                which model sets are installed
    speechnotes --import-note P
                                import a .docx (office) or .jex (Joplin export)
                                into the notes library
    speechnotes --import-book P import a book — .epub, .pdf, Kindle/Mobipocket,
                                FB2, RTF, HTML or plain text — into the library
    speechnotes --books         list the imported books
    speechnotes --read-book ID  print the first chapter of the book whose id
                                begins with ID (the headless read gate)
    speechnotes --import-audiobook P
                                import an .m4b/.opus/.mp3 into the library
    speechnotes --play-audiobook ID
                                open the window playing the audiobook whose id
                                begins with ID
    speechnotes --speak [FLAGS] TEXT
                                speak TEXT through the real pipeline, print
                                TTFA and per-chunk RTF metrics, exit

    --engine NAME   speak with this engine instead of the one in prefs
    --voice CODE    use this voice instead of the per-engine prefs voice
    --rate N        speaking rate, 0.5-2.0
    --switch NAME   switch to this engine once the first sentence has been
                    heard, and resume from there (the batch gate)

    speechnotes --open-book ID   open the window on the book whose id begins
                                with ID (the reader gate)
    speechnotes --open-book ID N open that book at chapter N — and for a PDF,
                                at page N
    speechnotes --play-note ID  open the window and start speaking the note
                                whose id begins with ID
    speechnotes --version       print the version
    speechnotes --help          this message"
    );
}
