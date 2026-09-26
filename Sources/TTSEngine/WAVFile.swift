import Foundation

/// Minimal RIFF/WAVE codec for the chunk handoff between engines and the
/// player: 16-bit mono PCM in, PCM out. Engines write one WAV per sentence
/// chunk into the cache dir; the player reads them back, so a crash or a
/// device change can never corrupt the note text itself.
public enum WAVFile {
    public struct ParseError: Error, Equatable {
        public let reason: String
        public init(_ reason: String) { self.reason = reason }
    }

    /// Writes 16-bit mono PCM as a canonical 44-byte-header RIFF/WAVE file.
    public static func write(samples: [Int16], sampleRate: Int, to url: URL) throws {
        var data = Data(capacity: 44 + samples.count * 2)
        let dataLen = samples.count * 2

        func le32(_ v: UInt32) {
            var e = v.littleEndian
            withUnsafeBytes(of: &e) { data.append(contentsOf: $0) }
        }
        func le16(_ v: UInt16) {
            var e = v.littleEndian
            withUnsafeBytes(of: &e) { data.append(contentsOf: $0) }
        }

        data.append("RIFF".data(using: .ascii)!)
        le32(UInt32(36 + dataLen))
        data.append("WAVE".data(using: .ascii)!)
        data.append("fmt ".data(using: .ascii)!)
        le32(16)                    // fmt chunk size
        le16(1)                     // PCM
        le16(1)                     // mono
        le32(UInt32(sampleRate))
        le32(UInt32(sampleRate * 2)) // byte rate = rate * block align
        le16(2)                     // block align
        le16(16)                    // bits per sample
        data.append("data".data(using: .ascii)!)
        le32(UInt32(dataLen))
        samples.withUnsafeBytes { data.append(contentsOf: $0) }

        try data.write(to: url, options: .atomic)
    }

    /// Reads a WAV written by `write` (and tolerant of extra chunks other
    /// tools may insert — it walks the chunk list rather than trusting
    /// offsets).
    public static func read(at url: URL) throws -> (samples: [Int16], sampleRate: Int) {
        let data = try Data(contentsOf: url)
        guard data.count >= 44, data.prefix(4) == Data("RIFF".utf8),
              data.dropFirst(8).prefix(4) == Data("WAVE".utf8) else {
            throw ParseError("not a RIFF/WAVE file")
        }

        var offset = 12
        var sampleRate = 0
        var samples: [Int16] = []
        while offset + 8 <= data.count {
            let id = data.subdata(in: offset..<offset + 4)
            let size = Int(data.subdata(in: offset + 4..<offset + 8).withUnsafeBytes {
                $0.loadUnaligned(as: UInt32.self).littleEndian
            })
            let body = offset + 8
            guard body + size <= data.count else { break }
            if id == Data("fmt ".utf8), size >= 16 {
                sampleRate = Int(data.subdata(in: body + 4..<body + 8).withUnsafeBytes {
                    $0.loadUnaligned(as: UInt32.self).littleEndian
                })
            } else if id == Data("data".utf8) {
                let pcm = data.subdata(in: body..<body + size)
                samples = pcm.withUnsafeBytes { raw in
                    Array(raw.bindMemory(to: Int16.self))
                }
            }
            offset = body + size + (size % 2)  // chunks are word-aligned
        }

        guard sampleRate > 0 else { throw ParseError("missing fmt chunk") }
        return (samples, sampleRate)
    }
}
