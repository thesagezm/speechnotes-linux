import CGTK4
import Foundation
import Appearance

/// Theme diagnostic: proves the GValue/property plumbing and the display CSS
/// without opening a window — useful because a second app instance can't map
/// while the user's instance owns the GTK application id.
///
/// Pointer idiom matches GTKSmoke: GTK typedefs import as distinct Swift
/// types; here the settings API is called through Appearance's controller.
guard gtk_init_check() != 0 else {
    FileHandle.standardError.write("theme-smoke: no display available\n".data(using: .utf8)!)
    exit(2)
}

let darkRoundTrip = MainActor.assumeIsolated {
    let theme = ThemeController.shared
    // Forced dark must flip the GTK prefer-dark flag; reading it back through
    // the detection path closes the loop on both GValue directions.
    theme.sync(appearance: "dark", accentChoice: "teal")
    let darkSeen = ThemeController.systemScheme() == .dark
    theme.sync(appearance: "light", accentChoice: "system")
    let seenAfterLight = ThemeController.systemScheme()
    FileHandle.standardError.write(
        "theme-smoke: dark round-trip=\(darkSeen), after forced light detection=\(seenAfterLight)\n"
            .data(using: .utf8)!)
    return darkSeen
}

exit(darkRoundTrip ? 0 : 1)
