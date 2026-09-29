import Foundation
import CGTK4
import SwiftCrossUI
import Log

/// Owns the app's effective color scheme and accent, and installs the
/// display-wide GTK CSS that carries them into the theme-owned parts of the
/// UI (window background, text views, entries, native controls). The
/// SwiftCrossUI side follows `effectiveScheme` through the `.colorScheme`
/// environment on the app shell.
///
/// All GTK C interop is isolated here; the rest of the app sees only
/// SwiftCrossUI types.
@MainActor
public final class ThemeController: ObservableObject {
    public static let shared = ThemeController()

    /// The scheme SCU views should paint with (root `.colorScheme` value).
    @Published public private(set) var effectiveScheme: ColorScheme = .light
    /// The accent for SCU-side tints (active sidebar rows, playing states).
    @Published public private(set) var accent: Color = AccentChoice.blue.color

    private var currentAccent: AccentChoice = .blue
    private var provider: UnsafeMutablePointer<GtkCssProvider>?
    private var appliedCSS: String?

    /// Idempotent: called from the app shell body on every preference change.
    public func sync(appearance: String, accentChoice: String) {
        let scheme: ColorScheme
        switch appearance {
        case "light": scheme = .light
        case "dark": scheme = .dark
        default: scheme = Self.systemScheme()
        }
        if scheme != effectiveScheme {
            effectiveScheme = scheme
        }

        let choice = AccentChoice(rawValue: accentChoice) ?? .blue
        if choice != currentAccent {
            currentAccent = choice
            accent = choice.color
        }

        applyGTK(scheme: scheme, accent: choice)
    }

    // MARK: - GTK plumbing

    private func applyGTK(scheme: ColorScheme, accent: AccentChoice) {
        let css = Palette.themeCSS(scheme: scheme, accent: accent)
        guard css != appliedCSS else { return }
        appliedCSS = css

        if provider == nil {
            guard let newProvider = gtk_css_provider_new() else {
                Log.error("ThemeController: gtk_css_provider_new failed")
                return
            }
            gtk_style_context_add_provider_for_display(
                gdk_display_get_default(),
                OpaquePointer(newProvider),
                Palette.providerPriority
            )
            provider = newProvider
        }
        gtk_css_provider_load_from_string(provider, css)

        // Flip the whole GTK theme (dialogs, file choosers, scrollbars) with
        // the app scheme; in "system" mode this just restates the desktop's
        // own preference.
        Self.setPreferDark(scheme == .dark)
    }

    /// Best-effort read of the desktop's dark preference: the explicit
    /// prefer-dark flag first, then dark-sounding theme names. Light is the
    /// fallback when GTK offers no opinion.
    public static func systemScheme() -> ColorScheme {
        guard let settings = gtk_settings_get_default() else { return .light }
        let object = UnsafeMutableRawPointer(settings).assumingMemoryBound(to: GObject.self)

        if boolProperty(of: object, name: "gtk-application-prefer-dark-theme") {
            return .dark
        }
        if let themeName = stringProperty(of: object, name: "gtk-theme-name") {
            let lowered = themeName.lowercased()
            if lowered.contains("dark") || lowered.contains("night") || lowered.contains("black") {
                return .dark
            }
        }
        return .light
    }

    static func setPreferDark(_ dark: Bool) {
        guard let settings = gtk_settings_get_default() else { return }
        let object = UnsafeMutableRawPointer(settings).assumingMemoryBound(to: GObject.self)
        withGValue(boolean: true) { value in
            g_value_set_boolean(value, dark ? 1 : 0)
            g_object_set_property(object, "gtk-application-prefer-dark-theme", value)
        }
    }

    private static func boolProperty(
        of object: UnsafeMutablePointer<GObject>,
        name: String
    ) -> Bool {
        withGValue(boolean: true) { value in
            g_object_get_property(object, name, value)
            return g_value_get_boolean(value) != 0
        }
    }

    private static func stringProperty(
        of object: UnsafeMutablePointer<GObject>,
        name: String
    ) -> String? {
        withGValue(boolean: false) { value in
            g_object_get_property(object, name, value)
            guard let cString = g_value_get_string(value) else { return nil }
            return String(cString: cString)
        }
    }

    /// Runs a closure with an initialized GValue. GLib fundamentals are frozen
    /// ABI: G_TYPE_BOOLEAN is fundamental 5, G_TYPE_STRING fundamental 16,
    /// shifted left by G_TYPE_FUNDAMENTAL_SHIFT (2).
    private static func withGValue<T>(
        boolean: Bool,
        _ body: (UnsafeMutablePointer<GValue>) -> T
    ) -> T {
        let value = UnsafeMutablePointer<GValue>.allocate(capacity: 1)
        value.initialize(repeating: GValue(), count: 1)
        defer { value.deallocate() }
        g_value_init(value, boolean ? GType(20) : GType(64))
        return body(value)
    }
}
