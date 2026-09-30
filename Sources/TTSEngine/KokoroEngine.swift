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

        // Prefer the uint8 tier when both exist — half the resident memory
        // (177 MB vs 326 MB) for near-identical weight-only-quantized
        // quality, which matters on memory-constrained machines.
        let fm = FileManager.default
        var modelFile = "model_uint8.onnx"
        if !fm.fileExists(atPath: dir.appendingPathComponent(modelFile).path),
           fm.fileExists(atPath: dir.appendingPathComponent("model.onnx").path) {
            modelFile = "model.onnx"
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

        voices = try KokoroModelManager.loadVoiceBanks()
        guard !voices.isEmpty else {
            throw TTSError.synthesisFailed("no voice banks under \(KokoroModelManager.voicesDirectory.path)")
        }
        Log.info("KokoroEngine ready: \(modelFile), \(vocab.count) vocab entries, \(voices.count) voices")
    }

    /// Test-only: the tokens + style slice this engine would run.
    func testingPreparation(voice: String) -> (ids: [Int64], style: [Float], speed: [Float])? {
        let bridge = EspeakBridge()
        try? bridge.initialize()
        _ = bridge.setVoice(phonemizerVoice)
        let phonemes = bridge.phonemes(for: "Hello from Kokoro.").joined(separator: " ")
        let tokens = tokenize(phonemes)
        guard let flat = voicesFlat(for: voice) else { return nil }
        let rows = max(1, flat.count / Self.styleDim)
        let adjusted = min(max(tokens.count - 2, 0), rows - 1)
        let offset = adjusted * Self.styleDim
        guard offset + Self.styleDim <= flat.count else { return nil }
        return (tokens, Array(flat[offset..<(offset + Self.styleDim)]), [1.0])
    }

    /// Test-only: run the graph directly on this engine's session to isolate
    /// engine-state vs session-state failures.
    func sessionForTesting() -> OrtSessionRef? {
        try? ensureInitialized()
        return session
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

        let idTensor = try OrtValueRef(tensorData: tokens, shape: [1, Int64(tokens.count)], elementType: OrtElementType.int64)
        let styleTensor = try OrtValueRef(tensorData: style, shape: [1, Int64(Self.styleDim)], elementType: OrtElementType.float)
        let speedTensor = try OrtValueRef(tensorData: [speed], shape: [1], elementType: OrtElementType.float)
        let outputs: [String: OrtValueRef]
        do {
            outputs = try session.run(
                inputs: [
                    "input_ids": idTensor,
                    "style": styleTensor,
                    "speed": speedTensor,
                ],
                outputNames: [outputName]
            )
        } catch {
            throw TTSError.synthesisFailed("Kokoro run failed: \(error)")
        }

        guard let outputValue = outputs[outputName],
              let wave: [Float] = try? outputValue.floatTensorData(),
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

    /// Reference pipeline encoding: EOS id (0) pads both ends, with each
    /// phoneme character mapped through the vocab (space = 16).
    private func tokenize(_ phonemes: String) -> [Int64] {
        let eos = Int64(vocab["$"] ?? 0)
        let space = vocab[" "] ?? 16
        let ids = phonemes.compactMap { vocab[String($0)] ?? space }.map(Int64.init)
        return [eos] + ids + [eos]
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

    /// One raw float32 file per voice — the onnx-community repo's format
    /// (each `[rows, 256]` style bank, 510 rows × 256 × 4 bytes ≈ 522 KB).
    public static var voicesDirectory: URL {
        modelDirectory.appendingPathComponent("voices", isDirectory: true)
    }

    public static func modelFileURL() -> URL {
        modelDirectory.appendingPathComponent("model.onnx")
    }

    public static func tokenizerFileURL() -> URL {
        modelDirectory.appendingPathComponent("tokenizer.json")
    }

    /// Voices with a .bin bank on disk (bare ids, "af_heart").
    public static func installedVoices() -> [String] {
        let dir = voicesDirectory
        guard let names = try? FileManager.default.contentsOfDirectory(atPath: dir.path) else {
            return []
        }
        return names
            .filter { $0.hasSuffix(".bin") }
            .map { String($0.dropLast(4)) }
            .sorted()
    }

    /// Loads every voice bank: the .bin files, plus a legacy voices.npz if
    /// one is present (keyed "af_heart.npy" — the iOS app's format). Keys
    /// are bare voice ids.
    public static func loadVoiceBanks() throws -> [String: [Float]] {
        var banks: [String: [Float]] = [:]
        let fm = FileManager.default
        if let npzPath = (try? fm.contentsOfDirectory(atPath: modelDirectory.path))?
            .first(where: { $0 == "voices.npz" }),
           let npz = NpyzReader.read(
            fileFromPath: modelDirectory.appendingPathComponent(npzPath).path
           ) {
            for (name, floats) in npz {
                let id = name.hasSuffix(".npy") ? String(name.dropLast(4)) : name
                banks[id] = floats
            }
        }
        for voice in installedVoices() {
            let url = voicesDirectory.appendingPathComponent("\(voice).bin")
            guard let data = fm.contents(atPath: url.path),
                  data.count % MemoryLayout<Float>.stride == 0, data.count > 0 else { continue }
            banks[voice] = data.withUnsafeBytes {
                Array($0.bindMemory(to: Float.self))
            }
        }
        return banks
    }

    /// True when a model tier + tokenizer + at least one voice bank exist.
    public static func modelFilesAreValid() -> Bool {
        let fm = FileManager.default
        let dir = modelDirectory
        func size(_ name: String) -> Int64 {
            (try? fm.attributesOfItem(atPath: dir.appendingPathComponent(name).path))?[.size] as? Int64 ?? 0
        }
        // The uint8 tier on disk is ~82 MB — the threshold must sit below
        // that or an installed model reads as missing.
        let hasModel = size("model.onnx") > 200_000_000
            || size("model_uint8.onnx") > 60_000_000
        return hasModel && size("tokenizer.json") > 1_000
            && !installedVoices().isEmpty
    }

    /// Kokoro v1.0's 54 voice ids — the fallback when the repo listing is
    /// unreachable (offline download retry, rate limit).
    static let knownVoices = [
        "af_heart", "af_alloy", "af_aoede", "af_bella", "af_jessica", "af_kore",
        "af_nicole", "af_nova", "af_river", "af_sarah", "af_sky",
        "am_adam", "am_echo", "am_eric", "am_fenrir", "am_liam", "am_michael",
        "am_onyx", "am_puck", "am_santa",
        "bf_alice", "bf_emma", "bf_isabella", "bf_lily",
        "bm_daniel", "bm_fable", "bm_george", "bm_lewis",
        "ef_dora", "em_alex", "em_santa",
        "ff_siwis",
        "hf_alpha", "hf_beta", "hm_omega", "hm_psi",
        "if_sara", "im_nicola",
        "jf_alpha", "jf_gongitsune", "jf_nezumi", "jf_tebukuro",
        "pf_dora", "pm_alex", "pm_santa",
        "zf_xiaobei", "zf_xiaoni", "zf_xiaoxiao", "zf_xiaoyi",
        "zm_yunjian", "zm_yunxi", "zm_yunxia", "zm_yunyang",
    ]

    /// Downloads the model tier + tokenizer + every voice bank (~200 MB for
    /// uint8 + voices). The tier comes from KOKORO_TIER (unset = uint8 —
    /// half the resident memory of fp32). Voice ids come from the repo's
    /// tree listing when reachable, else `knownVoices`.
    public static func download() async throws {
        try FileManager.default.createDirectory(at: modelDirectory, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: voicesDirectory, withIntermediateDirectories: true)
        let tier = ProcessInfo.processInfo.environment["KOKORO_TIER"] == "fp32"
            ? "model.onnx"
            : "model_uint8.onnx"
        let files: [(path: String, name: String)] = [
            ("onnx/\(tier)", tier),
            ("tokenizer.json", "tokenizer.json"),
        ]
        for file in files {
            guard let url = URL(string: "\(baseURL)/\(file.path)") else {
                throw TTSError.synthesisFailed("bad Kokoro URL")
            }
            let (data, _) = try await URLSession.shared.data(from: url)
            try data.write(to: modelDirectory.appendingPathComponent(file.name), options: .atomic)
            Log.info("KokoroModelManager: wrote \(file.name) (\(data.count) bytes)")
        }

        for voice in await availableVoiceIds() {
            guard let url = URL(string: "\(baseURL)/voices/\(voice).bin") else { continue }
            let (data, _) = try await URLSession.shared.data(from: url)
            guard data.count > 100_000 else {
                // "Entry not found"-style garbage — skip, never save.
                Log.error("KokoroModelManager: \(voice).bin response too small (\(data.count) bytes)")
                continue
            }
            try data.write(to: voicesDirectory.appendingPathComponent("\(voice).bin"), options: .atomic)
        }
        Log.info("KokoroModelManager: \(installedVoices().count) voice bank(s) installed")
    }

    /// The repo's voice list (the authoritative set), falling back to the
    /// hardcoded v1.0 ids when the tree API is unreachable.
    static func availableVoiceIds() async -> [String] {
        guard let url = URL(string: "https://huggingface.co/api/models/onnx-community/Kokoro-82M-v1.0-ONNX/tree/main/voices") else {
            return knownVoices
        }
        do {
            let (data, _) = try await URLSession.shared.data(from: url)
            let root = try JSONSerialization.jsonObject(with: data) as? [[String: Any]] ?? []
            let ids = root.compactMap { entry -> String? in
                guard let path = entry["path"] as? String, path.hasSuffix(".bin") else { return nil }
                return String(path.dropLast(4)).split(separator: "/").last.map(String.init)
            }
            return ids.isEmpty ? knownVoices : ids
        } catch {
            return knownVoices
        }
    }
}
