import Foundation
import SpeechLogic

/// The Books library manifest model — byte-compatible with speechnotes-ios
/// `Book` (one manifest.json per book directory under Books/).
public enum BookFormat: String, Codable, Sendable {
    case epub
    case pdf
    /// A finished audiobook file (M4B / M4A / MP4 with a chpl atom, or MP3
    /// with ID3 CHAP frames). The audio already exists — Speechnotes plays
    /// it and keeps the chapter list for navigation, instead of synthesizing
    /// anything.
    case audio
}

/// Where the reader left off. EPUB: spine index + fraction inside the
/// chapter; PDF: page index + fraction inside the page. One shape keeps the
/// manifest and the resume logic format-agnostic. `cfi` is EPUB-only.
public struct BookPosition: Codable, Equatable, Hashable, Sendable {
    public var chapterIndex: Int
    public var chapterFraction: Double
    public var cfi: String?

    public init(chapterIndex: Int, chapterFraction: Double, cfi: String? = nil) {
        self.chapterIndex = chapterIndex
        self.chapterFraction = chapterFraction
        self.cfi = cfi
    }
}

/// One TOC row snapshotted at import time (epub: from EpubInfo's native
/// parse). PDF outlines resolve to chapters instead.
public struct BookTocEntry: Codable, Equatable, Hashable, Sendable {
    public var label: String
    /// Zip entry path the entry points at (epub only).
    public var href: String
    /// Index into the book's spine when it could be resolved.
    public var spineIndex: Int?
    /// Nesting level for the TOC tree (0 = top). Optional so manifests
    /// written before round 6 keep decoding.
    public var depth: Int?

    public init(label: String, href: String, spineIndex: Int? = nil, depth: Int? = nil) {
        self.label = label
        self.href = href
        self.spineIndex = spineIndex
        self.depth = depth
    }
}

/// The library's manifest record — ONE manifest.json per book directory.
/// Deliberately not notes.json: a shelf of 50 MB books must not share the
/// file the editor rewrites on every save flush.
public struct Book: Identifiable, Codable, Equatable, Hashable, Sendable {
    public let id: UUID
    public var title: String
    public var author: String?
    public var format: BookFormat
    public var originalFileName: String
    public var addedAt: Date
    public var lastOpenedAt: Date?
    /// epub: chapter (spine) count.
    public var spineCount: Int?
    /// epub: the spine's zip entry paths in reading order — the TTS chapter
    /// text pipeline reads chapters straight from the archive with these.
    public var spine: [String]?
    /// pdf: page count.
    public var pageCount: Int?
    /// pdf: speech chapters, resolved once at import.
    public var pdfChapters: [PdfChapter]?
    public var pdfChapterSource: String?
    /// audio: chapters read out of the file's own metadata (chpl / ID3 CHAP).
    public var audioChapters: [AudioChapter]?
    /// audio: where the chapters came from — "chpl", "id3" or "single".
    public var audioChapterSource: String?
    /// audio: total duration in seconds.
    public var audioDuration: Double?
    public var hasCover: Bool
    public var toc: [BookTocEntry]?
    public var position: BookPosition?
    /// Set at import when the book parsed badly — the shelf explains WHY
    /// instead of shelving a silent husk.
    public var importError: String?
    /// Soft-delete stamp for the books recycle bin.
    public var deletedAt: Date?

    public init(
        id: UUID,
        title: String,
        author: String? = nil,
        format: BookFormat,
        originalFileName: String,
        addedAt: Date = Date(),
        lastOpenedAt: Date? = nil,
        spineCount: Int? = nil,
        spine: [String]? = nil,
        pageCount: Int? = nil,
        pdfChapters: [PdfChapter]? = nil,
        pdfChapterSource: String? = nil,
        audioChapters: [AudioChapter]? = nil,
        audioChapterSource: String? = nil,
        audioDuration: Double? = nil,
        hasCover: Bool = false,
        toc: [BookTocEntry]? = nil,
        position: BookPosition? = nil,
        importError: String? = nil,
        deletedAt: Date? = nil
    ) {
        self.id = id
        self.title = title
        self.author = author
        self.format = format
        self.originalFileName = originalFileName
        self.addedAt = addedAt
        self.lastOpenedAt = lastOpenedAt
        self.spineCount = spineCount
        self.spine = spine
        self.pageCount = pageCount
        self.pdfChapters = pdfChapters
        self.pdfChapterSource = pdfChapterSource
        self.audioChapters = audioChapters
        self.audioChapterSource = audioChapterSource
        self.audioDuration = audioDuration
        self.hasCover = hasCover
        self.toc = toc
        self.position = position
        self.importError = importError
        self.deletedAt = deletedAt
    }

    /// True when the book sits in the recycle bin.
    public var isDeleted: Bool { deletedAt != nil }

    /// How long a binned book stays on disk. Same window as notes.
    public static let recycleRetentionDays = 30

    /// "Book Title — Chapter 3" style subtitle for the shelf row.
    public var authorOrFormat: String {
        if let author, !author.isEmpty { return author }
        switch format {
        case .epub: return "EPUB"
        case .pdf: return "PDF"
        case .audio: return "Audiobook"
        }
    }
}
