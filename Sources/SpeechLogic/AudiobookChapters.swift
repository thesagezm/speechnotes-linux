//
//  AudiobookChapters.swift
//  SpeechLogic
//
//  Chapter-list resolution for audiobook containers built on MPEG-4 or MP3
//  (M4B/M4A/MP4 and MP3-with-chapters). Two metadata formats matter in the
//  wild and both are covered here:
//
//   * **Nero / QuickTime chapter tracks and `chpl` atoms** — an MP4 chapter
//     list, either as a timed text track or an Apple `chpl` atom.
//   * **ID3v2 CHAP/CTOC frames** — the MP3 answer to the same problem, one
//     CHAP frame per chapter with its own title.
//
//  The parser reads only what it needs: boxes are walked by length prefix, so
//  a 900 MB audiobook is never loaded — the caller hands it a byte range. Only
//  integer decoding, big-endian box walking and ID3v2 frame splitting live
//  here; nothing in this file allocates in proportion to the file size.
//
//  This is a fresh implementation from the ISO/IEC 14496-12 box layout and the
//  ID3v2.3/2.4 informal spec. No third-party parser code.
//

import Foundation

/// What the container says about one chapter. Times are in SECONDS (fractional
/// where the container gives milliseconds) so the app can map playback time to
/// a chapter without knowing the container's native units.
public struct AudioChapter: Codable, Equatable, Hashable, Sendable {
    public var title: String
    public var startSeconds: Double
    public var endSeconds: Double

    public init(title: String, startSeconds: Double, endSeconds: Double) {
        self.title = title
        self.startSeconds = startSeconds
        self.endSeconds = endSeconds
    }
}

/// Resolution entry point. `data` must be the whole file for MP3 (ID3 frames
/// live at the head) and may be a HEAD slice for MP4 (the chapter boxes sit
/// near the start, before `mdat`).
public enum AudiobookChapters {

    /// Chapters from an MP4-family container (`.m4b`, `.m4a`, `.mp4`).
    ///
    /// Walks the top-level box tree looking for `moov`, then reads the `chpl`
    /// atom inside it (directly under moov, or one level down in moov/udta —
    /// ffmpeg nests it there; Nero encoders don't). The timestamps' unit is
    /// encoder-dependent, so the reading is chosen by plausibility against
    /// `totalSeconds` — same semantics as the iOS app's post-fix parser.
    public static func chaptersFromMP4(_ data: Data, totalSeconds: Double) -> [AudioChapter] {
        var chplPayload: Range<Int>?
        forEachBox(in: data, range: 0..<data.count) { type, payload in
            guard type == "moov" else { return }
            forEachBox(in: data, range: payload) { innerType, innerPayload in
                if innerType == "chpl" {
                    chplPayload = innerPayload
                } else if innerType == "udta" {
                    forEachBox(in: data, range: innerPayload) { deepType, deepPayload in
                        if deepType == "chpl" { chplPayload = deepPayload }
                    }
                }
            }
        }
        guard let chplPayload else {
            return chaptersFromMP4Tail(data, totalSeconds: totalSeconds)
        }
        let chapters = parseChpl(Array(data[chplPayload]), totalSeconds: totalSeconds)
        guard let normalized = normalize(chapters, totalSeconds: totalSeconds), !normalized.isEmpty else {
            return []
        }
        return normalized
    }

    /// `chaptersFromMP4` over a slice that does NOT start on a box boundary
    /// (a tail slice whose first bytes sit mid-mdat — plain-ffmpeg M4Bs put
    /// moov at the very end). Walks the payload for a `chpl` atom by
    /// signature; chpl's own structure (count + starts + Pascal titles)
    /// validates the hit.
    public static func chaptersFromMP4Tail(_ data: Data, totalSeconds: Double) -> [AudioChapter] {
        let bytes = Array(data)
        let signature = Array("chpl".utf8)
        var cursor = 0
        while let hit = bytes.firstRange(of: signature, in: cursor..<bytes.count) {
            let chapters = parseChpl(Array(bytes[hit.upperBound...]), totalSeconds: totalSeconds)
            if !chapters.isEmpty {
                return normalize(chapters, totalSeconds: totalSeconds) ?? chapters
            }
            cursor = hit.upperBound
        }
        return []
    }

    /// Chapters from ID3v2 `CHAP` frames (MP3). `CTOC` is read only to learn
    /// the intended order; with no CTOC the frames' own order is used.
    public static func chaptersFromID3(_ data: Data) -> [AudioChapter] {
        guard data.count > 10 else { return [] }
        let bytes = [UInt8](data.prefix(min(data.count, 4 * 1024 * 1024)))
        guard bytes[0] == 0x49, bytes[1] == 0x44, bytes[2] == 0x33 else { return [] } // "ID3"
        let major = Int(bytes[3])
        guard major == 3 || major == 4 else { return [] }
        let size = syncSafeInt(bytes[6], bytes[7], bytes[8], bytes[9])
        let tagEnd = min(bytes.count, 10 + size)
        var cursor = 10
        // An extended header, when the flag says there is one, must be skipped
        // rather than misread as the first frame. v2.4's size is sync-safe and
        // EXCLUDES its own four bytes; v2.3's is a plain 32-bit size that
        // INCLUDES them. Both encodings exist in the wild.
        if bytes[5] & 0x40 != 0, cursor + 4 <= tagEnd {
            let extended: Int
            if major == 4 {
                // v2.4's extended header is [4-byte sync-safe size][1-byte
                // flags][4-byte padding size] — the size comes FIRST and
                // counts only what follows it.
                extended = syncSafeInt(bytes[cursor], bytes[cursor + 1], bytes[cursor + 2], bytes[cursor + 3])
                cursor += 4 + extended
            } else {
                extended = Int(be32(bytes, cursor))
                cursor += extended
            }
        }

        var chapters: [AudioChapter] = []
        while cursor + 10 <= tagEnd {
            let id = String(bytes: bytes[cursor..<(cursor + 4)], encoding: .isoLatin1) ?? ""
            guard id != "\0\0\0\0", !id.isEmpty else { break }
            let frameSize: Int
            if major == 4 {
                frameSize = syncSafeInt(bytes[cursor + 4], bytes[cursor + 5], bytes[cursor + 6], bytes[cursor + 7])
            } else {
                frameSize = Int(bytes[cursor + 4]) << 24 | Int(bytes[cursor + 5]) << 16
                    | Int(bytes[cursor + 6]) << 8 | Int(bytes[cursor + 7])
            }
            let payloadStart = cursor + 10
            let payloadEnd = min(tagEnd, payloadStart + frameSize)
            guard payloadEnd > payloadStart else { break }
            if id == "CHAP" {
                if let chapter = parseChapFrame(Array(bytes[payloadStart..<payloadEnd])) {
                    chapters.append(chapter)
                }
            }
            cursor = payloadEnd
        }
        return normalize(chapters, totalSeconds: nil) ?? chapters
    }

    // MARK: - MP4 walking

    /// Calls `body` for every box at this level. A box is a 4-byte big-endian
    /// length, a 4-byte type, then the payload; length 1 means a 64-bit
    /// extended length follows, length 0 means "to end of file". A malformed
    /// length ends the walk instead of looping.
    private static func forEachBox(
        in data: Data,
        range: Range<Int>,
        _ body: (String, Range<Int>) -> Void
    ) {
        var cursor = range.lowerBound
        while cursor + 8 <= range.upperBound {
            let length = Int(be32(data, cursor))
            let type = fourCC(data, cursor + 4)
            var headerSize = 8
            var boxLength = length
            if length == 1 {
                guard cursor + 16 <= range.upperBound else { return }
                boxLength = Int(be64(data, cursor + 8))
                headerSize = 16
            } else if length == 0 {
                boxLength = range.upperBound - cursor
            }
            guard boxLength >= headerSize, cursor + boxLength <= range.upperBound else { return }
            body(type, (cursor + headerSize)..<(cursor + boxLength))
            cursor += boxLength
        }
    }

    // MARK: - chpl

    /// `chpl` payload reader. Two real-world layouts exist:
    ///   * Nero spec — a 4-byte count at offset 4, entries from offset 8;
    ///   * ffmpeg — 4 bytes of version/flags/reserved, a 1-byte count at
    ///     offset 8, entries from offset 9.
    /// Both are parsed and the first with entries wins. The entries' unit is
    /// encoder-dependent (ffmpeg writes 100-ns units; other tools wrote
    /// milliseconds), so with a known duration the divisor is the plausible
    /// one CLOSEST to the book's length; without one, 100 ns (what both
    /// ffmpeg and modern Nero files carry — the "VLC saw chapters we didn't"
    /// lesson from the iOS port).
    private static func parseChpl(_ bytes: [UInt8], totalSeconds: Double?) -> [AudioChapter] {
        func entries(count: Int, cursor: Int) -> (starts: [Int], titles: [String]) {
            var starts: [Int] = []
            var titles: [String] = []
            var cursor = cursor
            for _ in 0..<count {
                guard cursor + 9 <= bytes.count else { break }
                var start = 0
                for shift in stride(from: 56, through: 0, by: -8) {
                    start |= Int(bytes[cursor]) << shift
                    cursor += 1
                }
                let titleLength = Int(bytes[cursor])
                cursor += 1
                guard cursor + titleLength <= bytes.count else { break }
                let title = String(
                    bytes: bytes[cursor..<(cursor + titleLength)],
                    encoding: .utf8
                ) ?? ""
                cursor += titleLength
                starts.append(start)
                titles.append(title.trimmingCharacters(in: .whitespacesAndNewlines))
            }
            return (starts, titles)
        }

        var candidates: [(starts: [Int], titles: [String])] = []
        // Variant A — the 4-byte Nero count.
        if bytes.count >= 8 {
            let count = Int(bytes[4]) << 24 | Int(bytes[5]) << 16
                | Int(bytes[6]) << 8 | Int(bytes[7])
            if count > 0, count < 10_000 {
                let parsed = entries(count: count, cursor: 8)
                if !parsed.starts.isEmpty { candidates.append(parsed) }
            }
        }
        // Variant B — ffmpeg's 1-byte count after 4 zero bytes.
        if candidates.isEmpty, bytes.count >= 9 {
            let count = Int(bytes[8])
            if count > 0, count < 255 {
                let parsed = entries(count: count, cursor: 9)
                if !parsed.starts.isEmpty { candidates.append(parsed) }
            }
        }
        guard let (rawStarts, titles) = candidates.first, !rawStarts.isEmpty else { return [] }

        let maxStart = rawStarts.max() ?? 0
        let divisor: Double
        if let total = totalSeconds, total > 0 {
            let nsSeconds = Double(maxStart) / 10_000_000
            let msSeconds = Double(maxStart) / 1_000
            let nsFits = nsSeconds <= total * 2
            let msFits = msSeconds <= total * 2
            switch (nsFits, msFits) {
            case (true, true):
                divisor = abs(nsSeconds - total) <= abs(msSeconds - total) ? 10_000_000 : 1_000
            case (false, true):
                divisor = 1_000
            default:
                divisor = 10_000_000
            }
        } else {
            divisor = 10_000_000
        }

        var chapters: [AudioChapter] = []
        chapters.reserveCapacity(rawStarts.count)
        for (index, rawStart) in rawStarts.enumerated() {
            chapters.append(AudioChapter(
                title: titles[index],
                startSeconds: Double(rawStart) / divisor,
                endSeconds: 0
            ))
        }
        return chapters
    }

    // MARK: - ID3 CHAP

    /// CHAP payload: element id (null-terminated), start (4), end (4),
    /// start offset (4), end offset (4), then a sub-frame list whose TIT2
    /// (v2.3) / TIT2 (v2.4) is the chapter name.
    private static func parseChapFrame(_ bytes: [UInt8]) -> AudioChapter? {
        var cursor = 0
        while cursor < bytes.count, bytes[cursor] != 0 { cursor += 1 }
        guard cursor < bytes.count else { return nil }
        cursor += 1 // NUL
        guard cursor + 16 <= bytes.count else { return nil }
        let startMS = Int(be32(bytes, cursor))
        let endMS = Int(be32(bytes, cursor + 4))
        cursor += 16
        var title = ""
        while cursor + 10 <= bytes.count {
            let id = String(bytes: bytes[cursor..<(cursor + 4)], encoding: .isoLatin1) ?? ""
            let size = Int(bytes[cursor + 4]) << 24 | Int(bytes[cursor + 5]) << 16
                | Int(bytes[cursor + 6]) << 8 | Int(bytes[cursor + 7])
            let payloadStart = cursor + 10
            let payloadEnd = min(bytes.count, payloadStart + size)
            guard payloadEnd > payloadStart else { break }
            if id == "TIT2" {
                title = decodeTextFrame(Array(bytes[payloadStart..<payloadEnd]))
            }
            cursor = payloadEnd
        }
        return AudioChapter(
            title: title.trimmingCharacters(in: .whitespacesAndNewlines),
            startSeconds: Double(startMS) / 1000.0,
            endSeconds: Double(endMS) / 1000.0
        )
    }

    /// A text frame is an encoding byte then the payload; UTF-8 with a BOM and
    /// UTF-16 are both common in the wild.
    private static func decodeTextFrame(_ bytes: [UInt8]) -> String {
        guard let encodingByte = bytes.first else { return "" }
        let payload = Array(bytes.dropFirst())
        let data = Data(payload)
        switch encodingByte {
        case 0: return String(data: data, encoding: .isoLatin1) ?? ""
        case 1: return String(data: data, encoding: .utf16) ?? ""
        case 2: return String(data: data, encoding: .utf16BigEndian) ?? ""
        case 3: return String(data: data, encoding: .utf8) ?? ""
        default: return String(data: data, encoding: .utf8) ?? ""
        }
    }

    // MARK: - Shared

    /// Fills in missing end times (each chapter ends where the next starts),
    /// gives untitled chapters a positional name, and keeps only entries with
    /// a real start. Returns nil when there is nothing usable.
    private static func normalize(_ chapters: [AudioChapter], totalSeconds: Double?) -> [AudioChapter]? {
        let usable = chapters
            .filter { $0.startSeconds >= 0 }
            .sorted { $0.startSeconds < $1.startSeconds }
        guard !usable.isEmpty else { return nil }
        var out: [AudioChapter] = []
        out.reserveCapacity(usable.count)
        for (index, chapter) in usable.enumerated() {
            var fixed = chapter
            if index + 1 < usable.count {
                if fixed.endSeconds <= fixed.startSeconds {
                    fixed.endSeconds = usable[index + 1].startSeconds
                }
            } else if let totalSeconds, totalSeconds > fixed.startSeconds {
                if fixed.endSeconds <= fixed.startSeconds {
                    fixed.endSeconds = totalSeconds
                }
            }
            if fixed.endSeconds <= fixed.startSeconds {
                fixed.endSeconds = fixed.startSeconds + 1
            }
            if fixed.title.isEmpty {
                fixed.title = "Chapter \(index + 1)"
            }
            out.append(fixed)
        }
        return out
    }

    // MARK: - Byte helpers

    private static func be32(_ data: Data, _ offset: Int) -> UInt32 {
        var value: UInt32 = 0
        for index in 0..<4 { value = (value << 8) | UInt32(data[offset + index]) }
        return value
    }

    private static func be32(_ bytes: [UInt8], _ offset: Int) -> UInt32 {
        var value: UInt32 = 0
        for index in 0..<4 { value = (value << 8) | UInt32(bytes[offset + index]) }
        return value
    }

    private static func be64(_ data: Data, _ offset: Int) -> UInt64 {
        var value: UInt64 = 0
        for index in 0..<8 { value = (value << 8) | UInt64(data[offset + index]) }
        return value
    }

    private static func fourCC(_ data: Data, _ offset: Int) -> String {
        String(bytes: data[offset..<(offset + 4)], encoding: .isoLatin1) ?? ""
    }

    /// ID3 sizes are "sync-safe": 7 bits per byte, high bit always clear.
    private static func syncSafeInt(_ a: UInt8, _ b: UInt8, _ c: UInt8, _ d: UInt8) -> Int {
        (Int(a & 0x7F) << 21) | (Int(b & 0x7F) << 14) | (Int(c & 0x7F) << 7) | Int(d & 0x7F)
    }
}
