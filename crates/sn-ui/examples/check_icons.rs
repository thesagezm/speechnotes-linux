//! Open a real window and prove every toolbar icon resolves.
//!
//! Run: cargo run -p sn-ui --example check_icons
//!
//! The seven icons Sn-ui ships are Adwaita-only names. This registers them
//! exactly as the app does, asks the live `GtkIconTheme` about every name the
//! app uses, and exits non-zero if any of them is missing — so "the icons are
//! broken" is a one-line check rather than an eyeball on a screenshot.

use gtk4 as gtk;

fn main() {
    gtk::init().expect("gtk init (needs a display)");

    // Same order the app uses: mount icons, then apply the theme.
    sn_ui::icons::register();

    let mut missing = 0;
    for name in sn_ui::icons::SHIPPED {
        let ok = has(name);
        println!("{} {name}", if ok { "ok  " } else { "MISS" });
        if !ok {
            missing += 1;
        }
    }
    println!();
    for name in sn_ui::icons::INHERITED {
        let ok = has(name);
        println!("{} {name}  (system)", if ok { "ok  " } else { "MISS" });
        if !ok {
            missing += 1;
        }
    }

    // The names the app asks for that are in neither list above would be a
    // drift between `window.rs` and `icons.rs`; catch it here too.
    let unlisted = unlisted_names();
    if !unlisted.is_empty() {
        println!();
        for name in &unlisted {
            println!("UNTESTED {name} — add it to icons.rs");
        }
    }

    println!();
    if missing == 0 && unlisted.is_empty() {
        println!("every icon resolves");
    } else {
        println!("{missing} icon(s) missing, {} untested", unlisted.len());
        std::process::exit(1);
    }
}

fn has(name: &str) -> bool {
    let Some(display) = gtk::gdk::Display::default() else {
        return false;
    };
    gtk::IconTheme::for_display(&display).has_icon(name)
}

/// Every icon name in `crates/sn-ui/src` that is not in `SHIPPED` or
/// `INHERITED`. Read straight from the source tree so it cannot go stale.
fn unlisted_names() -> Vec<String> {
    use std::collections::BTreeSet;
    let mut found = BTreeSet::new();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![dir];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().map(|e| e != "rs").unwrap_or(true) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let mut rest = text.as_str();
            while let Some(i) = rest.find('"') {
                rest = &rest[i + 1..];
                let Some(j) = rest.find('"') else { break };
                let word = &rest[..j];
                if word.ends_with("-symbolic") {
                    found.insert(word.to_string());
                }
                rest = &rest[j + 1..];
            }
        }
    }
    let known: Vec<&str> = sn_ui::icons::SHIPPED
        .iter()
        .chain(sn_ui::icons::INHERITED.iter())
        .copied()
        .collect();
    found
        .into_iter()
        .filter(|n| !known.contains(&n.as_str()))
        .collect()
}
