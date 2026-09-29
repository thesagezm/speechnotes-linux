import XCTest
@testable import Appearance

/// The palette and accent mapping are pure — no GTK needed. The controller's
/// GTK plumbing is deliberately not exercised here (no display in CI).
final class PaletteTests: XCTestCase {
    func testLightAndDarkCSSDifferAndCoverBasics() {
        let light = Palette.themeCSS(scheme: .light, accent: .blue)
        let dark = Palette.themeCSS(scheme: .dark, accent: .blue)

        // The window background and text view rules are the load-bearing
        // part of forced dark mode: without them the theme's white window
        // shows behind SCU's dark controls.
        for css in [light, dark] {
            XCTAssertTrue(css.contains("window {"), "window rule missing")
            XCTAssertTrue(css.contains("textview text {"), "textview rule missing")
            XCTAssertTrue(css.contains("@define-color window_bg_color"))
            XCTAssertTrue(css.contains("@define-color accent_bg_color #3584e4"))
            XCTAssertTrue(css.contains("button:hover"), "hover feedback missing")
        }
        XCTAssertTrue(light.contains("#fafafb"))
        XCTAssertTrue(dark.contains("#222226"))
        XCTAssertNotEqual(light, dark)
    }

    func testSystemAccentLeavesThemeAccentAlone() {
        let css = Palette.themeCSS(scheme: .light, accent: .system)
        XCTAssertFalse(css.contains("accent_bg_color #"), "system accent must not override the theme's")
    }

    func testAccentChoicesMirroriOSPalette() {
        // Same 12 choices, same order, as the iOS AccentColorChoice.
        let names = AccentChoice.allCases.map(\.displayName)
        XCTAssertEqual(
            names,
            ["System", "Blue", "Indigo", "Purple", "Pink", "Red", "Orange",
             "Yellow", "Green", "Teal", "Mint", "Cyan"]
        )
        // Every non-system choice has a usable hex in both worlds.
        for choice in AccentChoice.allCases {
            XCTAssertFalse(choice.displayName.isEmpty)
            XCTAssertEqual(choice.cssHex == nil, choice == .system)
            XCTAssertFalse(choice.rawValue.isEmpty)
        }
    }

    func testUnknownPrefValuesFallBackSensibly() {
        // The controller maps unknown appearance strings to system detection;
        // accent strings to the default. Just pin the raw-value lookups.
        XCTAssertNil(AccentChoice(rawValue: "chartreuse"))
        XCTAssertEqual(AccentChoice(rawValue: "blue"), .blue)
    }
}
