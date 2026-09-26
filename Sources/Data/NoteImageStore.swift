import Foundation
import AppPaths

/// Per-note image files under `note-images/<uuid>/`.
///
/// iOS used CryptoKit for content hashes and ImageIO for thumbnails; Linux
/// Phase 2 only needs stable file naming and purge (djb2 filename hash,
/// original files kept as-is).
enum NoteImageStore {
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
