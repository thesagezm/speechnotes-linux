#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
import Foundation
import EspeakBridge
import AppPaths
import Log

/// Kokoro-82M over ONNX Runtime: the best-quality tier in the app, and one
/// of the user's two non-negotiable engines.
///
/// Model set (modelsDir/kokoro/): `model.onnx` (~326 MB fp32) or the uint8
/// weight-only quant `model_uint8.onnx` (~177 MB, same graph), plus
/// `tokenizer.json` (per-character phoneme vocab) and `voices.npz` (54 style
/// vectors, one [rows × 256] float32 array per voice).
///
/// Inference spec (verified against the PocketPal React Native engine):
/// inputs `input_ids` int64 [1, N], `style` float32 [1, 256] sliced at
/// clamp(N-2, 0, 509) * 256, `speed` float32 [1]; output `waveform` float32
/// @ 24 kHz. The model takes speed natively, so the controller's multiplier
/// feeds straight in.
public final class KokoroEngine: TTSEngineBase, @unchecked Sendable {
    private var session: OrtSessionRef?
    private var voices: [String: [Float]] = [:]
    private var phonemizer: EspeakBridge?
    private var outputName = "waveform"

    /// espeak voice name used for G2P (the phonemizer front end).
    public var voice = "af_heart"
    /// espeak's own language voice, e.g. "en-us" — Kokoro is English-only,
    /// so British/American pronunciation is the only real choice.
    public var phonemizerVoice = "en-us"

    /// Model files and their sizes in bytes (for the download manifest).
    private static let styleDim = 256
    private static let maxTokens = 510

    public override func modelCreated() -> Bool { session != nil }

    public override func modelSupportsSpeed() -> Bool { true }

    /// `modelPath` is the directory; `modelId` selects the tier
    /// ("model.onnx" or "model_uint8.onnx" — the fp32 tier is default).
    public override func createModel(modelPath: String, modelId: String) -> Bool {
        do {
            try ensureInitialized()
            return true
        } catch {
            Log.error("KokoroEngine load failed: \(error)")
            return false
        }
    }

    private func ensureInitialized() throws {
        guard session == nil else { return }

        let dir = KokoroModelManager.modelDirectory
        guard KokoroModelManager.modelFilesAreValid() else {
            throw TTSError.synthesisFailed("Kokoro model files missing at \(dir.path)")
        }

        // Prefer fp32; fall back to the uint8 tier on small disks.
        let fm = FileManager.default
        var modelFile = "model.onnx"
        if !fm.fileExists(atPath: dir.appendingPathComponent(modelFile).path),
           fm.fileExists(atPath: dir.appendingPathComponent("model_uint8.onnx").path) {
            modelFile = "model_uint8.onnx"
        }

        session = try OrtSessionRef(modelPath: dir.appendingPathComponent(modelFile).path)
        if let name = session?.outputNames.first(where: { $0 == "waveform" })
            ?? session?.outputNames.first {
            outputName = name
        }

        // tokenizer.json's vocab maps every phoneme character to an id.
        let tokenizerData = try Data(contentsOf: dir.appendingPathComponent("tokenizer.json"))
        let tokenizerJSON = try JSONSerialization.jsonObject(with: tokenizerData) as? [String: Any]
        vocab = (tokenizerJSON?["model"] as? [String: Any])?["vocab"] as? [String: Int] ?? [:]
        guard !vocab.isEmpty else {
            throw TTSError.synthesisFailed("tokenizer.json has no vocab")
        }

        // voices.npz: a zip of .npy arrays, one per voice.
        guard let voicesPath = KokoroModelManager.voiceBankPath() else {
            throw TTSError.synthesisFailed("voices.npz missing")
        }
        guard let npz = NpyzReader.read(fileFromPath: voicesPath), !npz.isEmpty else {
            throw TTSError.synthesisFailed("voices.npz unreadable")
        }
        voices = npz
        Log.info("KokoroEngine ready: \(modelFile), \(vocab.count) vocab entries, \(voices.count) voices")
    }

    private var vocab: [String: Int] = [:]

    @discardableResult
    public override func encodeSpeechImpl(
        text: String,
        speed: Float,
        outFile: URL,
        abort: @escaping @Sendable () -> Bool
    ) throws -> Int {
        try ensureInitialized()
        guard let session else { throw TTSError.synthesisFailed("Kokoro model not loaded") }

        let bridge = EspeakBridge()
        try bridge.initialize()
        _ = bridge.setVoice(phonemizerVoice)
        let phonemes = bridge.phonemes(for: text).joined(separator: " ")
        guard !phonemes.isEmpty else {
            throw TTSError.synthesisFailed("phonemization produced nothing")
        }
        let tokens = tokenize(phonemes)
        guard tokens.count > 1, tokens.count <= Self.maxTokens else {
            throw TTSError.synthesisFailed("token count \(tokens.count) out of range")
        }

        guard let flat = voicesFlat(for: voice) else {
            throw TTSError.synthesisFailed("voice \(voice) not in voices.npz")
        }

        // Style slice: clamp(N - 2, 0, rows - 1) * 256 — the reference
        // implementation's index for "voice style at this length".
        let rows = max(1, flat.count / Self.styleDim)
        let adjusted = min(max(tokens.count - 2, 0), rows - 1)
        let offset = adjusted * Self.styleDim
        guard offset + Self.styleDim <= flat.count else {
            throw TTSError.synthesisFailed("voice style slice out of bounds")
        }
        let style = Array(flat[offset..<(offset + Self.styleDim)])

        let outputs: [String: OrtValueRef]
        do {
            outputs = try session.run(
                inputs: [
                    "input_ids": try OrtValueRef(tensorData: tokens, shape: [1, Int64(tokens.count)], elementType: OrtElementType.int64),
                    "style": try OrtValueRef(tensorData: style, shape: [1, Int64(Self.styleDim)], elementType: OrtElementType.float),
                    "speed": try OrtValueRef(tensorData: [speed], shape: [1], elementType: OrtElementType.float),
                ],
                outputNames: [outputName]
            )
        } catch {
            throw TTSError.synthesisFailed("Kokoro run failed: \(error)")
        }

        guard let outputValue = outputs[outputName],
              let wave: [Float] = try? outputValue.tensorData(),
              !wave.isEmpty else {
            throw TTSError.synthesisFailed("model produced no audio")
        }
        guard !abort() else { return 24_000 }

        let samples = wave.map { w -> Int16 in
            Int16((max(-1.0, min(1.0, Double(w))) * 32767.0).rounded())
        }
        try WAVFile.write(samples: samples, sampleRate: 24_000, to: outFile)
        return 24_000
    }

    /// Per-character vocab lookup, matching the reference tokenizer.
    /// Whitespace between clauses maps to the vocab's space id.
    private func tokenize(_ phonemes: String) -> [Int64] {
        phonemes.compactMap { vocab[String($0)] ?? vocab[" "] }.map(Int64.init)
    }

    /// Voice banks are keyed by name + ".npy" in the zip.
    private func voicesFlat(for voice: String) -> [Float]? {
        voices[voice + ".npy"] ?? voices[voice]
    }
}

/// Voice/config discovery + download under modelsDir/kokoro/.
public enum KokoroModelManager {
    public static let baseURL = "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main"
    /// Voices bundled in voices.npz (same set the iOS app ships).
    public static let curatedVoice = "af_heart"

    public static var modelDirectory: URL {
        AppPaths.modelsDir.appendingPathComponent("kokoro", isDirectory: true)
    }

    public static func modelFileURL() -> URL {
        modelDirectory.appendingPathComponent("model.onnx")
    }

    public static func tokenizerFileURL() -> URL {
        modelDirectory.appendingPathComponent("tokenizer.json")
    }

    public static func voiceBankPath() -> String? {
        let url = modelDirectory.appendingPathComponent("voices.npz")
        return FileManager.default.fileExists(atPath: url.path) ? url.path : nil
    }

    /// True when a model tier + tokenizer + voice bank are all present.
    public static func modelFilesAreValid() -> Bool {
        let fm = FileManager.default
        let dir = modelDirectory
        func size(_ name: String) -> Int64 {
            (try? fm.attributesOfItem(atPath: dir.appendingPathComponent(name).path))?[.size] as? Int64 ?? 0
        }
        let hasModel = size("model.onnx") > 200_000_000
            || size("model_uint8.onnx") > 100_000_000
        return hasModel && size("tokenizer.json") > 1_000
            && (size("voices.npz") > 10_000_000)
    }

    /// Downloads the fp32 model + tokenizer + voice bank (~340 MB total).
    public static func download() async throws {
        try FileManager.default.createDirectory(at: modelDirectory, withIntermediateDirectories: true)
        for (path, size) in [
            ("onnx/model.onnx", 326_000_000),
            ("tokenizer.json", 3_500),
            ("voices.npz", 10_500_000),
        ] {
            let name = (path as NSString).lastPathComponent
            guard let url = URL(string: "\(baseURL)/\(path)") else {
                throw TTSError.synthesisFailed("bad Kokoro URL")
            }
            let (data, _) = try await URLSession.shared.data(from: url)
            try data.write(to: modelDirectory.appendingPathComponent(name), options: .atomic)
            Log.info("KokoroModelManager: wrote \(name) (\(data.count) bytes)")
        }
    }
}
