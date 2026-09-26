import Foundation
import EspeakBridge
import Log

/// The instant tier: eSpeak-NG through the retrieval-mode bridge. No model
/// files, ~131 voices, synthesis in tens of milliseconds per sentence —
/// the always-available fallback the whole tier degrades to.
public final class EspeakEngine: TTSEngineBase, @unchecked Sendable {
    private let bridge = EspeakBridge()
    private var initialized = false
    private var appliedVoice = ""

    /// espeak voice name ("" = library default).
    public var voice: String = ""

    public override func modelCreated() -> Bool {
        do { try ensureInitialized(); return true } catch { return false }
    }

    public override func modelSupportsSpeed() -> Bool { true }

    public override func createModel(modelPath: String, modelId: String) -> Bool {
        do { try ensureInitialized(); return true } catch { return false }
    }

    @discardableResult
    public override func encodeSpeechImpl(
        text: String,
        speed: Float,
        outFile: URL,
        abort: @escaping @Sendable () -> Bool
    ) throws -> Int {
        try ensureInitialized()

        // ~170 wpm reads naturally at 1.0×; espeak's own range is 80…450.
        let wpm = Int((170.0 * Double(speed)).rounded())
        bridge.setRate(max(80, min(450, wpm)))
        if voice != appliedVoice {
            if !voice.isEmpty {
                _ = bridge.setVoice(voice)
            }
            appliedVoice = voice
        }

        let samples = try bridge.synthesize(text, abort: abort)
        guard !abort() else { return bridge.sampleRate }
        guard !samples.isEmpty else {
            throw TTSError.synthesisFailed("espeak produced no audio")
        }
        try WAVFile.write(samples: samples, sampleRate: bridge.sampleRate, to: outFile)
        return bridge.sampleRate
    }

    private func ensureInitialized() throws {
        guard !initialized else { return }
        do {
            try bridge.initialize()
            initialized = true
            Log.info("EspeakEngine ready at \(bridge.sampleRate) Hz, \(bridge.listVoices().count) voices")
        } catch {
            Log.error("EspeakEngine init failed: \(error)")
            throw error
        }
    }
}
