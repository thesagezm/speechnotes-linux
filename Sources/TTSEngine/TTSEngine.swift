import Foundation
import SpeechLogic
import AppPaths
import Log

/// Engine tier selector — raw values match `Prefs.engineKind` exactly (the
/// iOS app's UserDefaults vocabulary).
public enum EngineKind: String, Sendable {
    case espeak
    case pico
    case piper
    case kokoro
    case supertonic

    public var displayName: String {
        switch self {
        case .espeak: return "eSpeak-NG"
        case .pico: return "Pico2Wave"
        case .piper: return "Piper"
        case .kokoro: return "Kokoro"
        case .supertonic: return "Supertonic"
        }
    }
}

/// Engine lifecycle, mirroring dsnote's state machine.
public enum TTSState: Equatable, Sendable {
    case idle
    case loading
    case speaking
    case paused
}

/// Where playback is right now — the read-along clock. `textOffset` is a
/// UTF-16 offset into the note (SentenceChunker's coordinate space);
/// `fraction` is rough progress through the whole note for the slider.
public struct PlaybackPosition: Equatable, Sendable {
    public let noteId: UUID
    public let chunkIndex: Int
    public let textOffset: Int
    public let fraction: Double

    public init(noteId: UUID, chunkIndex: Int, textOffset: Int, fraction: Double) {
        self.noteId = noteId
        self.chunkIndex = chunkIndex
        self.textOffset = textOffset
        self.fraction = fraction
    }
}

public enum TTSError: Error, Equatable {
    case engineNotAvailable(EngineKind)
    case synthesisFailed(String)
}

/// Stop/pause signals shared between the UI thread and the worker/player.
/// Plain NSLock flag set: both sides poll between buffers, so no condition
/// variables or async plumbing are needed for control flow.
public final class ControlFlags: @unchecked Sendable {
    private let lock = NSLock()
    private var stopped = false
    private var paused = false

    public init() {}

    public func reset() {
        lock.lock(); stopped = false; paused = false; lock.unlock()
    }

    public func stop() {
        lock.lock(); stopped = true; paused = false; lock.unlock()
    }

    public var isStopped: Bool {
        lock.lock(); defer { lock.unlock() }
        return stopped
    }

    public var isPaused: Bool {
        lock.lock(); defer { lock.unlock() }
        return paused
    }

    public func setPaused(_ value: Bool) {
        lock.lock(); paused = value; lock.unlock()
    }
}

/// One enqueued unit of speech: a sentence-range piece of a note, carrying
/// its UTF-16 coordinates so playback can drive the read-along highlight.
public struct SpeechChunk: Sendable {
    public let noteId: UUID
    public let index: Int
    public let textUTF16Offset: Int
    public let textUTF16Length: Int
    public let text: String
}

/// dsnote's four-virtual engine contract, Swift edition. The base owns the
/// pipeline (chunk splitting, WAV handoff, queue); subclasses only answer
/// how to produce PCM for one chunk:
///
///   - `modelCreated()` — is a model loaded and ready?
///   - `modelSupportsSpeed()` — does the engine honor the rate multiplier?
///   - `createModel(modelPath:modelId:)` — load a model (file engines).
///   - `encodeSpeechImpl(text:speed:outFile:abort:)` — synthesize one chunk
///     and write it to `outFile` as 16-bit mono WAV.
///
/// Concurrency: one engine instance belongs to exactly one worker task at a
/// time (enforced by the controller handing it over), so the unchecked
/// Sendable is a confinement promise, not a data-race waiver.
public class TTSEngineBase: @unchecked Sendable {
    public init() {}

    public func modelCreated() -> Bool { true }

    public func modelSupportsSpeed() -> Bool { true }

    @discardableResult
    public func createModel(modelPath: String, modelId: String) -> Bool { true }

    /// Synthesizes `text` and writes a WAV to `outFile`. Returns the sample
    /// rate of the produced audio. `abort` is polled by the engine mid-
    /// synthesis; a true return means "give up, the queue was stopped".
    @discardableResult
    public func encodeSpeechImpl(
        text: String,
        speed: Float,
        outFile: URL,
        abort: @escaping @Sendable () -> Bool
    ) throws -> Int {
        throw TTSError.synthesisFailed("engine has no encodeSpeechImpl")
    }
}

/// Builds engines for the tier. Later phases register pico/piper/kokoro/
/// supertonic here; asking for a missing engine is a soft error the UI
/// surfaces as a log line (never a crash — TTS must never be worse than
/// silence).
public enum EngineFactory {
    public static func make(kind: EngineKind) throws -> TTSEngineBase {
        switch kind {
        case .espeak: return EspeakEngine()
        case .pico, .piper, .kokoro, .supertonic:
            throw TTSError.engineNotAvailable(kind)
        }
    }
}

/// Pure helpers shared by the controller and tests.
public enum TTSChunker {
    /// Splits `text` into sentence-bounded chunks (SentenceChunker, UTF-16
    /// coordinates preserved for the read-along clock). `resumeFromUTF16`
    /// drops everything before that offset and trims the straddling chunk —
    /// resuming playback mid-note starts at the containing sentence.
    public static func planChunks(
        noteId: UUID,
        text: String,
        maxChars: Int = 300,
        resumeFromUTF16: Int = 0
    ) -> [SpeechChunk] {
        let pieces = SentenceChunker.sentencePieces(in: text, maxChars: maxChars)
        guard !pieces.isEmpty else { return [] }
        let units = Array(text.utf16)
        var chunks: [SpeechChunk] = []
        for piece in pieces {
            let end = min(piece.endOffset, units.count)
            guard end > resumeFromUTF16 else { continue }  // fully before resume
            let start = max(piece.offset, resumeFromUTF16)
            let body = String(decoding: units[start..<end], as: UTF16.self)
            guard !body.isEmpty else { continue }
            chunks.append(
                SpeechChunk(
                    noteId: noteId,
                    index: chunks.count,  // renumbered — cache is cleared per run
                    textUTF16Offset: start,
                    textUTF16Length: end - start,
                    text: body
                )
            )
        }
        return chunks
    }

    /// Where a chunk's WAV lives in the cache. Regenerating overwrites —
    /// chunks are pure functions of (text, voice, speed).
    public static func cacheFile(noteId: UUID, index: Int) -> URL {
        AppPaths.cacheDir
            .appendingPathComponent("tts", isDirectory: true)
            .appendingPathComponent(noteId.uuidString, isDirectory: true)
            .appendingPathComponent(String(format: "%04d.wav", index))
    }

    /// Drops a note's cached chunk WAVs (called when its text changes or the
    /// note is purged).
    public static func clearCache(noteId: UUID) {
        let dir = AppPaths.cacheDir
            .appendingPathComponent("tts", isDirectory: true)
            .appendingPathComponent(noteId.uuidString, isDirectory: true)
        try? FileManager.default.removeItem(at: dir)
    }
}
