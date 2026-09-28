#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
import Foundation
import AppPaths
import Log

/// Supertonic (supertone) — 4-graph ONNX pipeline (duration predictor,
/// text encoder, vector estimator, vocoder) driven by the vendored
/// `SupertonicHelper` port. 44.1 kHz output, ~262 MB of models, English-only
/// (v1) with 10 preset voice styles.
public final class SupertonicEngine: TTSEngineBase, @unchecked Sendable {
    /// The reference examples denoise in 5 steps — the estimator is a
    /// consistency model built for exactly this few-step regime.
    private static let totalStep = 5

    private var tts: TextToSpeech?
    private var style: Style?

    /// Supertonic v1 speaks English; the note text is passed through as-is.
    public var lang = "en"

    public override func modelCreated() -> Bool { tts != nil }

    public override func modelSupportsSpeed() -> Bool { true }

    /// `modelPath` is the onnx/ directory (sessions + tts.json +
    /// unicode_indexer.json); `modelId` is a voice style name ("M1").
    public override func createModel(modelPath: String, modelId: String) -> Bool {
        do {
            let loaded = try loadTextToSpeech(modelPath, false)
            let stylePath = SupertonicModelManager.stylePath(for: modelId)
                ?? URL(fileURLWithPath: modelPath)
                    .deletingLastPathComponent()
                    .appendingPathComponent("voice_styles/\(modelId).json").path
            let loadedStyle = try loadVoiceStyle([stylePath], verbose: false)
            tts = loaded
            style = loadedStyle
            Log.info("SupertonicEngine ready: \(modelPath), voice \(modelId), \(loaded.sampleRate) Hz")
            return true
        } catch {
            Log.error("SupertonicEngine load failed: \(error)")
            tts = nil
            style = nil
            return false
        }
    }

    @discardableResult
    public override func encodeSpeechImpl(
        text: String,
        speed: Float,
        outFile: URL,
        abort: @escaping @Sendable () -> Bool
    ) throws -> Int {
        guard let tts, let style else {
            throw TTSError.synthesisFailed("Supertonic model not loaded")
        }
        guard !abort() else { return tts.sampleRate }

        // Extreme speed values explode the duration predictor's latent
        // length; clamp to the sane range the UI slider produces.
        let clampedSpeed = max(0.5, min(3.0, speed))
        let (wav, _) = try tts.call(
            text, lang, style, Self.totalStep, speed: clampedSpeed
        )
        guard !abort() else { return tts.sampleRate }
        guard !wav.isEmpty else {
            throw TTSError.synthesisFailed("model produced no audio")
        }

        let samples = wav.map { w -> Int16 in
            Int16((max(-1.0, min(1.0, Double(w))) * 32767.0).rounded())
        }
        try WAVFile.write(samples: samples, sampleRate: tts.sampleRate, to: outFile)
        return tts.sampleRate
    }
}

/// Download/discovery for the Supertonic asset set under
/// modelsDir/supertonic/: the four ONNX graphs + tts.json +
/// unicode_indexer.json in onnx/, and the ten preset voice styles in
/// voice_styles/. Files come from supertone's HuggingFace mirror of the
/// (archived) supertonic v1 release; weights are OpenRAIL-M.
public enum SupertonicModelManager {
    public static let baseURL = "https://huggingface.co/supertone-oss-archive/supertonic/resolve/main"
    public static let curatedVoice = "M1"
    public static let voices = ["F1", "F2", "F3", "F4", "F5", "M1", "M2", "M3", "M4", "M5"]

    public static var modelDirectory: URL {
        AppPaths.modelsDir.appendingPathComponent("supertonic", isDirectory: true)
    }

    /// The directory `loadTextToSpeech` reads: sessions + tts.json +
    /// unicode_indexer.json.
    public static var onnxDirectory: URL {
        modelDirectory.appendingPathComponent("onnx", isDirectory: true)
    }

    public static func stylePath(for voice: String) -> String? {
        let url = modelDirectory
            .appendingPathComponent("voice_styles", isDirectory: true)
            .appendingPathComponent("\(voice).json")
        return FileManager.default.fileExists(atPath: url.path) ? url.path : nil
    }

    public static func installedVoices() -> [String] {
        voices.filter { stylePath(for: $0) != nil }
    }

    /// True when the four graphs + config + indexer are all present at
    /// plausible sizes.
    public static func modelFilesAreValid() -> Bool {
        let fm = FileManager.default
        let dir = onnxDirectory
        func size(_ name: String) -> Int64 {
            (try? fm.attributesOfItem(atPath: dir.appendingPathComponent(name).path))?[.size] as? Int64 ?? 0
        }
        return size("duration_predictor.onnx") > 1_000_000
            && size("text_encoder.onnx") > 20_000_000
            && size("vector_estimator.onnx") > 100_000_000
            && size("vocoder.onnx") > 80_000_000
            && size("tts.json") > 1_000
            && size("unicode_indexer.json") > 100_000
    }

    /// Downloads the full asset set (~262 MB). Voice styles are small, so
    /// all ten ship — the engine picks one by modelId.
    public static func download() async throws {
        try FileManager.default.createDirectory(at: onnxDirectory, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(
            at: modelDirectory.appendingPathComponent("voice_styles", isDirectory: true),
            withIntermediateDirectories: true
        )
        var files = [
            "onnx/duration_predictor.onnx",
            "onnx/text_encoder.onnx",
            "onnx/vector_estimator.onnx",
            "onnx/vocoder.onnx",
            "onnx/tts.json",
            "onnx/unicode_indexer.json",
        ]
        files.append(contentsOf: voices.map { "voice_styles/\($0).json" })
        for path in files {
            guard let url = URL(string: "\(baseURL)/\(path)") else {
                throw TTSError.synthesisFailed("bad Supertonic URL")
            }
            let (data, _) = try await URLSession.shared.data(from: url)
            try data.write(
                to: modelDirectory.appendingPathComponent(path),
                options: .atomic
            )
            Log.info("SupertonicModelManager: wrote \(path) (\(data.count) bytes)")
        }
    }
}
