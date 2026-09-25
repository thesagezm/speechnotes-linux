import AlsaSink
import Foundation
import Log

/// Phase-0 audio gate: play a short sine tone through the ALSA default device.
/// Verifies the C-interop sink: open → write frames → close without errors.
let sampleRate = 44100
let frequency = 440.0
let duration = 1.0

do {
    let sink = try AlsaSink(sampleRate: sampleRate)
    Log.info("ALSA opened: requested \(sampleRate) Hz, granted \(sink.actualSampleRate) Hz")

    let frameCount = Int(Double(sink.actualSampleRate) * duration)
    var samples = [Int16]()
    samples.reserveCapacity(frameCount)
    for i in 0..<frameCount {
        let t = Double(i) / Double(sink.actualSampleRate)
        // Fade in/out over the first/last 20 ms to avoid a click.
        let envelope: Double
        let ramp = 0.02 * Double(sink.actualSampleRate)
        if Double(i) < ramp {
            envelope = Double(i) / ramp
        } else if Double(frameCount - i) < ramp {
            envelope = Double(frameCount - i) / ramp
        } else {
            envelope = 1.0
        }
        let value = sin(2.0 * .pi * frequency * t) * envelope
        samples.append(Int16(max(-32768, min(32767, value * 32000))))
    }

    let written = sink.write(samples)
    Log.info("ALSA wrote \(written) frames of \(frameCount); delay=\(sink.delayFrames()) frames")
    if written != frameCount {
        Log.error("ALSA short write: \(written)/\(frameCount)")
        exit(1)
    }

    // deinit drains + closes.
    Log.info("ALSA tone complete")
    exit(0)
} catch {
    Log.error("ALSA tone failed: \(error)")
    exit(1)
}
