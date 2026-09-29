import XCTest
@testable import TTSEngine
@testable import Data
@testable import AppPaths
import SpeechLogic

/// Audiobook integration: manifest building from a real ffmpeg-generated
/// M4B (chpl chapters), and the streaming decode loop verified against
/// ALSA's "null" device (full ffmpeg→pipe→sink path, no audible output).
/// Both skip when the box has no ffmpeg.
final class AudioBookTests: XCTestCase {

    private func freshHome() throws -> URL {
        let home = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("speechnotes-audio-tests-\(UUID().uuidString)", isDirectory: true)
        setenv("XDG_DATA_HOME", home.path, 1)
        setenv("XDG_CONFIG_HOME", home.path, 1)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        return home
    }

    private var ffmpegAvailable: Bool {
        FileManager.default.isExecutableFile(atPath: "/usr/bin/ffmpeg")
            && FileManager.default.isExecutableFile(atPath: "/usr/bin/ffprobe")
    }

    /// Generates `<dir>/test.m4b`: 4 s of 440 Hz sine carrying two chpl
    /// chapters (0–2 s "Chapter One", 2–4 s "Chapter Two").
    private func generateChapteredM4b(_ dir: URL) throws -> URL {
        let meta = dir.appendingPathComponent("chapters.txt")
        try """
        ;FFMETADATA1
        title=Test Book
        artist=Test Author
        [CHAPTER]
        TIMEBASE=1/1000
        START=0
        END=2000
        title=Chapter One
        [CHAPTER]
        TIMEBASE=1/1000
        START=2000
        END=4000
        title=Chapter Two
        """.write(to: meta, atomically: true, encoding: .utf8)

        let out = dir.appendingPathComponent("test.m4b")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/ffmpeg")
        process.arguments = [
            "-v", "error", "-y",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=4",
            "-i", meta.path,
            "-map_metadata", "1",
            "-codec:a", "aac",
            "-movflags", "+faststart",
            out.path,
        ]
        process.standardOutput = Pipe()
        process.standardError = Pipe()
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw XCTSkip("ffmpeg could not generate the test M4B")
        }
        return out
    }

    @MainActor
    func testAudiobookManifestFromGeneratedM4b() async throws {
        guard ffmpegAvailable else { throw XCTSkip("no ffmpeg on this box") }
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()

        let m4b = try generateChapteredM4b(AppPaths.dataDir)
        let store = BooksStore()
        let bookOpt = await store.importBook(from: m4b)
        let book = try XCTUnwrap(bookOpt)

        XCTAssertNil(book.importError)
        XCTAssertEqual(book.format, .audio)
        XCTAssertEqual(book.title, "Test Book", "ffprobe tags name the book")
        XCTAssertEqual(book.author, "Test Author")
        XCTAssertEqual(book.audioChapterSource, "chpl")
        XCTAssertEqual(book.audioChapters?.count, 2)
        XCTAssertEqual(book.audioChapters?[0].title, "Chapter One")
        XCTAssertEqual(book.audioChapters?[0].endSeconds ?? 0, 2.0, accuracy: 0.5)
        XCTAssertEqual(book.audioDuration ?? 0, 4.0, accuracy: 1.0)

        // The file kept its true extension so decoders can map containers.
        let original = BooksStore.resolveAudioOriginalURL(book: book)
        XCTAssertEqual(original.pathExtension, "m4b")

        XCTAssertEqual(original.pathExtension, "m4b")

        // TEMP DIAGNOSTIC: parse the file's slices directly.
        let head = BooksStore.headSlice(original, bytes: 8 * 1024 * 1024) ?? Data()
        let tail = BooksStore.tailSlice(original, bytes: 8 * 1024 * 1024) ?? Data()
        print("DIAG sizes: file=\((try? FileManager.default.attributesOfItem(atPath: original.path))?[.size] ?? 0) head=\(head.count) tail=\(tail.count)")
        let bytes = [UInt8](head + tail)
        if let idx = bytes.firstIndex(of: 0x63), bytes.count > idx + 4,
           bytes[idx...].prefix(4).elementsEqual([0x63, 0x68, 0x70, 0x6C]) {
            print("DIAG chpl byte offset in concatenated data:", idx)
        } else {
            print("DIAG chpl NOT present in concatenated bytes as raw pattern")
        }
        let direct = AudiobookChapters.chaptersFromMP4(head + tail, totalSeconds: 4.0)
        print("DIAG direct parse chapters:", direct.map { "\($0.title)@\($0.startSeconds)" })
    }

    /// The streaming decode loop end-to-end: ffmpeg → pipe → ALSA null
    /// sink. A 0.7 s span starting 0.3 s in must reach ≈1.0 s on the clock.
    func testBookAudioPlayerDecodesSpanToNullSink() throws {
        guard ffmpegAvailable else { throw XCTSkip("no ffmpeg on this box") }
        let dir = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("audio-player-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }

        let wav = dir.appendingPathComponent("tone.wav")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/ffmpeg")
        process.arguments = [
            "-v", "error", "-y",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
            wav.path,
        ]
        process.standardOutput = Pipe()
        process.standardError = Pipe()
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw XCTSkip("ffmpeg could not generate the test WAV")
        }

        let flags = ControlFlags()
        let player = BookAudioPlayer()
        final class Recorder: @unchecked Sendable {
            let lock = NSLock()
            var lastSeconds = 0.0
            var lastFraction = 0.0
            var ticks = 0
        }
        let recorder = Recorder()
        try player.play(
            fileURL: wav,
            startSeconds: 0.3,
            durationSeconds: 0.7,
            flags: flags,
            device: "null",
            onPosition: { seconds, fraction in
                recorder.lock.lock()
                recorder.lastSeconds = seconds
                recorder.lastFraction = fraction
                recorder.ticks += 1
                recorder.lock.unlock()
            }
        )
        recorder.lock.lock()
        let (lastS, lastF, tickCount) = (recorder.lastSeconds, recorder.lastFraction, recorder.ticks)
        recorder.lock.unlock()
        XCTAssertGreaterThan(tickCount, 5, "position ticks streamed, not batched once")
        XCTAssertEqual(lastS, 1.0, accuracy: 0.15, "audible clock tracks start + span")
        XCTAssertEqual(lastF, 1.0, accuracy: 0.1)
    }

    /// Stop mid-stream tears the pipeline down promptly.
    func testBookAudioPlayerStopIsPrompt() throws {
        guard ffmpegAvailable else { throw XCTSkip("no ffmpeg on this box") }
        let dir = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("audio-stop-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }

        let wav = dir.appendingPathComponent("tone.wav")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/ffmpeg")
        process.arguments = [
            "-v", "error", "-y",
            "-f", "lavfi", "-i", "sine=frequency=440:duration=60",
            wav.path,
        ]
        process.standardOutput = Pipe()
        process.standardError = Pipe()
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw XCTSkip("ffmpeg could not generate the test WAV")
        }

        let flags = ControlFlags()
        let player = BookAudioPlayer()
        let started = Date()
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.5) {
            flags.stop()
        }
        try player.play(
            fileURL: wav,
            startSeconds: 0,
            durationSeconds: 60,
            flags: flags,
            device: "null",
            onPosition: { _, _ in }
        )
        XCTAssertLessThan(Date().timeIntervalSince(started), 3, "stop flags end the loop promptly")
    }
}
