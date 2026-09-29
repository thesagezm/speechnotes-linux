import Foundation
import AppPaths

/// Per-note image files under `note-images/<uuid>/`.
///
/// iOS used CryptoKit for content hashes and ImageIO for thumbnails; Linux
/// Phase 2 only needs stable file naming and purge (djb2 filename hash,
/// original files kept as-is).
enum NoteImageStore {
    /// The markdown target form stored inside note bodies.
    static let scheme = "speechnotes"
    static let pathPrefix = "note-image"

    /// Stores image bytes under the note's image dir and returns the
    /// markdown target the body should reference. Raw bytes (no HEIC
    /// re-encode on Linux — files stay byte-identical across devices).
    static func importImageData(
        _ data: Data,
        pathExtension rawExt: String?,
        noteId: UUID
    ) -> String? {
        let ext = normalizedExtension(rawExt, of: data)
        guard let ext else { return nil }
        let dir = directory(for: noteId)
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        } catch {
            return nil
        }
        let name = preferredName(for: data, fileExtension: ext)
        let target = dir.appendingPathComponent(name)
        if !FileManager.default.fileExists(atPath: target.path) {
            do {
                try data.write(to: target, options: .atomic)
            } catch {
                return nil
            }
        }
        return "\(scheme)://\(pathPrefix)/\(name)"
    }

    /// Magic-byte sniffing first, extension second; nil when neither
    /// produces something plausible.
    static func normalizedExtension(_ raw: String?, of data: Data) -> String? {
        if data.count >= 4 {
            let b = [UInt8](data.prefix(4))
            if b[0] == 0x89, b[1] == 0x50, b[2] == 0x4E, b[3] == 0x47 { return "png" }
            if b[0] == 0xFF, b[1] == 0xD8 { return "jpg" }
            if b[0] == 0x47, b[1] == 0x49, b[2] == 0x46 { return "gif" }
        }
        var ext = (raw ?? "").lowercased()
        if ext.hasPrefix(".") { ext.removeFirst() }
        guard !ext.isEmpty, ext.count <= 8,
              ext.allSatisfy({ $0.isLetter || $0.isNumber }) else { return nil }
        return ext
    }

    static func removeAllImages(for noteId: UUID) {
        let dir = AppPaths.noteImagesDir.appendingPathComponent(noteId.uuidString, isDirectory: true)
        try? FileManager.default.removeItem(at: dir)
    }

    static func directory(for noteId: UUID) -> URL {
        AppPaths.noteImagesDir.appendingPathComponent(noteId.uuidString, isDirectory: true)
    }

    /// djb2 content hash → filename. Same algorithm iOS uses, so identical
    /// bytes map to identical names across the family.
    static func preferredName(for data: Data, fileExtension ext: String) -> String {
        var hash: UInt64 = 5381
        for byte in data {
            hash = ((hash << 5) &+ hash) &+ UInt64(byte)
        }
        return String(hash, radix: 16) + "." + ext
    }
}
