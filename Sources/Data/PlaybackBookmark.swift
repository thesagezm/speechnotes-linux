import Foundation

/// Resume point for one note/book playback, persisted per-item in
/// `bookmarks.json` (keys `note:<uuid>` / `book:<id>:<chapter>`).
///
/// iOS nests this type inside SpeechPlayer; on Linux it lives in the data
/// layer so BookmarkStore doesn't depend on audio. Offsets are UTF-16 units —
/// SentenceChunker's coordinate space, snapped to sentence boundaries by the
/// player when saved.
public struct PlaybackBookmark: Codable, Equatable, Sendable {
    public var noteId: UUID?
    public var bookId: String?
    public var chapterIndex: Int?
    public var textOffset: Int
    public var savedAt: Date

    public init(noteId: UUID? = nil, bookId: String? = nil, chapterIndex: Int? = nil,
         textOffset: Int = 0, savedAt: Date = Date()) {
        self.noteId = noteId
        self.bookId = bookId
        self.chapterIndex = chapterIndex
        self.textOffset = textOffset
        self.savedAt = savedAt
    }
}
