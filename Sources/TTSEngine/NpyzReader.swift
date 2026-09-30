import Foundation

/// Minimal `.npz` (NumPy zip archive) reader — just enough to load Kokoro's
/// `voices.npz`: a zip whose members are `.npy` arrays named `af_heart.npy`,
/// `am_eric.npy`, … each a float32 [rows, 256] style-vector bank. The iOS app
/// got this from an external NpyzReader package; the format is small and
/// stable, so the ~80 lines live here.
public enum NpyzReader {
    public enum ReadError: Error, Equatable {
        case notAZip
        case badCentralDirectory
        case badNpy(String)
    }

    /// Just the member names (e.g. ["af_heart.npy", …]) — walks the central
    /// directory without inflating any arrays. The voice picker lists 54
    /// Kokoro voices this way instead of loading ~15 MB of style vectors.
    public static func names(fileFromPath path: String) -> [String] {
        guard let data = FileManager.default.contents(atPath: path),
              let eocd = data.range(of: Data("PK\u{5}\u{6}".utf8)) else { return [] }
        let centralDirectoryOffset = Int(readLittleEndian(data, at: eocd.lowerBound + 16, count: 4))
        var cursor = centralDirectoryOffset
        var result: [String] = []
        while cursor + 46 <= data.count,
              data[cursor..<(cursor + 4)] == Data("PK\u{1}\u{2}".utf8) {
            let nameLength = Int(readLittleEndian(data, at: cursor + 28, count: 2))
            let extraLength = Int(readLittleEndian(data, at: cursor + 30, count: 2))
            let commentLength = Int(readLittleEndian(data, at: cursor + 32, count: 2))
            if nameLength > 0,
               let name = String(data: data[(cursor + 46)..<(cursor + 46 + nameLength)], encoding: .utf8),
               name.hasSuffix(".npy"),
               let member = name.split(separator: "/").last {
                result.append(String(member))
            }
            cursor += 46 + nameLength + extraLength + commentLength
        }
        return result
    }

    /// Parses every `.npy` member into a flat float32 array, keyed by the
    /// member name (e.g. "af_heart.npy"). Unreadable members are skipped.
    public static func read(fileFromPath path: String) -> [String: [Float]]? {
        guard let data = FileManager.default.contents(atPath: path) else { return nil }
        return try? parse(data)
    }

    public static func parse(_ data: Data) throws -> [String: [Float]] {
        guard let eocd = data.range(of: Data("PK\u{5}\u{6}".utf8)) else {
            throw ReadError.notAZip
        }
        var result: [String: [Float]] = [:]

        // The end-of-central-directory record carries the central
        // directory's absolute offset (its own size varies with the comment).
        let centralDirectoryOffset = Int(readLittleEndian(data, at: eocd.lowerBound + 16, count: 4))
        var cursor = centralDirectoryOffset
        while cursor + 46 <= data.count,
              data[cursor..<(cursor + 4)] == Data("PK\u{1}\u{2}".utf8) {
            // compressedSize comes from the LOCAL header (see below)
            let nameLength = Int(readLittleEndian(data, at: cursor + 28, count: 2))
            let extraLength = Int(readLittleEndian(data, at: cursor + 30, count: 2))
            let commentLength = Int(readLittleEndian(data, at: cursor + 32, count: 2))
            let localOffset = Int(readLittleEndian(data, at: cursor + 42, count: 4))

            guard nameLength > 0, localOffset + 30 <= data.count,
                  let name = String(data: data[(cursor + 46)..<(cursor + 46 + nameLength)], encoding: .utf8) else {
                throw ReadError.badCentralDirectory
            }

            let localNameLength = Int(readLittleEndian(data, at: localOffset + 26, count: 2))
            let localExtraLength = Int(readLittleEndian(data, at: localOffset + 28, count: 2))
            // The local header's size fields are 0xFFFFFFFF when a zip64
            // extra block carries the real value — which NumPy's writer
            // emits even for small members, so voices.npz hits this path.
            let localSize = Self.zip64Size(
                data,
                extraStart: localOffset + 30 + localNameLength,
                extraLength: localExtraLength
            ) ?? Int(readLittleEndian(data, at: localOffset + 18, count: 4))
            let bodyStart = localOffset + 30 + localNameLength + localExtraLength
            let bodyEnd = bodyStart + localSize
            guard bodyEnd <= data.count else { throw ReadError.badCentralDirectory }

            if let memberName = name.split(separator: "/").last.map(String.init),
               memberName.hasSuffix(".npy"),
               let floats = try? NpyArray.parseFloats(from: data[bodyStart..<bodyEnd]) {
                result[memberName] = floats
            }

            cursor += 46 + nameLength + extraLength + commentLength
        }
        return result
    }

    /// Reads the compressed size from a local extra field's zip64 block
    /// (id 0x0001), which stores the 8-byte uncompressed size first and the
    /// compressed size right after it.
    private static func zip64Size(_ data: Data, extraStart: Int, extraLength: Int) -> Int? {
        var offset = extraStart
        let limit = extraStart + extraLength
        while offset + 4 <= limit {
            let id = Int(readLittleEndian(data, at: offset, count: 2))
            let blockLength = Int(readLittleEndian(data, at: offset + 2, count: 2))
            if id == 0x0001, offset + 4 + 16 <= data.count {
                return Int(readLittleEndian(data, at: offset + 12, count: 8))
            }
            offset += 4 + blockLength
        }
        return nil
    }

    static func readLittleEndian(_ data: Data, at offset: Int, count: Int) -> UInt64 {
        var value: UInt64 = 0
        for index in 0..<count where data.count > offset + index {
            value |= UInt64(data[offset + index]) << (8 * index)
        }
        return value
    }
}

/// One `.npy` member: the header dictionary is ASCII (shape + dtype), and
/// v1 pads it to a 16-byte boundary before the raw little-endian data.
enum NpyArray {
    enum NpyError: Error, Equatable {
        case badMagic
        case unsupportedDtype(String)
        case shortHeader
    }

    /// npy's magic: the raw byte 0x93 followed by "NUMPY". Swift string
    /// literals hold Unicode scalars, so this can't be written as a String
    /// (U+0093 encodes to two UTF-8 bytes) — compare bytes instead.
    private static let numpyMagic: [UInt8] = [0x93, 0x4E, 0x55, 0x4D, 0x50, 0x59]

    /// Parses a float32 (or float64, downcast) array into flat Swift floats.
    /// Only C-order arrays are supported — the voice banks are.
    static func parseFloats(from data: Data) throws -> [Float] {
        // Sliced Data keeps its parent's absolute indices; rebind so the
        // fixed npy offsets below (0-based) address the member itself.
        var member = Data()
        member.reserveCapacity(data.count)
        for byte in data { member.append(byte) }

        guard member.count >= 10, Array(member.prefix(6)) == Self.numpyMagic else {
            throw NpyError.badMagic
        }
        let major = member[member.startIndex + 6]
        let headerLength: Int
        let headerStart: Int
        if major == 1 {
            headerLength = Int(NpyzReader.readLittleEndian(member, at: member.startIndex + 8, count: 2))
            headerStart = 10
        } else {
            headerLength = Int(NpyzReader.readLittleEndian(member, at: member.startIndex + 8, count: 4))
            headerStart = 12
        }
        let headerEnd = member.startIndex + headerStart + headerLength
        guard headerEnd <= member.count else { throw NpyError.shortHeader }
        let header = String(
            decoding: member[(member.startIndex + headerStart)..<headerEnd],
            as: UTF8.self
        )
        guard header.contains("'fortran_order': False") else { throw NpyError.badMagic }
        let body = member[headerEnd...]

        if header.contains("'descr': '<f4'") {
            return body.withUnsafeBytes { raw in Array(raw.bindMemory(to: Float.self)) }
        }
        if header.contains("'descr': '<f8'") {
            return body.withUnsafeBytes { raw in
                raw.bindMemory(to: Double.self).map(Float.init)
            }
        }
        let dtype = header.components(separatedBy: "'descr': ").last?
            .components(separatedBy: "'").first ?? "?"
        throw NpyError.unsupportedDtype(dtype)
    }
}
