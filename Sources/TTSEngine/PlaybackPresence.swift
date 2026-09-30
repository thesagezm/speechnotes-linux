import Foundation
import SwiftCrossUI

/// A deliberately tiny signal the app shell can observe without subscribing
/// to the playback controllers' high-frequency position updates. The
/// controllers flip their active flag only on state transitions
/// (start/stop/finish — never on position ticks), so the shell re-renders a
/// handful of times per session instead of 20 times per second. Every tick
/// used to re-measure the whole shell and the active pane through
/// AppShell's controller observations.
@MainActor
public final class PlaybackPresence: ObservableObject {
    public static let shared = PlaybackPresence()

    @Published public private(set) var isVisible = false

    private var ttsActive = false
    private var audioActive = false

    private init() {}

    func setTTSActive(_ active: Bool) {
        ttsActive = active
        sync()
    }

    func setAudioActive(_ active: Bool) {
        audioActive = active
        sync()
    }

    private func sync() {
        let visible = ttsActive || audioActive
        if visible != isVisible {
            isVisible = visible
        }
    }
}
