// swift-tools-version: 6.0

import Foundation
import PackageDescription

/// The dev machine has no espeak-ng -dev package and no root, so bootstrap.sh
/// drops a linker symlink in the user's home.
let homeLibDir = FileManager.default.homeDirectoryForCurrentUser
    .appendingPathComponent(".local/lib").path

let package = Package(
    name: "speechnotes-linux",
    platforms: [
        .macOS(.v13)
    ],
    products: [
        .executable(name: "speechnotes-linux", targets: ["SpeechnotesLinux"]),
        .executable(name: "gtk-smoke", targets: ["GTKSmoke"]),
        .executable(name: "alsa-tone", targets: ["AlsaTone"]),
        .executable(name: "espeak-say", targets: ["EspeakSay"])
    ],
    dependencies: [
        .package(url: "https://github.com/stackotter/swift-cross-ui.git", from: "0.9.0")
    ],
    targets: [
        // C-library interop shims.
        .systemLibrary(
            name: "CALSA",
            path: "Sources/CALSA",
            pkgConfig: "alsa"
        ),
        .systemLibrary(
            name: "CZlib",
            path: "Sources/CZlib",
            pkgConfig: "zlib"
        ),
        // espeak-ng: the system shared library + vendored matching headers.
        // The manifest can't take linkerSettings, so the executable target
        // that depends on the bridge adds the search path instead.
        .systemLibrary(
            name: "CEspeakNG",
            path: "Sources/CEspeakNG"
        ),
        // Swift bridge over libespeak-ng.
        .target(
            name: "EspeakBridge",
            dependencies: ["CEspeakNG"]
        ),
        .systemLibrary(
            name: "CGTK4",
            path: "Sources/CGTK4",
            pkgConfig: "gtk4"
        ),
        // ALSA playback sink.
        .target(
            name: "AlsaSink",
            dependencies: ["CALSA", "Log"]
        ),
        // Shared leveled logger.
        .target(
            name: "Log",
            dependencies: ["AppPaths"]
        ),        // XDG paths.
        .target(
            name: "AppPaths"
        ),
        // Pure logic ported from speechnotes-ios' SpeechLogic package.
        .target(
            name: "SpeechLogic",
            dependencies: ["CZlib"]
        ),
        .testTarget(
            name: "SpeechLogicTests",
            dependencies: ["SpeechLogic"],
            path: "Tests/SpeechLogicTests",
            resources: [
                .copy("Fixtures")
            ]
        ),
        // Phase-0 diagnostic: a raw GTK4 window straight from the C API.
        .executableTarget(
            name: "GTKSmoke",
            dependencies: ["CGTK4"]
        ),
        // Phase-0 diagnostic: a 440 Hz tone through the ALSA sink.
        .executableTarget(
            name: "AlsaTone",
            dependencies: ["AlsaSink", "Log"]
        ),
        // Phase-0 diagnostic: one sentence spoken through espeak-ng.
        .executableTarget(
            name: "EspeakSay",
            dependencies: ["EspeakBridge", "AlsaSink"],
            linkerSettings: [.unsafeFlags(["-L" + homeLibDir])]
        ),
        // The app.
        .executableTarget(
            name: "SpeechnotesLinux",
            dependencies: [
                "SpeechLogic",
                "Data",
                "CALSA",
                "AlsaSink",
                "Log",
                "AppPaths",
                .product(name: "SwiftCrossUI", package: "swift-cross-ui"),
                .product(name: "GtkBackend", package: "swift-cross-ui", condition: .when(platforms: [.linux]))
            ]
        ),
        // Note/notebook/prefs stores, ported from the iOS app (Phase 2).
        // Depends on SwiftCrossUI because ObservableObject/@Published live
        // there on Linux (there is no Combine).
        .target(
            name: "Data",
            dependencies: [
                "AppPaths",
                "SpeechLogic",
                "Log",
                .product(name: "SwiftCrossUI", package: "swift-cross-ui")
            ]
        ),
        .testTarget(
            name: "DataTests",
            dependencies: ["Data", "AppPaths"]
        )
    ]
)
