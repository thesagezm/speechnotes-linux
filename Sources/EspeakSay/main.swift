import AlsaSink
import EspeakBridge
import Foundation

/// Phase-0 speech gate: synthesize one sentence with espeak-ng and play it
/// through the ALSA sink. Verifies the whole chain — espeak's retrieval
/// callback → Int16 PCM → ALSA — end to end.
///
/// Voice names are espeak's identifiers (e.g. "en-us", "de"), not the display
/// names; `--list` prints them.
let args = CommandLine.arguments.dropFirst()
if args.first == "--list" {
    let probe = EspeakBridge()
    try? probe.initialize()
    for v in probe.listVoices() where v.languages.contains(where: { $0.hasPrefix("en") }) {
        print("\(v.identifier)\t\(v.name)\t[\(v.languages.joined(separator: ","))] \(v.gender)")
    }
    exit(0)
}

let voice = args.first ?? "en-us"
let text = args.dropFirst().joined(separator: " ").trimmingCharacters(in: .whitespacesAndNewlines)
let spokenText = text.isEmpty ? "Speechnotes for Linux can speak this sentence." : text

do {
    let espeak = EspeakBridge()
    try espeak.initialize()
    print("espeak initialized at \(espeak.sampleRate) Hz; \(espeak.listVoices().count) voices")

    guard espeak.setVoice(voice) else {
        print("voice '\(voice)' not found — try: \(espeak.listVoices().prefix(5).map(\.name))")
        exit(2)
    }
    espeak.setRate(175)

    let samples = try espeak.synthesize(spokenText)
    let seconds = Double(samples.count) / Double(espeak.sampleRate)
    print("synthesized \(samples.count) samples (\(String(format: "%.2f", seconds)) s)")

    let sink = try AlsaSink(sampleRate: espeak.sampleRate)
    let written = sink.write(samples)
    print("played \(written) frames")
    if written != samples.count {
        print("SHORT WRITE: \(written)/\(samples.count)")
        exit(1)
    }
    print("PASS: espeak-ng → ALSA")
    exit(0)
} catch {
    print("FAIL: \(error)")
    exit(1)
}
