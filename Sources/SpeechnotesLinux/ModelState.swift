import Foundation
import AppPaths
import TTSEngine

/// A snapshot of what is installed under modelsDir, taken once per process
/// and refreshed on demand.
///
/// The settings pane asks "is this model installed?" on every render, and
/// every one of those answers was a fresh `stat` sweep — including
/// SupertonicModelManager.installedVoices(), which stats a style file per
/// voice, and KokoroModelManager.modelFilesAreValid(), which stats three
/// files and then lists the voices directory *again*. On a warm page that is
/// a dozen syscalls a frame, on a page GTK is already relaying three times
/// per click.
///
/// Nothing here is allowed to go stale silently, so there is exactly one
/// invalidation point: the download handler calls ``invalidate()`` when a
/// model finishes landing.
enum ModelState {
    struct Piper {
        var installed: Bool = false
        var subtitle: String = ""
    }
    struct Kokoro {
        var installed: Bool = false
        var subtitle: String = ""
    }
    struct Supertonic {
        var installed: Bool = false
        var subtitle: String = ""
    }

    struct Snapshot {
        var piper = Piper()
        var kokoro = Kokoro()
        var supertonic = Supertonic()
    }

    private static let lock = NSLock()
    nonisolated(unsafe) private static var cached: Snapshot?

    /// The per-process snapshot, built on first use.
    static func current() -> Snapshot {
        lock.lock()
        defer { lock.unlock() }
        if let cached { return cached }
        let built = Snapshot(
            piper: Piper(
                installed: !PiperModelManager.installedVoices().isEmpty,
                subtitle: "\(PiperModelManager.curatedVoice), ~63 MB"
            ),
            kokoro: Kokoro(
                installed: KokoroModelManager.modelFilesAreValid(),
                subtitle: "82M uint8 tier, ~190 MB"
            ),
            supertonic: Supertonic(
                installed: SupertonicModelManager.installedVoices().isEmpty == false,
                subtitle: "\(SupertonicModelManager.curatedVoice), full set ~260 MB"
            )
        )
        cached = built
        return built
    }

    /// Called after a model download completes (and from the settings pane
    /// when the user hits Refresh, if one is ever added).
    static func invalidate() {
        lock.lock()
        cached = nil
        lock.unlock()
        // The voice picker's own list is derived from the same directories,
        // so it has to be dropped too or a newly installed voice would not
        // appear until the next launch.
        VoiceCatalog.invalidateCache()
    }

    /// Whether the models directory exists at all — used by the About pane
    /// and cheap enough not to cache.
    static var modelsDirectoryExists: Bool {
        FileManager.default.fileExists(atPath: AppPaths.modelsDir.path)
    }
}
