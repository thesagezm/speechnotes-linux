//! The fence every app webview sits behind.
//!
//! Both web engines in the reader — the EPUB book and the slide deck —
//! serve their document from this app's own URI scheme, and that scheme is
//! the document's only data source. The context and settings that enforce
//! that live here once, so the two engines cannot drift apart: a toggle
//! added for the book is a toggle the deck gets too.

/// A web context with `scheme` registered and served by `handler`, and the
/// scheme marked local — the document's own stylesheet and images resolve
/// without a cross-origin refusal.
pub fn scheme_context(
    scheme: &str,
    handler: impl Fn(&webkit6::URISchemeRequest) + 'static,
) -> webkit6::WebContext {
    let ctx = webkit6::WebContext::new();
    // The handler: the only way into the archive.
    ctx.register_uri_scheme(scheme, move |req| handler(req));
    if let Some(manager) = ctx.security_manager() {
        manager.register_uri_scheme_as_local(scheme);
    }
    ctx
}

/// The settings fence: JavaScript on (the reading layer needs it), developer
/// extras and console spam off, and no way out of the app's own scheme — no
/// speculative DNS, no file access from file URLs, no universal access.
/// (Hyperlink auditing no longer needs disabling — WebKit removed the
/// feature outright, and the setter only logs a deprecation warning.) A
/// page's own `<script>` still runs, but in a page whose only data source
/// is the archive it came in.
pub fn hardened_settings() -> webkit6::Settings {
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    settings.set_enable_developer_extras(false);
    settings.set_enable_write_console_messages_to_stdout(false);
    settings.set_enable_dns_prefetching(false);
    settings.set_allow_file_access_from_file_urls(false);
    settings.set_allow_universal_access_from_file_urls(false);
    settings
}
