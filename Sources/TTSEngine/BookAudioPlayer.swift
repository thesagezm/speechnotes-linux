import Foundation
import AlsaSink
import Log

/// Streams one audiobook chapter through ALSA: ffmpeg decodes the source
/// (M4B/M4A/MP4/MP3 — anything the box's ffmpeg reads) to raw mono PCM on a
/// pipe, and this loop writes 50 ms blocks to the sink, polling the shared
/// stop/pause flags between buffers. Same blocking-writei pacing as
/// TTSPlayer — control stays responsive without condition variables, and a
/// one-hour chapter never materializes in memory (TTSPlayer's full-chunk
/// arrays are only affordable for synthesized sentences).
///
/// Confined to the worker task like TTSPlayer — plain mutable state.
final class BookAudioPlayer {
    private var sink: AlsaSink?
    private var ffmpeg: Process?

    /// The audio tier decodes to one fixed wire format; ALSA soft-resamples
    /// to the device's supported rate.
    static let sampleRate = 44_100

    static let ffmpegPath: String = {
        for candidate in ["/usr/bin/ffmpeg", "/usr/local/bin/ffmpeg"] {
            if FileManager.default.isExecutableFile(atPath: candidate) { return candidate }
        }
        return "/usr/bin/ffmpeg"
    }()

    var isPlaying: Bool { sink?.isOpen == true }

    /// Plays `[startSeconds, startSeconds + durationSeconds)` of `fileURL`.
    /// `onPosition` fires per buffer with the audible absolute seconds and
    /// the fraction through the requested span. Returns when the span ends,
    /// the pipe closes, or the flags say stop.
    func play(
        fileURL: URL,
        startSeconds: Double,
        durationSeconds: Double?,
        flags: ControlFlags,
        device: String = "default",
        onPosition: @escaping @Sendable (_ seconds: Double, _ fraction: Double) -> Void
    ) throws {
        let ffmpeg = Process()
        ffmpeg.executableURL = URL(fileURLWithPath: Self.ffmpegPath)
        var args = [
            "-v", "error",
            "-ss", String(format: "%.3f", max(0, startSeconds)),
        ]
        if let durationSeconds, durationSeconds > 0 {
            args += ["-t", String(format: "%.3f", durationSeconds)]
        }
        args += [
            "-i", fileURL.path,
            "-vn",
            "-f", "s16le",
            "-acodec", "pcm_s16le",
            "-ac", "1",
            "-ar", String(Self.sampleRate),
            "pipe:1",
        ]
        ffmpeg.arguments = args
        let pipe = Pipe()
        ffmpeg.standardOutput = pipe
        ffmpeg.standardError = Pipe()
        try ffmpeg.run()
        self.ffmpeg = ffmpeg
        defer {
            if ffmpeg.isRunning {
                ffmpeg.terminate()
            }
            ffmpeg.waitUntilExit()
            self.ffmpeg = nil
        }

        let sink = try AlsaSink(sampleRate: Self.sampleRate, device: device)
        self.sink = sink
        defer {
            sink.drop()
            self.sink = nil  // AlsaSink.deinit drains + closes
        }

        let fileHandle = pipe.fileHandleForReading
        // ~50 ms of mono 16-bit audio per read/write.
        let blockBytes = Self.sampleRate / 20 * 2
        var totalFrames = 0

        while true {
            while flags.isPaused && !flags.isStopped {
                Thread.sleep(forTimeInterval: 0.05)
            }
            if flags.isStopped {
                sink.drop()
                return
            }

            let data = fileHandle.readData(ofLength: blockBytes)
            guard !data.isEmpty else { return }  // EOF — the span played out
            let samples = data.withUnsafeBytes { raw in
                Array(raw.bindMemory(to: Int16.self))
            }
            let frames = sink.write(samples)
            guard frames > 0 else {
                throw TTSError.synthesisFailed("ALSA write failed (\(frames))")
            }
            totalFrames += frames

            let audible = max(0, totalFrames - sink.delayFrames())
            let seconds = startSeconds + Double(audible) / Double(Self.sampleRate)
            let fraction: Double
            if let durationSeconds, durationSeconds > 0 {
                fraction = min(1, (seconds - startSeconds) / durationSeconds)
            } else {
                fraction = 0
            }
            onPosition(seconds, fraction)
        }
    }

    /// Forcibly tears down a running stream (controller stop path).
    func abort() {
        if let ffmpeg, ffmpeg.isRunning {
            ffmpeg.terminate()
        }
        sink?.drop()
    }
}
