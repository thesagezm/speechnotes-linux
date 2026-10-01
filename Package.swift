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
        .executable(name: "theme-smoke", targets: ["ThemeSmoke"]),
        .executable(name: "alsa-tone", targets: ["AlsaTone"]),
        .executable(name: "espeak-say", targets: ["EspeakSay"]),
        .executable(name: "tts-smoke", targets: ["TtsSmoke"])
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
        // ONNX Runtime C API — the escape hatch for piper/kokoro/supertonic.
        // Vendored 1.30.0 header; the shared library comes from
        // ~/.local/lib locally and /usr/local/lib on CI.
        .systemLibrary(
            name: "COnnxRuntime",
            path: "Sources/COnnxRuntime"
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
        // Theme + accent + display-wide CSS. GTK C interop is isolated here;
        // depends on SwiftCrossUI for ObservableObject/ColorScheme on Linux.
        .target(
            name: "Appearance",
            dependencies: ["CGTK4", "Log", .product(name: "SwiftCrossUI", package: "swift-cross-ui")]
        ),
        // Shared leveled logger.
        .target(
            name: "BookDrop",
            dependencies: ["Log", .product(name: "SwiftCrossUI", package: "swift-cross-ui")]
        ),
        // The keyboard binding table and keysym matcher. Pure Swift with no
        // framework dependencies, so the half of the keyboard layer that
        // decides "is this keypress ours" is testable on its own.
        .target(
            name: "Shortcuts"
        ),
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
        // Diagnostic: theme CSS + settings plumbing without a window.
        .executableTarget(
            name: "ThemeSmoke",
            dependencies: ["CGTK4", "Appearance"]
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
        // TTS tier diagnostic: one sentence through every engine, headless.
        .executableTarget(
            name: "TtsSmoke",
            dependencies: ["TTSEngine"],
            linkerSettings: [.unsafeFlags(["-L" + homeLibDir])]
        ),
        // The app.
        .executableTarget(
            name: "SpeechnotesLinux",
            dependencies: [
                "BookDrop",
                "Shortcuts",
                "SpeechLogic",
                "Data",
                "TTSEngine",
                "Appearance",
                "CALSA",
                "AlsaSink",
                "Log",
                "AppPaths",
                .product(name: "SwiftCrossUI", package: "swift-cross-ui"),
                .product(name: "GtkBackend", package: "swift-cross-ui", condition: .when(platforms: [.linux]))
            ],
            // espeak-ng resolves through the user-local symlink the distro's
            // runtime-only package forces us into; onnxruntime lives there
            // too (loader needs the rpath — it's not on the default path).
            // CI's -dev/runtime installs sit on the default search path,
            // where a missing -L dir is harmless.
            linkerSettings: [
                .unsafeFlags([
                    "-L" + homeLibDir,
                    "-Xlinker", "-rpath", "-Xlinker", homeLibDir,
                ])
            ]
        ),
        // TTS tier: dsnote engine contract, eSpeak tier, ALSA player.
        .target(
            name: "TTSEngine",
            dependencies: [
                "Data",
                "SpeechLogic",
                "EspeakBridge",
                "COnnxRuntime",
                "AlsaSink",
                "AppPaths",
                "Log",
                .product(name: "SwiftCrossUI", package: "swift-cross-ui")
            ]
        ),
        .testTarget(
            name: "TTSEngineTests",
            dependencies: ["TTSEngine", "AppPaths", "Data"],
            linkerSettings: [
                .unsafeFlags([
                    "-L" + homeLibDir,
                    "-Xlinker", "-rpath", "-Xlinker", homeLibDir,
                ])
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
            dependencies: ["Data", "AppPaths", "SpeechLogic"],
            resources: [
                .copy("Fixtures")
            ]
        ),
        .testTarget(
            name: "AppearanceTests",
            dependencies: ["Appearance"]
        ),
        .testTarget(
            name: "BookDropTests",
            dependencies: ["BookDrop", "Log"]
        ),
        .testTarget(
            name: "ShortcutsTests",
            dependencies: ["Shortcuts"]
        )
    ]
)
