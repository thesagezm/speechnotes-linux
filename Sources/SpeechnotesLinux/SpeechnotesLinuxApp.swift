import Foundation
import SwiftCrossUI
import GtkBackend
import AppPaths
import Log

/// Speechnotes-Linux — first window. Proves the SwiftCrossUI/GTK4 stack:
/// native GTK4 widgets, state binding, a button, and the app scaffold the
/// rest of the phases build on.
@main
struct SpeechnotesLinuxApp: App {
    @State private var launchedAt = Date()
    @State private var buttonPresses = 0
    @State private var statusLine = "SwiftCrossUI window is live."

    init() {
        if let problem = AppPaths.ensureDirectories() {
            Log.error("AppPaths: \(problem)")
        } else {
            Log.info("Speechnotes Linux starting; data dir: \(AppPaths.dataDir.path)")
        }
    }

    var body: some Scene {
        WindowGroup("Speechnotes Linux") {
            VStack(spacing: 12) {
                Text("Speechnotes Linux")
                    .font(.title)
                Text(statusLine)
                    .font(.body)
                Text("Launched at \(launchedAt.formatted(date: .abbreviated, time: .standard))")
                    .font(.caption)
                    .foregroundColor(.gray)
                Button("Press me (\(buttonPresses))") {
                    buttonPresses += 1
                    statusLine = "Buttons pressed: \(buttonPresses)"
                }
            }
            .padding(24)
            .frame(minWidth: 480, minHeight: 320)
        }
        .defaultSize(width: 640, height: 480)
    }
}
