import Foundation
import AlsaSink
import Log

/// Drives one chunk's WAV through ALSA with the read-along position clock.
/// Confined to the worker task (never touched from two threads), so it can
/// keep plain mutable state.
///
/// Blocking `snd_pcm_writei` is the pacing mechanism: the sink backpressures,
/// and between 50 ms buffers the loop polls the stop/pause flags — control
/// stays responsive without condition variables.
final class TTSPlayer {
    private var sink: AlsaSink?
    private var sinkRate = 0

    var isPlaying: Bool { sink?.isOpen == true }

    /// Plays one chunk's PCM. `onPosition` fires per buffer with the audible
    /// UTF-16 text offset and rough whole-note fraction (chunk-relative math
    /// is the caller's: it knows the chunk list).
    func play(
        samples: [Int16],
        sampleRate: Int,
        chunk: SpeechChunk,
        chunkCount: Int,
        flags: ControlFlags,
        onPosition: @escaping @Sendable (_ textOffset: Int, _ fraction: Double) -> Void
    ) throws {
        try openSink(rate: sampleRate)
        guard let sink else { throw TTSError.synthesisFailed("no ALSA device") }

        // ~50 ms of mono 16-bit audio per write.
        let bufferFrames = max(1, sampleRate / 20)
        var written = 0
        while written < samples.count {
            while flags.isPaused && !flags.isStopped {
                Thread.sleep(forTimeInterval: 0.05)
            }
            if flags.isStopped {
                sink.drop()
                return
            }

            let end = min(written + bufferFrames, samples.count)
            let frames = sink.write(Array(samples[written..<end]))
            guard frames > 0 else {
                throw TTSError.synthesisFailed("ALSA write failed (\(frames))")
            }
            written += frames

            // The audible position trails the write position by the device
            // pipeline delay — subtracting it keeps the highlight honest.
            let audible = max(0, written - sink.delayFrames())
            let localFraction = Double(audible) / Double(max(1, samples.count))
            let offset = chunk.textUTF16Offset
                + Int((Double(chunk.textUTF16Length) * localFraction).rounded())
            let globalFraction = (Double(chunk.index) + localFraction)
                / Double(max(1, chunkCount))
            onPosition(offset, globalFraction)
        }
    }

    /// Stops output immediately and discards buffered audio.
    func drop() {
        sink?.drop()
    }

    func close() {
        if let sink {
            // drain() in the sink's deinit flushes the tail naturally.
            _ = sink
        }
        sink = nil
        sinkRate = 0
    }

    private func openSink(rate: Int) throws {
        if let sink, sinkRate == rate, sink.isOpen { return }
        sink = nil
        sinkRate = 0
        let opened = try AlsaSink(sampleRate: rate)
        sink = opened
        sinkRate = rate
        Log.debug("TTSPlayer: ALSA sink opened at \(opened.actualSampleRate) Hz")
    }
}
