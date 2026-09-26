import XCTest
@testable import TTSEngine

/// Offline pipeline tests: WAV codec, chunk planning, control flags, and the
/// real eSpeak path (no audio device needed — the bridge runs in retrieval
/// mode and the player never opens for these).
final class TTSEngineTests: XCTestCase {

    func testWAVRoundtrip() throws {
        let samples: [Int16] = [0, 1000, -1000, 32767, -32768, 12345]
        let url = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("tts-\(UUID().uuidString).wav")
        defer { try? FileManager.default.removeItem(at: url) }

        try WAVFile.write(samples: samples, sampleRate: 22050, to: url)
        let (back, rate) = try WAVFile.read(at: url)
        XCTAssertEqual(rate, 22050)
        XCTAssertEqual(back, samples)
    }

    func testPlanChunksCoversTextInOrder() {
        let text = "First sentence here. Second one follows! Third asks a question?"
        let chunks = TTSChunker.planChunks(noteId: UUID(), text: text)
        XCTAssertFalse(chunks.isEmpty)
        XCTAssertEqual(chunks[0].textUTF16Offset, 0)
        for pair in zip(chunks, chunks.dropFirst()) {
            XCTAssertLessThan(pair.0.textUTF16Offset, pair.1.textUTF16Offset)
        }
        for chunk in chunks {
            XCTAssertFalse(chunk.text.isEmpty)
            XCTAssertGreaterThan(chunk.textUTF16Length, 0)
        }
    }

    func testPlanChunksEmptyText() {
        XCTAssertTrue(TTSChunker.planChunks(noteId: UUID(), text: "").isEmpty)
    }

    func testControlFlagsStopAndPause() {
        let flags = ControlFlags()
        XCTAssertFalse(flags.isStopped)
        XCTAssertFalse(flags.isPaused)

        flags.setPaused(true)
        XCTAssertTrue(flags.isPaused)
        XCTAssertFalse(flags.isStopped)

        flags.stop()
        XCTAssertTrue(flags.isStopped)
        XCTAssertFalse(flags.isPaused, "stop overrides pause")

        flags.reset()
        XCTAssertFalse(flags.isStopped)
        XCTAssertFalse(flags.isPaused)
    }

    func testEngineFactoryRefusesMissingEngines() {
        XCTAssertThrowsError(try EngineFactory.make(kind: .kokoro))
        XCTAssertThrowsError(try EngineFactory.make(kind: .supertonic))
        XCTAssertNoThrow(try EngineFactory.make(kind: .espeak))
    }

    /// The real eSpeak path end to end: synth in retrieval mode, WAV on
    /// disk, parseable back. Uses the default voice; no ALSA involved.
    func testEspeakEngineWritesPlayableWAV() throws {
        let engine = EspeakEngine()
        guard engine.modelCreated() else {
            XCTFail("espeak should always be ready")
            return
        }
        let url = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("tts-\(UUID().uuidString).wav")
        defer { try? FileManager.default.removeItem(at: url) }

        let rate = try engine.encodeSpeechImpl(
            text: "Testing one two three.",
            speed: 1.0,
            outFile: url,
            abort: { false }
        )
        XCTAssertGreaterThan(rate, 0)
        let (samples, wavRate) = try WAVFile.read(at: url)
        XCTAssertEqual(wavRate, rate)
        XCTAssertFalse(samples.isEmpty)
    }
}
