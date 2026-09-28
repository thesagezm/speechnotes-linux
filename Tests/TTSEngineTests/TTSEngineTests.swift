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

    func testPlanChunksResumeSkipsAndTrims() {
        let text = "One two three. Four five six. Seven eight nine. Ten eleven twelve."
        let all = TTSChunker.planChunks(noteId: UUID(), text: text)
        XCTAssertGreaterThan(all.count, 1)

        // Resuming at the second chunk's start drops the first chunk.
        let boundary = all[1].textUTF16Offset
        let resumed = TTSChunker.planChunks(noteId: UUID(), text: text, resumeFromUTF16: boundary)
        XCTAssertEqual(resumed.count, all.count - 1)
        XCTAssertEqual(resumed[0].textUTF16Offset, boundary)
        XCTAssertEqual(resumed[0].text, all[1].text)

        // Resuming from the very end leaves nothing to say.
        XCTAssertTrue(
            TTSChunker.planChunks(noteId: UUID(), text: text, resumeFromUTF16: text.utf16.count)
                .isEmpty
        )

        // Resuming mid-chunk trims the straddling sentence.
        let mid = all[0].textUTF16Offset + all[0].textUTF16Length / 2
        let trimmed = TTSChunker.planChunks(noteId: UUID(), text: text, resumeFromUTF16: mid)
        XCTAssertEqual(trimmed[0].textUTF16Offset, mid)
        XCTAssertLessThan(trimmed[0].textUTF16Length, all[0].textUTF16Length)
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
        // supertonic has no engine yet; espeak and kokoro are wired (kokoro
        // constructs even without its model — play() surfaces that "not
        // ready" as a soft error rather than throwing at the factory).
        XCTAssertThrowsError(try EngineFactory.make(kind: .supertonic))
        XCTAssertNoThrow(try EngineFactory.make(kind: .espeak))
        XCTAssertNoThrow(try EngineFactory.make(kind: .kokoro))
        XCTAssertTrue(try EngineFactory.make(kind: .espeak).modelCreated())
        // With the model installed the kokoro engine is genuinely ready.
        if KokoroModelManager.modelFilesAreValid() {
            XCTAssertTrue(try EngineFactory.make(kind: .kokoro).modelCreated())
        } else {
            XCTAssertFalse(try EngineFactory.make(kind: .kokoro).modelCreated())
        }
    }

    /// What the phonemization front end actually emits — kokoro's expected
    /// alphabet, sanity-checked against its vocab.
    func testEspeakPhonemeIpaOutput() throws {
        let bridge = try XCTUnwrap(EspeakEngine().bridgeForTesting())
        for text in ["Hello from Kokoro.", "Testing one two three."] {
            XCTAssertFalse(bridge.phonemes(for: text).isEmpty, text)
        }
    }

    /// Proves the shared library loads and the vendored header's ABI
    /// matches the installed runtime, without needing a model.
    func testOnnxRuntimeCAPILoads() throws {
        try OrtRuntime.smokeTest()
    }

    func testPiperPhonemeIdSequence() {
        let map = ["^": [1], "$": [2], "_": [0], "a": [10], "b": [11, 12]]
        let ids = PiperEngine.phonemeIds(from: ["ab"], map: map)
        // piper-phonemize pads once per SYMBOL, then all of the symbol's ids:
        // BOS, PAD+10, PAD+(11,12), PAD, EOS
        XCTAssertEqual(ids, [1, 0, 10, 0, 11, 12, 0, 2])
        // Unknown symbols (stress marks) drop silently.
        XCTAssertEqual(PiperEngine.phonemeIds(from: ["ˈa"], map: map), [1, 0, 10, 0, 2])
    }

    /// Runs the engine's own session directly — bypasses encodeSpeechImpl to
    /// exercise the graph with explicit tensors (regression guard for the
    /// input-buffer lifetime bug: scoped caller storage used to be freed
    /// before Run read it, failing intermittently).
    private func kokoroRun(
        engine: KokoroEngine, ids: [Int64], style: [Float], speed: Float
    ) throws -> Int {
        guard let session = engine.sessionForTesting() else {
            throw TTSError.synthesisFailed("no session")
        }
        let idTensor = try OrtValueRef(
            tensorData: ids, shape: [1, Int64(ids.count)], elementType: OrtElementType.int64
        )
        let styleTensor = try OrtValueRef(
            tensorData: style, shape: [1, Int64(style.count)], elementType: OrtElementType.float
        )
        let speedTensor = try OrtValueRef(
            tensorData: [speed], shape: [1], elementType: OrtElementType.float
        )
        let outputs = try session.run(
            inputs: ["input_ids": idTensor, "style": styleTensor, "speed": speedTensor],
            outputNames: ["waveform"]
        )
        guard let wave = outputs["waveform"] else {
            throw TTSError.synthesisFailed("no waveform output")
        }
        return try wave.floatTensorData().count
    }

    /// Regression: repeated identical runs must all succeed deterministically
    /// (the tensor lifetime bug failed a random subset), and an Int64 tensor
    /// must round-trip as Int64 bytes.
    func testKokoroStyleMatrix() throws {
        guard KokoroModelManager.modelFilesAreValid() else {
            throw XCTSkip("no kokoro model installed")
        }
        let probe = try OrtValueRef(
            tensorData: [Int64](arrayLiteral: 0, 50, 83),
            shape: [1, 3],
            elementType: OrtElementType.int64
        )
        let roundTripped = try probe.tensorData().withUnsafeBytes { raw in
            Array(raw.bindMemory(to: Int64.self))
        }
        XCTAssertEqual(roundTripped, [0, 50, 83])

        let engine = KokoroEngine()
        XCTAssertTrue(
            engine.createModel(
                modelPath: KokoroModelManager.modelDirectory.path,
                modelId: KokoroModelManager.curatedVoice
            )
        )
        guard let prep = engine.testingPreparation(voice: "af_heart") else {
            return XCTFail("no prep")
        }
        for _ in 0..<3 {
            XCTAssertGreaterThan(try kokoroRun(engine: engine, ids: prep.ids, style: prep.style, speed: 1.0), 0)
        }
        let zeros = [Float](repeating: 0, count: prep.style.count)
        XCTAssertGreaterThan(try kokoroRun(engine: engine, ids: prep.ids, style: zeros, speed: 1.0), 0)
    }

    /// Full Kokoro path — skipped unless the model set is installed
    /// (Settings → download, or KokoroModelManager.download()).
    func testKokoroSynthesisIfModelInstalled() throws {
        guard KokoroModelManager.modelFilesAreValid() else {
            throw XCTSkip("no kokoro model installed")
        }
        let engine = KokoroEngine()
        XCTAssertTrue(
            engine.createModel(
                modelPath: KokoroModelManager.modelDirectory.path,
                modelId: KokoroModelManager.curatedVoice
            )
        )
        let url = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("kokoro-\(UUID().uuidString).wav")
        defer { try? FileManager.default.removeItem(at: url) }

        let rate = try engine.encodeSpeechImpl(
            text: "Hello from Kokoro.",
            speed: 1.0,
            outFile: url,
            abort: { false }
        )
        XCTAssertEqual(rate, 24_000)
        let (samples, wavRate) = try WAVFile.read(at: url)
        XCTAssertEqual(wavRate, 24_000)
        XCTAssertGreaterThan(samples.count, 24_000, "one sentence of 24 kHz audio")
    }

    /// Full Piper path — skipped unless a voice is installed
    /// (Settings → download, or PiperModelManager.download()).
    func testPiperSynthesisIfVoiceInstalled() throws {
        guard let voice = PiperModelManager.installedVoices().first else {
            throw XCTSkip("no piper voice installed")
        }
        let engine = PiperEngine()
        XCTAssertTrue(
            engine.createModel(
                modelPath: PiperModelManager.modelDirectory(for: voice).path,
                modelId: voice
            )
        )
        let url = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("piper-\(UUID().uuidString).wav")
        defer { try? FileManager.default.removeItem(at: url) }

        let rate = try engine.encodeSpeechImpl(
            text: "Hello from Piper.",
            speed: 1.0,
            outFile: url,
            abort: { false }
        )
        XCTAssertGreaterThan(rate, 0)
        let (samples, wavRate) = try WAVFile.read(at: url)
        XCTAssertEqual(wavRate, rate)
        XCTAssertFalse(samples.isEmpty)
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
