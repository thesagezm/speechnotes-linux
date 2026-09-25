/// Tiny wrapper around the ALSA PCM playback API, just enough for the TTS
/// player: open the default device at a given sample rate, write mono 16-bit
/// PCM frames, and read the pipeline position (for the read-along clock).
import CALSA
import Foundation
import Log

/// Errors surfaced by `AlsaSink`. The SDK returns negative errno-style codes;
/// we carry the message so the log is actionable.
public struct AlsaError: Error, Equatable {
    public let code: Int32
    public let message: String

    init(_ code: Int32) {
        self.code = code
        self.message = String(cString: snd_strerror(code))
    }
}

/// A mono 16-bit PCM playback sink on the ALSA default device.
///
/// One instance = one open PCM handle. Handles are opened in blocking mode
/// with a moderate buffer, so `write` backpressures naturally when the
/// consumer (the player) gets ahead of the device — no extra pacing logic
/// needed at this layer.
public final class AlsaSink {
    private var handle: OpaquePointer?
    public let sampleRate: Int
    public let channels: Int = 1

    /// Hardware parameters actually granted by the device (may differ from
    /// what we asked for — the player's position clock uses the real rate).
    public private(set) var actualSampleRate: Int = 0

    public var isOpen: Bool { handle != nil }

    public init(sampleRate: Int) throws {
        self.sampleRate = sampleRate

        var opened: OpaquePointer?
        var code = snd_pcm_open(&opened, "default", SND_PCM_STREAM_PLAYBACK, 0)
        guard code >= 0, let h = opened else { throw AlsaError(code) }
        handle = h

        code = snd_pcm_set_params(
            h,
            SND_PCM_FORMAT_S16_LE,
            SND_PCM_ACCESS_RW_INTERLEAVED,
            1,
            UInt32(sampleRate),
            1,      // soft resample: ALSA may adapt to a nearby supported rate
            170_000 // ~170 ms latency target
        )
        guard code >= 0 else {
            let err = AlsaError(code)
            snd_pcm_close(h)
            handle = nil
            throw err
        }

        // The library exposes hw params as an opaque struct pointer; Swift
        // imports the malloc out-parameter as a plain OpaquePointer slot.
        if let granted = Self.grantedRate(h), granted > 0 { actualSampleRate = granted }
        if actualSampleRate == 0 { actualSampleRate = sampleRate }
    }

    deinit {
        if let h = handle {
            snd_pcm_drain(h)
            snd_pcm_close(h)
        }
    }

    /// Writes one buffer of interleaved 16-bit PCM frames. Blocks until all
    /// frames are accepted (or an unrecoverable error occurs).
    /// - Returns: number of frames written.
    @discardableResult
    public func write(_ samples: [Int16]) -> Int {
        guard let h = handle, !samples.isEmpty else { return 0 }
        let frames = snd_pcm_writei(h, samples, snd_pcm_uframes_t(samples.count))
        if frames < 0 {
            // Recoverable errors (underrun, suspend) are handled by ALSA's
            // recovery helper; anything else returns as the negative code.
            let recovered = snd_pcm_recover(h, Int32(frames), 1)
            if recovered < 0 {
                Log.error("ALSA write failed and did not recover: \(AlsaError(Int32(frames)).message)")
                return Int(frames)
            }
            return snd_pcm_writei(h, samples, snd_pcm_uframes_t(samples.count))
        }
        return frames
    }

    /// Delay in frames between the write position and the play position —
    /// the "samples already written but not yet audible" count the player
    /// uses to answer "which sentence is sounding right now".
    public func delayFrames() -> Int {
        guard let h = handle else { return 0 }
        var delay: snd_pcm_sframes_t = 0
        if snd_pcm_delay(h, &delay) >= 0, delay >= 0 {
            return Int(delay)
        }
        return 0
    }

    /// Frames still free in the buffer (how far ahead the player may schedule).
    public func availableFrames() -> Int {
        guard let h = handle else { return 0 }
        return snd_pcm_avail_update(h)
    }

    /// Stops playback immediately, discarding buffered frames (used on stop()).
    public func drop() {
        guard let h = handle else { return }
        snd_pcm_drop(h)
        snd_pcm_prepare(h)
    }

    /// Suspends the stream, keeping buffered frames (used on pause()).
    public func pause() {
        guard let h = handle else { return }
        snd_pcm_pause(h, 1)
    }

    public func resume() {
        guard let h = handle else { return }
        snd_pcm_pause(h, 0)
    }

    /// Asks ALSA which sample rate the device actually granted, so the
    /// player's position clock uses the real rate.
    private static func grantedRate(_ handle: OpaquePointer) -> Int? {
        var slot: OpaquePointer?
        guard snd_pcm_hw_params_malloc(&slot) >= 0, let params = slot else { return nil }
        defer { snd_pcm_hw_params_free(params) }
        guard snd_pcm_hw_params_current(handle, params) >= 0 else { return nil }
        var rate: CUnsignedInt = 0
        var dir: CInt = 0
        guard snd_pcm_hw_params_get_rate(params, &rate, &dir) >= 0 else { return nil }
        return Int(rate)
    }
}
