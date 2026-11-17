//! The static assets the book view serves over its loopback HTTP origin:
//! the vendored foliate-js modules (readest's fork, MIT — see
//! `assets/foliate-js/LICENSE`), the shell that boots them, and the
//! in-memory book builders the shell dispatches through.
//!
//! Every file is `include_bytes!`'d into the binary — the reader's engine
//! ships with the app, nothing is read from disk at open time. The view's
//! server answers a `viewer/<name>` path from this table and refuses
//! anything else, so the page can load only what was compiled in.

/// (name, MIME, bytes) — the table the scheme handler serves from.
macro_rules! assets {
    ($($name:literal => $mime:literal, $path:literal;)*) => {
        pub const VIEWER_ASSETS: &[(&str, &str, &[u8])] = &[
            $(($name, $mime, include_bytes!($path))),*
        ];
    };
}

assets! {
    "sn-shell.html" => "text/html", "../assets/sn-shell.html";
    "sn-shell.js" => "text/javascript", "../assets/foliate-js/sn-shell.js";
    "view.js" => "text/javascript", "../assets/foliate-js/view.js";
    "paginator.js" => "text/javascript", "../assets/foliate-js/paginator.js";
    "epub.js" => "text/javascript", "../assets/foliate-js/epub.js";
    "epubcfi.js" => "text/javascript", "../assets/foliate-js/epubcfi.js";
    "mobi.js" => "text/javascript", "../assets/foliate-js/mobi.js";
    "fb2.js" => "text/javascript", "../assets/foliate-js/fb2.js";
    "fixed-layout.js" => "text/javascript", "../assets/foliate-js/fixed-layout.js";
    "progress.js" => "text/javascript", "../assets/foliate-js/progress.js";
    "overlayer.js" => "text/javascript", "../assets/foliate-js/overlayer.js";
    "text-walker.js" => "text/javascript", "../assets/foliate-js/text-walker.js";
    "section-chunks.js" => "text/javascript", "../assets/foliate-js/section-chunks.js";
    "sn-docbook.js" => "text/javascript", "../assets/foliate-js/sn-docbook.js";
    "sn-txt.js" => "text/javascript", "../assets/foliate-js/sn-txt.js";
    "sn-html.js" => "text/javascript", "../assets/foliate-js/sn-html.js";
    "sn-rtf.js" => "text/javascript", "../assets/foliate-js/sn-rtf.js";
    "vendor/zip.js" => "text/javascript", "../assets/foliate-js/vendor/zip.js";
    "vendor/fflate.js" => "text/javascript", "../assets/foliate-js/vendor/fflate.js";
    "vendor/construct-style-sheets-polyfill.js" => "text/javascript",
        "../assets/foliate-js/vendor/construct-style-sheets-polyfill.js";
}

/// The shell page as served: `sn-shell.html` with the shell's module
/// spliced inline before `</body>`.
///
/// This WebKit never executes an external `<script type="module" src>` —
/// no fetch, no error, nothing (measured with a known-good server; an
/// inline module importing the very same files runs fine). So the asset
/// layer composes the page once, at first use, from the two compiled-in
/// files: the HTML keeps its head, styles and the classic error-hook
/// script, and the marker comment is replaced by the module. The module's
/// own imports stay external (`./view.js` beside the page) — external
/// *imports* work; only the src tag fails.
pub fn shell_page() -> &'static str {
    static PAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PAGE.get_or_init(|| {
        let html = std::str::from_utf8(
            viewer_asset_from_table("sn-shell.html").expect("the shell page").1,
        )
        .expect("the shell page is utf-8");
        let js = std::str::from_utf8(
            viewer_asset_from_table("sn-shell.js").expect("the shell script").1,
        )
        .expect("the shell script is utf-8");
        // Inline-script escaping: a literal `</script` inside the module
        // would close the element mid-module; `\/` is the same character
        // wherever it can legally appear in JS (strings, regexes). The
        // shell carries none today; the escape is future-proofing.
        let js = js.replace("</script", "<\\/script");
        html.replace(
            "<!-- sn-shell-module -->",
            &format!("<script type=\"module\">\n{js}\n</script>"),
        )
    })
}

/// The bytes for one `viewer/<name>` path, if the table holds it. The
/// shell page answers with the composed page — the module spliced inline.
pub fn viewer_asset(name: &str) -> Option<(&'static str, &'static [u8])> {
    if name == "sn-shell.html" {
        return Some(("text/html", shell_page().as_bytes()));
    }
    viewer_asset_from_table(name)
}

/// The plain table lookup — the raw compiled-in bytes.
fn viewer_asset_from_table(name: &str) -> Option<(&'static str, &'static [u8])> {
    VIEWER_ASSETS
        .iter()
        .find(|(asset_name, _, _)| *asset_name == name)
        .map(|(_, mime, bytes)| (*mime, *bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shell_and_the_view_are_in_the_table() {
        let (mime, _) = viewer_asset("sn-shell.html").expect("the shell page");
        assert_eq!(mime, "text/html");
        viewer_asset("view.js").expect("the foliate view");
        viewer_asset("vendor/zip.js").expect("the zip reader");
        assert!(viewer_asset("not-a-thing.js").is_none(), "no guesses");
        // Every table entry's name is a safe URL path: no separators, no
        // escapes, nothing the handler would have to decode.
        for (name, _, _) in VIEWER_ASSETS {
            assert!(
                name.chars().all(|c| c.is_ascii_alphanumeric()
                    || matches!(c, '-' | '_' | '.' | '/')),
                "{name} must be a plain path"
            );
            assert!(!name.starts_with('/') && !name.contains(".."));
        }
    }

    #[test]
    fn the_shell_page_serves_the_module_inline() {
        // This WebKit never executes an external <script type="module"
        // src>, so the served page must carry the module itself — and the
        // error hook must precede it, to catch even a failed graph.
        let (_, page) = viewer_asset("sn-shell.html").expect("the shell page");
        let page = std::str::from_utf8(page).expect("utf-8");
        assert!(page.contains("<script type=\"module\">"), "the spliced module");
        assert!(
            !page.contains("<script type=\"module\" src="),
            "no external module src survives"
        );
        assert!(
            page.contains("import { View } from './view.js'"),
            "the shell's own imports came along"
        );
        let hook = page.find("window.__snLastError").expect("the error hook");
        let module = page.find("<script type=\"module\">").expect("the module");
        assert!(hook < module, "the hook is installed before the module");
        assert!(!page.contains("sn-shell-module"), "the marker was replaced");
    }
}
