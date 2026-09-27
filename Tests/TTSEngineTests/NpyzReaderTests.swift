import XCTest
@testable import TTSEngine

/// The real Kokoro path, end to end, against a real voice bank: zip → npy →
/// float arrays, plus the phoneme tokenization the model expects. The ONNX
/// model itself is only pulled in by the download (340 MB), so those tests
/// skip when it's absent — the parser is proven either way.
final class NpyzReaderTests: XCTestCase {

    /// voices.npz sitting next to the sources (test bundle resource) or in
    /// the models dir. Skips when neither is present.
    private func voicesPath() throws -> String? {
        for candidate in [
            ProcessInfo.processInfo.environment["KOKORO_VOICES"],
            KokoroModelManager.modelDirectory.appendingPathComponent("voices.npz").path,
        ].compactMap({ $0 }) {
            if FileManager.default.fileExists(atPath: candidate) { return candidate }
        }
        return nil
    }

    func testParseSyntheticNpyArray() throws {
        // A hand-built v1 npy: header then 3 float32 values.
        let floats: [Float] = [1.5, -2.0, 0.0]
        var payload = Data([0x93, 0x4E, 0x55, 0x4D, 0x50, 0x59])  // \x93NUMPY
        let header = "{'descr': '<f4', 'fortran_order': False, 'shape': (3,), }"
        var padded = Data(header.utf8)
        padded.append(UInt8(0x0A))
        while (10 + padded.count) % 64 != 0 { padded.append(UInt8(0x20)) }
        payload.append(contentsOf: [UInt8(0x01), UInt8(0x00)])
        payload.append(contentsOf: UInt16(padded.count).littleEndianBytes)
        payload.append(padded)
        for f in floats { payload.append(contentsOf: f.bitPattern.littleEndianBytes) }

        let parsed = try NpyArray.parseFloats(from: payload)
        XCTAssertEqual(parsed, floats)
    }

    func testReaderParsesRealVoicesBank() throws {
        guard let path = try voicesPath() else {
            throw XCTSkip("no voices.npz available")
        }
        guard let voices = NpyzReader.read(fileFromPath: path) else {
            XCTFail("voices bank failed to parse")
            return
        }
        XCTAssertFalse(voices.isEmpty)
        // af_heart is a [510, 1, 256] bank — 130,560 floats verbatim.
        guard let heart = voices["af_heart.npy"] else {
            XCTFail("af_heart.npy missing")
            return
        }
        XCTAssertEqual(heart.count, 510 * 1 * 256)
        let sum = heart.reduce(0, +)
        XCTAssertNotEqual(sum, 0, "voice data must not be silent")
    }
}

private extension FixedWidthInteger {
    var littleEndianBytes: [UInt8] {
        withUnsafeBytes(of: littleEndian) { Array($0) }
    }
}
