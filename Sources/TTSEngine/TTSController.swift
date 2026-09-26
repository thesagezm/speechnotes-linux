import Foundation
import SwiftCrossUI
import Data
import Log

/// MainActor facade over the TTS pipeline: owns the current engine run and
/// publishes state + read-along position for the UI. One run at a time —
/// `play` replaces whatever is playing (dsnote's queue, trimmed to what a
/// notes app needs).
///
/// Bookmarks: while speaking, the audible position is throttled into
/// BookmarkStore (note:<uuid>) so a stopped run can resume where it left
/// off; a run that plays to the end clears its own bookmark.
@MainActor
public final class TTSController: ObservableObject {
    public static let shared = TTSController()

    @Published public private(set) var state: TTSState = .idle
    @Published public private(set) var position: PlaybackPosition?
    /// The sentence currently sounding — the read-along strip's text.
    @Published public private(set) var currentSentence: String?
    @Published public private(set) var lastError: String?

    /// Fresh flags per run: a replaced run keeps its own stopped instance,
    /// so resetting a new run can never resurrect the old worker.
    private var flags = ControlFlags()
    private var worker: Task<Void, Never>?
    private var playingNoteId: UUID?

    private init() {}

    public var isBusy: Bool { state == .speaking || state == .paused }

    /// True while `noteId` is the note this controller is working on — the
    /// list rows use it to render the playing state on the right row.
    public func isPlaying(noteId: UUID) -> Bool {
        isBusy && playingNoteId == noteId
    }

    /// Speaks a whole note (or resumes it from a UTF-16 offset). Soft-fails:
    /// engine unavailable → log + idle, the UI is never worse than silence.
    public func play(
        note: Note,
        engineKind: EngineKind,
        speed: Float,
        voice: String,
        resumeFromUTF16: Int = 0
    ) {
        stop()
        let chunks = TTSChunker.planChunks(
            noteId: note.id,
            text: note.text,
            resumeFromUTF16: resumeFromUTF16
        )
        guard !chunks.isEmpty else { return }

        let engine: TTSEngineBase
        do {
            engine = try EngineFactory.make(kind: engineKind)
        } catch {
            lastError = "\(engineKind.displayName): \(error)"
            Log.error("TTS: \(lastError ?? "?")")
            return
        }
        if !engine.modelCreated() {
            lastError = "\(engineKind.displayName) is not ready — no model (Settings → download a voice)"
            Log.error("TTS: \(lastError ?? "?")")
            return
        }
        if let espeak = engine as? EspeakEngine {
            espeak.voice = voice
        }

        // Cached chunk WAVs are keyed by index; a resume renumbers chunks, so
        // start every run from a clean cache (synthesis is cheap on this tier).
        TTSChunker.clearCache(noteId: note.id)

        let flags = ControlFlags()
        self.flags = flags
        playingNoteId = note.id
        state = .speaking
        position = nil
        currentSentence = nil
        let noteId = note.id
        worker = Task.detached(priority: .userInitiated) { [weak self] in
            await self?.runPipeline(
                chunks: chunks,
                engine: engine,
                flags: flags,
                noteId: noteId,
                speed: speed
            )
        }
    }

    public func stop() {
        flags.stop()
        worker?.cancel()
        worker = nil
        // A manual stop keeps the position — that's what Resume plays from.
        if let pos = position {
            BookmarkStore.shared.set(
                BookmarkStore.noteKey(pos.noteId),
                PlaybackBookmark(noteId: pos.noteId, textOffset: pos.textOffset)
            )
        }
        state = .idle
        position = nil
        currentSentence = nil
        playingNoteId = nil
    }

    public func pause() {
        guard state == .speaking else { return }
        flags.setPaused(true)
        state = .paused
    }

    public func resume() {
        guard state == .paused else { return }
        flags.setPaused(false)
        state = .speaking
    }

    /// The worker body. Encode → WAV → play, chunk by chunk, checking the
    /// flags between every unit. A chunk that fails gets one attempt, a log
    /// line, and a skip (the TTS error philosophy).
    private nonisolated func runPipeline(
        chunks: [SpeechChunk],
        engine: TTSEngineBase,
        flags: ControlFlags,
        noteId: UUID,
        speed: Float
    ) async {
        let player = TTSPlayer()
        defer { player.close() }

        for chunk in chunks {
            if flags.isStopped { break }
            let outFile = TTSChunker.cacheFile(noteId: noteId, index: chunk.index)
            do {
                _ = try engine.encodeSpeechImpl(
                    text: chunk.text,
                    speed: speed,
                    outFile: outFile,
                    abort: { flags.isStopped }
                )
                if flags.isStopped { break }

                await self.setSentence(chunk.text)
                let (samples, rate) = try WAVFile.read(at: outFile)
                try player.play(
                    samples: samples,
                    sampleRate: rate,
                    chunk: chunk,
                    chunkCount: chunks.count,
                    flags: flags,
                    onPosition: { [weak self] offset, fraction in
                        Task { @MainActor [weak self] in
                            self?.position = PlaybackPosition(
                                noteId: noteId,
                                chunkIndex: chunk.index,
                                textOffset: offset,
                                fraction: fraction
                            )
                            // BookmarkStore throttles to 1 s with a
                            // guaranteed trailing write — safe to call
                            // on every tick.
                            BookmarkStore.shared.set(
                                BookmarkStore.noteKey(noteId),
                                PlaybackBookmark(noteId: noteId, textOffset: offset)
                            )
                        }
                    }
                )
            } catch {
                Log.error("TTS chunk \(chunk.index): \(error)")
                continue
            }
        }

        await self.pipelineDone(noteId: noteId, clean: !flags.isStopped)
    }

    private func setSentence(_ text: String) {
        currentSentence = text
    }

    /// Natural end of the queue. Guards on the note id so a replaced worker
    /// limping home can't clobber a NEW run's state or bookmark.
    private func pipelineDone(noteId: UUID, clean: Bool) {
        guard playingNoteId == noteId else { return }
        if clean {
            // Played to the end — the resume bookmark served its purpose.
            BookmarkStore.shared.removeAll(forNote: noteId)
        }
        state = .idle
        position = nil
        currentSentence = nil
        playingNoteId = nil
    }
}
