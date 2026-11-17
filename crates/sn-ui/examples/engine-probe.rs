// The engine probe: open one book in the BookView, then dump what the
// shell actually produced — the DOM's size, the __snbook surface, the
// paginator's presence — into a file. The GUI probe for "the pane is
// white": everything the reader shows, printed from inside the engine.
//
//     cargo run --release -p sn-ui --example engine-probe -- <book-file> <out.json>

use std::cell::RefCell;
use std::rc::Rc;

use webkit6::prelude::*;

fn main() {
    let path = std::env::args().nth(1).expect("a book file path");
    let out = std::env::args().nth(2).expect("an output path");
    gtk4::init().expect("gtk init");
    let log: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    let view = sn_ui::bookview::BookView::new();
    {
        let log = log.clone();
        view.on_book_info(move |info| {
            log.borrow_mut().push(format!(
                "book-report: sections={} toc-rows={} title={:?}",
                info.section_count,
                info.toc.len(),
                info.title
            ));
        });
    }
    {
        let log = log.clone();
        view.on_relocate(move |rel| {
            log.borrow_mut().push(format!(
                "relocate: index={} fraction={} label={:?} cfi={}",
                rel.index,
                rel.fraction,
                rel.label,
                if rel.cfi.is_empty() { "-" } else { &rel.cfi[..rel.cfi.len().min(40)] }
            ));
        });
    }
    {
        let log = log.clone();
        view.on_error(move |e| {
            log.borrow_mut().push(format!("error: {e}"));
        });
    }
    {
        let log = log.clone();
        view.on_text(move |index, text| {
            log.borrow_mut().push(format!(
                "text[{}]: {} chars, starts {:?}",
                index,
                text.chars().count(),
                text.chars().take(400).collect::<String>()
            ));
        });
    }

    let format = path
        .rsplit('.')
        .next()
        .unwrap_or("mobi")
        .to_ascii_lowercase();
    let p = std::path::PathBuf::from(&path);
    if let Ok(url) = std::env::var("SN_HTTP_SHELL") {
        // The bisect: the same module graph over plain http — if it runs
        // here and not over the scheme, the scheme handler is the suspect.
        view.widget().load_uri(&url);
    } else {
        view.open(&p, &format, "probe", "", None);
    }
    {
        let load_log = log.clone();
        view.widget().connect_load_changed(move |view, event| {
            load_log.borrow_mut().push(format!(
                "load: {event:?} uri={:?}",
                view.uri().map(|u| u.to_string()).unwrap_or_default()
            ));
        });
        let fail_log = log.clone();
        view.widget().connect_load_failed(move |_, event, uri, error| {
            fail_log.borrow_mut().push(format!("load-failed: {event:?} {uri} {error}"));
            true
        });
    }

    // NOTE: no window — an unmapped webview still loads and evaluates
    // (measured: the windowed run hangs on present(); the windowless one
    // reaches the timer), and the probe only needs the page's internals.
    let inner = log.clone();
    let view2 = view.clone();
    let main_loop = gtk4::glib::MainLoop::new(None, false);
    // SN_PROBE_STEER=1: at t=8 s steer the engine (goToIndex via a
    // promise-chained STRING result — also answers whether WebKit awaits
    // promises), then let the 10 s dump show where the view landed.
    if std::env::var_os("SN_PROBE_STEER").is_some() {
        let steer_log = log.clone();
        let view3 = view.clone();
        gtk4::glib::timeout_add_local(std::time::Duration::from_secs(8), move || {
            steer_log.borrow_mut().push("steering: go_to_index(2) issued".into());
            let steer_log2 = steer_log.clone();
            view3.widget().evaluate_javascript(
                "window.__snbook ? window.__snbook.goToIndex(2).then(() => 'steered-ok') : 'no-shell'",
                None,
                None,
                None::<&gtk4::gio::Cancellable>,
                move |result| {
                    steer_log2.borrow_mut().push(format!(
                        "steer-eval: {}",
                        match result {
                            Ok(v) => v.to_str().to_string(),
                            Err(e) => format!("ERR {e}"),
                        }
                    ));
                },
            );
            gtk4::glib::ControlFlow::Break
        });
    }
    {
        let main_loop = main_loop.clone();
        gtk4::glib::timeout_add_local(std::time::Duration::from_secs(10), move || {
            // What the page holds: the shell, the view element, the renderer.
            let script = "JSON.stringify({\
                shell: !!window.__snbook,\
                body: document.body ? document.body.children.length : -1,\
                dom: document.body ? document.body.innerHTML.length : -1,\
                viewEl: !!document.querySelector('foliate-view'),\
                renderer: document.querySelector('foliate-view') ?\
                    (document.querySelector('foliate-view').shadowRoot ?\
                     document.querySelector('foliate-view').shadowRoot.children.length : -1) : -2,\
                iframes: document.querySelectorAll('iframe').length,\
                page: window.__snbook ? JSON.stringify(window.__snbook.pageOffset()) : null,\
                html: (document.body ? document.body.innerHTML : '').slice(0, 400),\
                err: window.__snLastError || null\
            })";
            let log = inner.clone();
            let out = out.clone();
            let main_loop = main_loop.clone();
            view2.widget().evaluate_javascript(
                script,
                None,
                None,
                None::<&gtk4::gio::Cancellable>,
                move |result| {
                    match result {
                        Ok(v) => log.borrow_mut().push(format!("dom: {}", v.to_str())),
                        Err(e) => log.borrow_mut().push(format!("eval-error: {e}")),
                    }
                    let payload: String = log.borrow().join("\n");
                    let _ = std::fs::write(&out, payload);
                    main_loop.quit();
                },
            );
            gtk4::glib::ControlFlow::Break
        });
    }

    main_loop.run();
}
