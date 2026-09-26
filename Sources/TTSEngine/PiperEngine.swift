import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif
import EspeakBridge
import AppPaths
import Log

/// piper voice model metadata (the `<voice>.onnx.json` sidecar).
struct PiperVoiceConfig: Decodable {
    struct Audio: Decodable { let sample_rate: Int }
    struct Inference: Decodable {
        let noise_scale: Double?
        let noise_w: Double?
        let length_scale: Double?
    }
    struct Espeak: Decodable { let voice: String? }

    let audio: Audio
    let inference: Inference?
    let num_speakers: Int?
    let phoneme_id_map: [String: [Int]]
    let espeak: Espeak?
}

/// The Piper tier: a VITS model through ONNX Runtime, phonemized with
/// espeak-ng IPA output. 20–60 MB per voice, 22050 Hz audio, far better
/// quality than eSpeak at still-responsive speeds.
public final class PiperEngine: TTSEngineBase, @unchecked Sendable {
    private let bridge = EspeakBridge()
    private var model: OrtModel?
    private var config: PiperVoiceConfig?
    private var loadedVoice = ""

    public override func modelCreated() -> Bool { model != nil }

    public override func modelSupportsSpeed() -> Bool { true }

    /// `modelPath` is the voice directory, `modelId` the voice name:
    /// expects `<dir>/<id>.onnx` plus `<dir>/<id>.onnx.json`.
    public override func createModel(modelPath: String, modelId: String) -> Bool {
        let dir = URL(fileURLWithPath: modelPath)
        do {
            let cfg = try JSONDecoder().decode(
                PiperVoiceConfig.self,
                from: Data(contentsOf: dir.appendingPathComponent("\(modelId).onnx.json"))
            )
            let ort = try OrtModel(
                modelPath: dir.appendingPathComponent("\(modelId).onnx").path,
                withSpeakerId: (cfg.num_speakers ?? 1) > 1
            )
            try bridge.initialize()
            if let espeakVoice = cfg.espeak?.voice, !espeakVoice.isEmpty {
                _ = bridge.setVoice(espeakVoice)
            }
            config = cfg
            model = ort
            loadedVoice = modelId
            Log.info("PiperEngine loaded \(modelId) at \(cfg.audio.sample_rate) Hz")
            return true
        } catch {
            Log.error("PiperEngine load \(modelId) failed: \(error)")
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
        guard let model, let config else {
            throw TTSError.synthesisFailed("piper model not loaded")
        }
        let clauses = bridge.phonemes(for: text)
        guard !clauses.isEmpty else {
            throw TTSError.synthesisFailed("phonemization produced nothing")
        }
        let ids = Self.phonemeIds(from: clauses, map: config.phoneme_id_map)

        // Speed scales durations: length_scale shrinks as speed grows.
        let rate = max(0.25, min(4.0, Double(speed)))
        let lengthScale = (config.inference?.length_scale ?? 1.0) / rate
        // piper v1 voices take [noise_scale, length_scale, noise_w] on `scales`.
        var scales = [
            Float(config.inference?.noise_scale ?? 0.667),
            Float(lengthScale),
        ]
        if let noiseW = config.inference?.noise_w {
            scales.append(Float(noiseW))
        }

        let wave = try model.synthesize(inputIds: ids, scales: scales, speakerId: 0)
        guard !abort() else { return config.audio.sample_rate }
        guard !wave.isEmpty else {
            throw TTSError.synthesisFailed("piper produced no audio")
        }

        let samples = wave.map { w -> Int16 in
            Int16((max(-1.0, min(1.0, Double(w))) * 32767.0).rounded())
        }
        try WAVFile.write(samples: samples, sampleRate: config.audio.sample_rate, to: outFile)
        return config.audio.sample_rate
    }

    /// piper-phonemize's id sequence: BOS, then PAD + symbol ids per
    /// phoneme, PAD, EOS. Symbols the model's map doesn't know (stress
    /// marks and friends) are dropped rather than guessed at.
    static func phonemeIds(from clauses: [String], map: [String: [Int]]) -> [Int64] {
        let bos = Int64(map["^"]?.first ?? 1)
        let eos = Int64(map["$"]?.first ?? 2)
        let pad = Int64(map["_"]?.first ?? 0)
        var ids: [Int64] = [bos]
        for clause in clauses {
            for scalar in clause.unicodeScalars {
                guard let symbolIds = map[String(scalar)] else { continue }
                ids.append(pad)
                ids.append(contentsOf: symbolIds.map(Int64.init))
            }
        }
        ids.append(pad)
        ids.append(eos)
        return ids
    }
}

/// Discovers and downloads piper voices under `modelsDir/piper/<voice>/`.
public enum PiperModelManager {
    /// v1 ships one curated voice; a proper voice picker widens this later.
    public static let curatedVoice = "en_US-lessac-medium"

    public static func voicesDirectory() -> URL {
        AppPaths.modelsDir.appendingPathComponent("piper", isDirectory: true)
    }

    /// Voices with both an ONNX model and its config on disk.
    public static func installedVoices() -> [String] {
        let dir = voicesDirectory()
        guard let names = try? FileManager.default.contentsOfDirectory(atPath: dir.path) else {
            return []
        }
        return names
            .filter { name in
                FileManager.default.fileExists(
                    atPath: dir.appendingPathComponent("\(name)/\(name).onnx").path)
            }
            .sorted()
    }

    public static func modelDirectory(for voice: String) -> URL {
        voicesDirectory().appendingPathComponent(voice, isDirectory: true)
    }

    /// Fetches the voice's ONNX model + config from the piper-voices
    /// release. Voice ids parse as `<lang>_<REGION>-<speaker>-<quality>`.
    public static func download(voice: String = PiperModelManager.curatedVoice) async throws -> URL {
        let parts = voice.split(separator: "-")
        guard parts.count == 3 else {
            throw TTSError.synthesisFailed("unsupported voice id \(voice)")
        }
        let lang = parts[0].split(separator: "_").first.map(String.init) ?? "en"
        let base = "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/"
            + "\(lang)/\(parts[0])/\(parts[1])/\(parts[2])/\(voice)"
        let dir = modelDirectory(for: voice)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        for suffix in [".onnx", ".onnx.json"] {
            guard let url = URL(string: base + suffix) else {
                throw TTSError.synthesisFailed("bad model URL")
            }
            let (data, _) = try await URLSession.shared.data(from: url)
            try data.write(to: dir.appendingPathComponent(voice + suffix), options: .atomic)
            Log.info("PiperModelManager: wrote \(voice)\(suffix) (\(data.count) bytes)")
        }
        return dir
    }
}
