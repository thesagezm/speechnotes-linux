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
        let (windowBg, windowFg, viewBg, viewFg): (String, String, String, String)
        switch scheme {
        case .light:
            // GTK4 "Default" theme light values.
            (windowBg, windowFg, viewBg, viewFg)
                = ("#fafafb", "rgba(0, 0, 6, 0.8)", "#ffffff", "rgba(0, 0, 6, 0.8)")
        case .dark:
            (windowBg, windowFg, viewBg, viewFg)
                = ("#222226", "#ffffff", "#1d1d20", "#ffffff")
        }

        var css = """
            @define-color window_bg_color \(windowBg);
            @define-color window_fg_color \(windowFg);
            @define-color view_bg_color \(viewBg);
            @define-color view_fg_color \(viewFg);

            window {
                background-color: @window_bg_color;
                color: @window_fg_color;
            }

            textview text {
                color: @view_fg_color;
            }

            entry text {
                color: @window_fg_color;
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
