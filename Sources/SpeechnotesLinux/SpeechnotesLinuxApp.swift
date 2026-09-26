import Foundation
import SwiftCrossUI
import GtkBackend
import AppPaths
import Log

/// Speechnotes-Linux app entry. The window shell lives in AppShell.
@main
struct SpeechnotesLinuxApp: App {
    init() {
        if let problem = AppPaths.ensureDirectories() {
            Log.error("AppPaths: \(problem)")
        } else {
            Log.info("Speechnotes Linux starting; data dir: \(AppPaths.dataDir.path)")
        }
    }

    var body: some Scene {
        WindowGroup("Speechnotes Linux") {
            AppShell()
        }
        .defaultSize(width: 1150, height: 720)
    }
}
