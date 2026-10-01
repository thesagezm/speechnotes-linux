import Foundation
import SwiftCrossUI

/// Accent color choices, mirroring the iOS app's AccentColorChoice. `system`
/// leaves the GTK theme's own accent in place for the native side while the
/// SCU-side tints fall back to the GNOME blue.
public enum AccentChoice: String, CaseIterable, Sendable {
    case system
    case blue, indigo, purple, pink, red, orange, yellow, green, teal, mint, cyan

    public var displayName: String {
        switch self {
        case .system: return "System"
        case .blue: return "Blue"
        case .indigo: return "Indigo"
        case .purple: return "Purple"
        case .pink: return "Pink"
        case .red: return "Red"
        case .orange: return "Orange"
        case .yellow: return "Yellow"
        case .green: return "Green"
        case .teal: return "Teal"
        case .mint: return "Mint"
        case .cyan: return "Cyan"
        }
    }

    /// The concrete color used for SCU-side tints (active rows, playing
    /// states). Mid-tone values readable as text on both light and dark
    /// backgrounds — GNOME-palette adjacent.
    public var color: Color {
        switch self {
        case .system: return Color(hex: 0x3584E4)
        case .blue: return Color(hex: 0x3584E4)
        case .indigo: return Color(hex: 0x5B57BD)
        case .purple: return Color(hex: 0x9141AC)
        case .pink: return Color(hex: 0xD56199)
        case .red: return Color(hex: 0xC01C28)
        case .orange: return Color(hex: 0xED5B00)
        case .yellow: return Color(hex: 0xC88800)
        case .green: return Color(hex: 0x3A944A)
        case .teal: return Color(hex: 0x0F9D8F)
        case .mint: return Color(hex: 0x43B58C)
        case .cyan: return Color(hex: 0x1899B6)
        }
    }

    /// The CSS hex to install as the theme's accent colors, or nil for
    /// `system` (the GTK theme keeps its own accent).
    public var cssHex: String? {
        switch self {
        case .system: return nil
        case .blue: return "#3584e4"
        case .indigo: return "#5b57bd"
        case .purple: return "#9141ac"
        case .pink: return "#d56199"
        case .red: return "#c01c28"
        case .orange: return "#ed5b00"
        case .yellow: return "#c88800"
        case .green: return "#3a944a"
        case .teal: return "#0f9d8f"
        case .mint: return "#43b58c"
        case .cyan: return "#1899b6"
        }
    }
}

/// Shared SCU-side surface/text styling — the colors GTK CSS can't reach
/// (cards, rows, captions painted by the views themselves). Both schemes
/// use ink-over-paper translucency, but light mode needs borders where dark
/// gets away with a faint light wash alone.
public struct SurfaceStyle {
    public let scheme: ColorScheme

    public init(scheme: ColorScheme) { self.scheme = scheme }

    /// Card/row background: a subtle elevation over the window.
    public var card: Color {
        switch scheme {
        case .light: return Color(white: 0.0, opacity: 0.05)
        case .dark: return Color(white: 1.0, opacity: 0.055)
        }
    }

    /// Secondary text (captions, metadata) — GTK's ~55% ink, not flat gray,
    /// which goes muddy on the light background.
    public var text: Color {
        switch scheme {
        case .light: return Color(white: 0.0, opacity: 0.58)
        case .dark: return Color(white: 1.0, opacity: 0.64)
        }
    }
}

extension ThemeController {
    /// `theme.text` at the call sites — secondary text in the active scheme.
    public var text: Color {
        SurfaceStyle(scheme: effectiveScheme).text
    }

    public var surface: SurfaceStyle {
        SurfaceStyle(scheme: effectiveScheme)
    }
}

extension Color {
    /// 0xRRGGBB convenience used by the accent palette.
    init(hex: UInt32) {
        self.init(
            red: Double((hex >> 16) & 0xFF) / 255.0,
            green: Double((hex >> 8) & 0xFF) / 255.0,
            blue: Double(hex & 0xFF) / 255.0
        )
    }
}

/// The display-wide CSS the theme installs: GTK named-color overrides plus a
/// few node rules so the window, editor and entries follow the chosen scheme
/// even where SwiftCrossUI leaves styling to the GTK theme. Pure string
/// building — unit-testable without GTK.
public enum Palette {
    /// Priority between APPLICATION (600) and USER (800): high enough to win
    /// over SwiftCrossUI's per-widget providers, low enough to stay under
    /// genuine user overrides in XDG config.
    public static let providerPriority: UInt32 = 700

    public static func themeCSS(scheme: ColorScheme, accent: AccentChoice) -> String {
        // The COMPLETE standard named-color set, both schemes. Partial
        // overrides lose: any named color left to the system theme (a dark
        // one on COSMIC) keeps its dark panel under this app's dark text —
        // the "black on black, nothing visible" light-mode failure.
        let c: [String: String]
        switch scheme {
        case .light:
            c = [
                "window_bg": "#f6f5f4",
                "window_fg": "rgba(0, 0, 6, 0.85)",
                "view_bg": "#ffffff",
                "view_fg": "rgba(0, 0, 6, 0.85)",
                "headerbar_bg": "#fbfaf9",
                "headerbar_fg": "rgba(0, 0, 6, 0.85)",
                "headerbar_backdrop": "#f2f1f0",
                "sidebar_bg": "#f2f1f0",
                "sidebar_fg": "rgba(0, 0, 6, 0.85)",
                "sidebar_backdrop": "#f6f5f4",
                "card_bg": "#ffffff",
                "card_fg": "rgba(0, 0, 6, 0.85)",
                "popover_bg": "#ffffff",
                "popover_fg": "rgba(0, 0, 6, 0.85)",
                "dialog_bg": "#fafaf9",
                "dialog_fg": "rgba(0, 0, 6, 0.85)",
                "thumb_bg": "#e8e7e6",
                "border": "rgba(0, 0, 6, 0.12)",
            ]
        case .dark:
            c = [
                "window_bg": "#222226",
                "window_fg": "#ffffff",
                "view_bg": "#1d1d20",
                "view_fg": "#ffffff",
                "headerbar_bg": "#2a2a2e",
                "headerbar_fg": "#ffffff",
                "headerbar_backdrop": "#222226",
                "sidebar_bg": "#28282c",
                "sidebar_fg": "#ffffff",
                "sidebar_backdrop": "#222226",
                "card_bg": "#2e2e33",
                "card_fg": "#ffffff",
                "popover_bg": "#323236",
                "popover_fg": "#ffffff",
                "dialog_bg": "#323236",
                "dialog_fg": "#ffffff",
                "thumb_bg": "#3a3a3f",
                "border": "rgba(255, 255, 255, 0.10)",
            ]
        }

        var css = """
            @define-color window_bg_color \(c["window_bg"]!);
            @define-color window_fg_color \(c["window_fg"]!);
            @define-color view_bg_color \(c["view_bg"]!);
            @define-color view_fg_color \(c["view_fg"]!);
            @define-color headerbar_bg_color \(c["headerbar_bg"]!);
            @define-color headerbar_fg_color \(c["headerbar_fg"]!);
            @define-color headerbar_backdrop_color \(c["headerbar_backdrop"]!);
            @define-color sidebar_bg_color \(c["sidebar_bg"]!);
            @define-color sidebar_fg_color \(c["sidebar_fg"]!);
            @define-color sidebar_backdrop_color \(c["sidebar_backdrop"]!);
            @define-color card_bg_color \(c["card_bg"]!);
            @define-color card_fg_color \(c["card_fg"]!);
            @define-color popover_bg_color \(c["popover_bg"]!);
            @define-color popover_fg_color \(c["popover_fg"]!);
            @define-color dialog_bg_color \(c["dialog_bg"]!);
            @define-color dialog_fg_color \(c["dialog_fg"]!);
            @define-color thumbnail_bg_color \(c["thumb_bg"]!);

            window {
                background-color: @window_bg_color;
                color: @window_fg_color;
            }

            /* SCU's Text widgets are GtkLabels; their ink must follow THIS
               palette, not whatever scheme the GTK theme reports to the
               framework — the "white text on white background" failure. */
            label {
                color: @window_fg_color;
            }

            textview, textview text {
                background-color: @view_bg_color;
                color: @view_fg_color;
            }

            entry {
                background-color: @view_bg_color;
                color: @window_fg_color;
                border: 1px solid \(c["border"]!);
                border-radius: 6px;
            }

            entry:focus-within {
                border-color: @accent_bg_color;
            }

            scrolledwindow > viewport {
                background-color: @window_bg_color;
            }

            progressbar trough {
                background-color: \(c["thumb_bg"]!);
            }

            button:hover {
                filter: brightness(0.95);
            }

            button:active {
                filter: brightness(0.90);
            }

            """
        if let accentHex = accent.cssHex {
            css += """

                @define-color accent_bg_color \(accentHex);
                @define-color accent_color \(accentHex);

                switch:checked {
                    background-color: @accent_bg_color;
                }

                checkbutton:checked {
                    color: @accent_bg_color;
                }
                """
        }
        return css
    }
}
