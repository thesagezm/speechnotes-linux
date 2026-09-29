import Foundation
import Log

#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

/// Pico2Wave (SVOX Pico) — dsnote's light second engine, driven through
/// the distro's `pico2wave` CLI: it synthesizes a WAV file in one shot and
/// we re-emit it through the shared WAV codec. No speed control (the CLI
/// has no rate flag) — `modelSupportsSpeed()` says so honestly.
public final class PicoEngine: TTSEngineBase, @unchecked Sendable {
    /// Pico's voice set: en-US/en-GB/de-DE/es-ES/fr-FR/it-IT.
    public var lang = "en-US"

    public static let toolPath = "/usr/bin/pico2wave"

    public static var toolAvailable: Bool {
        FileManager.default.isExecutableFile(atPath: toolPath)
    }

    public override func modelCreated() -> Bool { Self.toolAvailable }

    public override func modelSupportsSpeed() -> Bool { false }

    public override func createModel(modelPath: String, modelId: String) -> Bool {
        // The "model" is the distro tool + its lang data.
        Self.toolAvailable
    }

    @discardableResult
    public override func encodeSpeechImpl(
        text: String,
        speed: Float,
        outFile: URL,
        abort: @escaping @Sendable () -> Bool
    ) throws -> Int {
        guard Self.toolAvailable else {
            throw TTSError.synthesisFailed("pico2wave is not installed")
        }
        guard !abort() else { return 22_050 }

        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw TTSError.synthesisFailed("nothing to speak")
        }

        let tmp = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("pico-\(UUID().uuidString).wav")
        defer { try? FileManager.default.removeItem(at: tmp) }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: Self.toolPath)
        process.arguments = ["-l", lang, "-w", tmp.path, trimmed]
        process.standardOutput = Pipe()
        process.standardError = Pipe()
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw TTSError.synthesisFailed("pico2wave exited \(process.terminationStatus)")
        }
        guard !abort() else { return 22_050 }

        let (samples, rate) = try WAVFile.read(at: tmp)
        guard !samples.isEmpty else {
            throw TTSError.synthesisFailed("pico2wave produced no audio")
        }
        try WAVFile.write(samples: samples, sampleRate: rate, to: outFile)
        return rate
    }
}
