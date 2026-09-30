import Foundation
import TTSEngine

/// TTS tier diagnostic: synthesizes one sentence through every requested
/// engine, headless (no GTK, no ALSA). Exits non-zero when an engine that
/// should work fails, so CI and the terminal can both gate on it.
///
///   tts-smoke            — all engines (missing models are soft skips)
///   tts-smoke piper      — just one engine, strict
///   tts-smoke --strict   — all engines, missing models count as failures
///
let args = CommandLine.arguments.dropFirst()
let strict = args.contains("--strict")
let wanted = args.filter { !$0.hasPrefix("--") }
let text = "Hello from Speechnotes. The quick brown fox jumps over the lazy dog."

let outDir = FileManager.default.temporaryDirectory
    .appendingPathComponent("tts-smoke", isDirectory: true)
try? FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)

var failures = 0
for kind in [EngineKind.espeak, .pico, .piper, .kokoro, .supertonic] {
    if !wanted.isEmpty, !wanted.contains(kind.rawValue) { continue }
    let engine: TTSEngineBase
    do {
        engine = try EngineFactory.make(kind: kind)
    } catch {
        print("❌ \(kind.displayName): factory failed — \(error)")
        failures += 1
        continue
    }
    guard engine.modelCreated() else {
        if strict {
            print("❌ \(kind.displayName): model not installed")
            failures += 1
        } else {
            print("⏭️  \(kind.displayName): model not installed — skipped")
        }
        continue
    }
    let out = outDir.appendingPathComponent("\(kind.rawValue).wav")
    do {
        let rate = try engine.encodeSpeechImpl(text: text, speed: 1.0, outFile: out, abort: { false })
        let size = (try? FileManager.default.attributesOfItem(atPath: out.path))?[.size] as? Int64 ?? 0
        let seconds = size > 44 ? Double(size - 44) / Double(2 * rate) : 0
        print("✅ \(kind.displayName): \(rate) Hz, \(String(format: "%.2f", seconds))s (\(size) bytes) → \(out.path)")
    } catch {
        print("❌ \(kind.displayName): \(error)")
        failures += 1
    }
}

exit(failures > 0 ? 1 : 0)
