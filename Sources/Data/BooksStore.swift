import Foundation
import SwiftCrossUI
import AppPaths
import SpeechLogic
import Log

/// The Books library. Each book is a self-contained directory under
/// AppPaths.booksDir holding the original file, a manifest.json, a cover
/// image and (from the TTS phase on) cached per-chapter speech text.
///
/// Ported from speechnotes-ios BooksStore: one manifest PER BOOK, never one
/// big file — the notes store rewrites its whole JSON on every save flush,
/// which is fine for notes and would be wasteful next to 50 MB books.
/// Scanning the shelf means decoding a handful of tiny manifests.
@MainActor
public final class BooksStore: ObservableObject {
    public static let shared = BooksStore()

    /// Everything on disk, active or binned — the raw shelf + the bin.
    @Published public private(set) var allBooks: [Book] = []
    /// On-shelf books only — what the UI reads as `books.books`.
    public var books: [Book] { allBooks.filter { !$0.isDeleted } }
    /// Binned books, most recently deleted first — the books recycle bin.
    public var deletedBooks: [Book] {
        allBooks
            .filter { $0.isDeleted }
            .sorted { ($0.deletedAt ?? .distantPast) > ($1.deletedAt ?? .distantPast) }
    }

    @Published public private(set) var isImporting = false
    @Published public var importError: String?

    private var shelfVersion: Int = 0

    // MARK: - Locations

    nonisolated public static var booksDirectory: URL {
        AppPaths.booksDir
    }

    nonisolated public static func bookDirectory(_ id: UUID) -> URL {
        booksDirectory.appendingPathComponent(id.uuidString, isDirectory: true)
    }

    public static func originalFileURL(_ book: Book) -> URL {
        bookDirectory(book.id).appendingPathComponent("original.\(book.format.rawValue)")
    }

    /// The audio book's file, resolved to whatever name it actually carries
    /// (new imports store the true extension; legacy layouts probed here).
    public static func resolveAudioOriginalURL(book: Book) -> URL {
        let dir = bookDirectory(book.id)
        for ext in ["m4b", "m4a", "mp3", "mp4"]
        where FileManager.default.fileExists(atPath: dir.appendingPathComponent("original.\(ext)").path) {
            return dir.appendingPathComponent("original.\(ext)")
        }
        return dir.appendingPathComponent("original.audio")
    }

    public static func coverFileURL(_ book: Book) -> URL {
        bookDirectory(book.id).appendingPathComponent("cover.jpg")
    }

    /// Cached speech text for one chapter (written by the TTS pipeline; the
    /// reader never has to re-extract a chapter it already spoke).
    public static func speechTextURL(_ book: Book, chapterIndex: Int) -> URL {
        bookDirectory(book.id)
            .appendingPathComponent("text", isDirectory: true)
            .appendingPathComponent(String(format: "%04d.txt", chapterIndex))
    }

    nonisolated public static func manifestURL(_ id: UUID) -> URL {
        bookDirectory(id).appendingPathComponent("manifest.json")
    }

    // MARK: - Shelf

    public init() {
        refresh()
        pruneExpiredBooks()
        Log.info("BooksStore ready with \(books.count) book(s), \(deletedBooks.count) in bin")
    }

    /// Manifest decode only: a directory listing plus N tiny decodes.
    public func refresh() {
        let fm = FileManager.default
        guard let dirs = try? fm.contentsOfDirectory(at: Self.booksDirectory, includingPropertiesForKeys: nil) else {
            return
        }
        var shelf: [Book] = []
        for dir in dirs {
            let manifest = dir.appendingPathComponent("manifest.json")
            guard let data = try? Data(contentsOf: manifest),
                  let book = try? JSONDecoder().decode(Book.self, from: data) else { continue }
            shelf.append(book)
        }
        allBooks = shelf.sorted {
            ($0.lastOpenedAt ?? $0.addedAt) > ($1.lastOpenedAt ?? $1.addedAt)
        }
        shelfVersion += 1
    }

    /// Binned books older than the retention window are purged from disk.
    /// Only books WITH a deletedAt stamp can expire — an active book never
    /// carries one, so it must never match.
    public func pruneExpiredBooks() {
        let cutoff = Date().addingTimeInterval(-Double(Book.recycleRetentionDays) * 86_400)
        let expired = allBooks.filter { book in
            guard let deletedAt = book.deletedAt else { return false }
            return deletedAt < cutoff
        }
        guard !expired.isEmpty else { return }
        for book in expired {
            try? FileManager.default.removeItem(at: Self.bookDirectory(book.id))
        }
        allBooks.removeAll { book in expired.contains(where: { $0.id == book.id }) }
        shelfVersion += 1
    }

    // MARK: - Import

    private enum ImportOutcome {
        case imported(Book)
        case failed(String)
    }

    /// Imports one book from a URL (the GTK file picker hands these over).
    /// Everything — the copy and the metadata parse — runs detached so a
    /// multi-GB audiobook can never freeze the UI (the 2.9 GB Harry Potter
    /// import used to block the main thread for its whole copy). Metadata
    /// failure downgrades to a filename-titled book — a book that parses
    /// badly is still a book. Returns the imported book, or nil when the
    /// copy/manifest write failed (importError is set).
    @discardableResult
    public func importBook(from sourceURL: URL) async -> Book? {
        isImporting = true
        defer { isImporting = false }

        let ext = sourceURL.pathExtension.lowercased()
        guard let format = Self.format(forExtension: ext) else {
            importError = "Unsupported book format: .\(ext)"
            return nil
        }

        let id = UUID()
        let dir = Self.bookDirectory(id)
        let fileName = sourceURL.lastPathComponent

        let outcome: ImportOutcome = await Task.detached(priority: .userInitiated) {
            do {
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                // Audio books keep their TRUE extension — players/decoders
                // map a URL to its container parser largely by extension.
                let destinationExtension = format == .audio ? ext : format.rawValue
                let destination = dir.appendingPathComponent("original.\(destinationExtension)")
                try Self.materializeSource(at: sourceURL, to: destination)
                let parsed = Self.buildManifest(
                    format: format,
                    id: id,
                    originalFileName: fileName,
                    directory: dir
                )
                let data = try JSONEncoder().encode(parsed)
                try data.write(to: Self.manifestURL(id), options: .atomic)
                return .imported(parsed)
            } catch {
                try? FileManager.default.removeItem(at: dir)
                return .failed("\(fileName): \(error.localizedDescription)")
            }
        }.value

        switch outcome {
        case .imported(let book):
            refresh()
            return book
        case .failed(let message):
            importError = "Could not import \(message)"
            return nil
        }
    }

    /// Hardlinks the source when it's on the same filesystem — a 3 GB
    /// audiobook then imports instantly and costs no extra space. Anything
    /// else (portal FUSE paths, other partitions) falls back to a real copy.
    nonisolated static func materializeSource(at sourceURL: URL, to destination: URL) throws {
        let fm = FileManager.default
        if fm.fileExists(atPath: destination.path) {
            try fm.removeItem(at: destination)
        }
        do {
            try fm.linkItem(at: sourceURL, to: destination)
        } catch {
            try fm.copyItem(at: sourceURL, to: destination)
        }
    }

    nonisolated static func format(forExtension ext: String) -> BookFormat? {
        switch ext {
        case "epub": return .epub
        case "pdf": return .pdf
        case "m4b", "m4a", "mp4", "mp3": return .audio
        default: return nil
        }
    }

    nonisolated static func buildManifest(
        format: BookFormat,
        id: UUID,
        originalFileName: String,
        directory: URL
    ) -> Book {
        switch format {
        case .epub: return buildEpubManifest(id: id, originalFileName: originalFileName, directory: directory)
        case .pdf: return buildPdfManifest(id: id, originalFileName: originalFileName, directory: directory)
        case .audio: return buildAudioManifest(id: id, originalFileName: originalFileName, directory: directory)
        }
    }

    /// Runs detached. Reads only a few zip entries — never the whole book
    /// into memory.
    nonisolated static func buildEpubManifest(id: UUID, originalFileName: String, directory: URL) -> Book {
        let fallbackTitle = (originalFileName as NSString)
            .deletingPathExtension
            .replacingOccurrences(of: "_", with: " ")
        var book = Book(id: id, title: fallbackTitle, format: .epub, originalFileName: originalFileName)

        guard let data = try? Data(contentsOf: directory.appendingPathComponent("original.epub"), options: .mappedIfSafe) else {
            book.importError = "The EPUB file could not be read."
            return book
        }
        let info: EpubInfo
        do {
            info = try EpubParser.parse(archive: data)
        } catch let ZipReader.ZipError.encryptedEntry(name) {
            book.importError = "DRM-protected (encrypted \(name)) — can't be read aloud."
            return book
        } catch {
            book.importError = "This EPUB is malformed — chapters and speech may be unavailable."
            return book
        }
        book.title = info.title?.isEmpty == false ? info.title! : fallbackTitle
        book.author = info.creator?.isEmpty == false ? info.creator : nil
        book.spineCount = info.spine.isEmpty ? nil : info.spine.count
        // The TTS pipeline reads chapters straight from the archive with
        // these paths — no webview needed for speech.
        book.spine = info.spine.isEmpty ? nil : info.spine
        if let coverPath = info.coverPath,
           let coverData = try? ZipReader.readEntry(coverPath, in: data),
           !coverData.isEmpty {
            try? coverData.write(to: directory.appendingPathComponent("cover.jpg"), options: .atomic)
            book.hasCover = true
        }
        // Snapshot the TOC with spine indices so the reader can navigate
        // without a webview. Anchors whose href missed the spine are kept
        // but unlinked.
        if !info.toc.isEmpty {
            let indexByHref = Dictionary(info.spine.enumerated().map { ($1, $0) },
                                         uniquingKeysWith: { first, _ in first })
            book.toc = info.toc.map { entry in
                BookTocEntry(label: entry.label, href: entry.href, spineIndex: indexByHref[entry.href])
            }
        }
        return book
    }

    /// PDF manifest: metadata + page count via pdfinfo, chapter floor via
    /// page-range grouping (poppler exposes no outline), cover = page 1
    /// rendered by pdftoppm. Degraded imports stay books, never dead.
    nonisolated static func buildPdfManifest(id: UUID, originalFileName: String, directory: URL) -> Book {
        let fallbackTitle = (originalFileName as NSString)
            .deletingPathExtension
            .replacingOccurrences(of: "_", with: " ")
        var book = Book(id: id, title: fallbackTitle, format: .pdf, originalFileName: originalFileName)

        guard PdfTextLinux.isAvailable else {
            book.importError = "poppler-utils (pdftotext/pdfinfo) not installed — PDF text unavailable."
            return book
        }
        let pdfPath = directory.appendingPathComponent("original.pdf").path
        let info = PdfTextLinux.documentInfo(pdfPath: pdfPath)
        guard info.pageCount > 0 else {
            book.importError = "This PDF could not be read (encrypted or malformed)."
            return book
        }
        if let title = info.title { book.title = title }
        if let author = info.author { book.author = author }
        book.pageCount = info.pageCount
        let chapters = PdfTextLinux.fallbackChapters(pageCount: info.pageCount)
        book.pdfChapters = chapters
        book.pdfChapterSource = "pages"

        let coverPrefix = directory.appendingPathComponent("cover")
        _ = PdfTextLinux.run(PdfTextLinux.pdftoppmPath, [
            "-f", "1", "-l", "1", "-singlefile",
            "-jpeg", "-jpegopt", "quality=85",
            pdfPath, coverPrefix.path,
        ])
        if let attrs = try? FileManager.default.attributesOfItem(atPath: directory.appendingPathComponent("cover.jpg").path),
           (attrs[.size] as? Int64 ?? 0) > 1_000 {
            book.hasCover = true
        }
        return book
    }

    /// Audiobook manifest: duration + tags via ffprobe, chapters from the
    /// file's own metadata (chpl atom for the MP4 family, ID3 CHAP frames
    /// for MP3), cover art via ffmpeg. All CLI-driven — the box's ffmpeg is
    /// the decoder of record. Chapters missing → one implicit chapter so
    /// the player always has navigable units (source "single").
    nonisolated static func buildAudioManifest(id: UUID, originalFileName: String, directory: URL) -> Book {
        let fallbackTitle = (originalFileName as NSString)
            .deletingPathExtension
            .replacingOccurrences(of: "_", with: " ")
        var book = Book(id: id, title: fallbackTitle, format: .audio, originalFileName: originalFileName)

        // The file kept its true extension at copy time.
        let fileURL: URL = {
            for ext in ["m4b", "m4a", "mp3", "mp4"] {
                let candidate = directory.appendingPathComponent("original.\(ext)")
                if FileManager.default.fileExists(atPath: candidate.path) { return candidate }
            }
            return directory.appendingPathComponent("original.audio")
        }()
        guard FileManager.default.fileExists(atPath: fileURL.path) else {
            book.importError = "The audio file could not be found."
            return book
        }

        // Duration + tags: one ffprobe call, flat key=value output.
        let probe = Self.runTool(
            BookAudioPlayerBridge.ffprobePath,
            ["-v", "error", "-show_entries", "format=duration",
             "-show_entries", "format_tags=title:format_tags=artist",
             "-of", "default=noprint_wrappers=1", fileURL.path]
        )
        var duration: Double?
        var title: String?
        var artist: String?
        for line in probe.split(separator: "\n") {
            let parts = line.split(separator: "=", maxSplits: 1)
            guard parts.count == 2 else { continue }
            // ffprobe prints tags as "TAG:title=…" — strip the prefix.
            let key = parts[0].hasPrefix("TAG:") ? String(parts[0].dropFirst(4)) : String(parts[0])
            switch key {
            case "duration": duration = Double(parts[1])
            case "title": title = String(parts[1])
            case "artist": artist = String(parts[1])
            default: break
            }
        }
        if let title, !title.isEmpty { book.title = title }
        if let artist, !artist.isEmpty { book.author = artist }
        book.audioDuration = duration

        // Chapters: ffprobe reads the container's own chapter atoms (chpl
        // for the MP4 family, ID3 CHAP for MP3) — the same decoder of record
        // the player uses, so what ffprobe sees, the reader gets. The
        // manual byte-level parsers remain the fallback.
        var chapters = Self.chaptersFromFFprobe(fileURL.path)
        if chapters.isEmpty {
            let ext = fileURL.pathExtension.lowercased()
            if ext == "mp3" {
                let data = Self.headSlice(fileURL, bytes: 8 * 1024 * 1024) ?? Data()
                chapters = AudiobookChapters.chaptersFromID3(data)
            } else {
                let head = Self.headSlice(fileURL, bytes: 8 * 1024 * 1024) ?? Data()
                let tail = Self.tailSlice(fileURL, bytes: 8 * 1024 * 1024) ?? Data()
                // Parse the windows SEPARATELY — in a head+tail
                // concatenation, a truncated mdat box makes the box walk
                // skip past the tail's moov (the missing-TOC bug).
                chapters = AudiobookChapters.chaptersFromMP4(head, totalSeconds: duration ?? 0)
                if chapters.isEmpty {
                    chapters = AudiobookChapters.chaptersFromMP4(tail, totalSeconds: duration ?? 0)
                }
            }
        }
        if chapters.isEmpty {
            let total = duration ?? 0
            book.audioChapters = [AudioChapter(
                title: fallbackTitle,
                startSeconds: 0,
                endSeconds: total
            )]
            book.audioChapterSource = "single"
        } else {
            book.audioChapters = chapters
            book.audioChapterSource = "chpl"
        }

        // Cover: first embedded video frame → cover.jpg.
        let coverURL = directory.appendingPathComponent("cover.jpg")
        Self.runTool(
            BookAudioPlayerBridge.ffmpegPath,
            ["-v", "error", "-y", "-i", fileURL.path, "-map", "0:v:0",
             "-frames:v", "1", "-q:v", "2", coverURL.path]
        )
        if let attrs = try? FileManager.default.attributesOfItem(atPath: coverURL.path),
           (attrs[.size] as? Int64 ?? 0) > 1_000 {
            book.hasCover = true
        }
        return book
    }

    // MARK: - Audio tooling (shared with the player via the bridge names)

    /// Chapter list straight from the container, via ffprobe's JSON output.
    /// Titles come from the tags; entries without a usable time range are
    /// dropped. Empty when the file has no chapters (or ffprobe is absent).
    nonisolated static func chaptersFromFFprobe(_ path: String) -> [AudioChapter] {
        guard FileManager.default.isExecutableFile(atPath: BookAudioPlayerBridge.ffprobePath) else {
            return []
        }
        let json = runTool(
            BookAudioPlayerBridge.ffprobePath,
            ["-v", "error", "-show_chapters", "-of", "json", path]
        )
        guard !json.isEmpty,
              let data = json.data(using: .utf8),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let entries = root["chapters"] as? [[String: Any]] else {
            return []
        }
        var chapters: [AudioChapter] = []
        for entry in entries {
            guard let start = Self.probeSeconds(entry["start_time"]),
                  let end = Self.probeSeconds(entry["end_time"]),
                  end > start else { continue }
            let tags = entry["tags"] as? [String: Any]
            chapters.append(AudioChapter(
                title: (tags?["title"] as? String) ?? "",
                startSeconds: start,
                endSeconds: end
            ))
        }
        return chapters
    }

    /// ffprobe prints times as strings ("63.531000"); JSON may also give
    /// numbers. Accepts both.
    nonisolated static func probeSeconds(_ value: Any?) -> Double? {
        if let d = value as? Double { return d }
        if let i = value as? Int { return Double(i) }
        if let s = value as? String { return Double(s) }
        return nil
    }

    enum BookAudioPlayerBridge {
        static let ffmpegPath: String = {
            for candidate in ["/usr/bin/ffmpeg", "/usr/local/bin/ffmpeg"] {
                if FileManager.default.isExecutableFile(atPath: candidate) { return candidate }
            }
            return "/usr/bin/ffmpeg"
        }()
        static let ffprobePath: String = {
            for candidate in ["/usr/bin/ffprobe", "/usr/local/bin/ffprobe"] {
                if FileManager.default.isExecutableFile(atPath: candidate) { return candidate }
            }
            return "/usr/bin/ffprobe"
        }()
    }

    /// Runs a CLI tool, returns stdout (empty on any failure — books import
    /// degraded, never dead).
    nonisolated static func runTool(_ path: String, _ args: [String]) -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: path)
        process.arguments = args
        let stdout = Pipe()
        process.standardOutput = stdout
        process.standardError = Pipe()
        do {
            try process.run()
        } catch {
            return ""
        }
        let data = stdout.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else { return "" }
        return String(decoding: data, as: UTF8.self)
    }

    /// `bytes` from the head of the file (clamped).
    nonisolated static func headSlice(_ url: URL, bytes: Int) -> Data? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        return handle.readData(ofLength: bytes)
    }

    /// `bytes` from the tail of the file (clamped).
    nonisolated static func tailSlice(_ url: URL, bytes: Int) -> Data? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        guard let size = try? handle.seekToEnd() else { return nil }
        let start = UInt64(max(0, Int64(size) - Int64(bytes)))
        try? handle.seek(toOffset: start)
        return handle.readData(ofLength: bytes)
    }

    // MARK: - Chapter text (epub spine → speakable text)

    /// Extracts one spine chapter's plain text straight from the archive.
    /// Cached to text/NNNN.txt by the TTS pipeline once spoken; the reader
    /// path extracts on demand (one zip entry — fast).
    public static func chapterText(book: Book, chapterIndex: Int, archiveData: Data) -> String? {
        guard let spine = book.spine, chapterIndex >= 0, chapterIndex < spine.count else {
            return nil
        }
        guard let entry = try? ZipReader.readEntry(spine[chapterIndex], in: archiveData) else {
            return nil
        }
        return XhtmlText.extract(from: entry).text
    }

    public static func epubArchiveURL(_ book: Book) -> URL {
        bookDirectory(book.id).appendingPathComponent("original.epub")
    }

    // MARK: - Mutations

    /// Saves the reader position + last-opened stamp (the shelf sorts by it).
    public func updatePosition(_ book: Book, position: BookPosition) {
        guard var updated = allBooks.first(where: { $0.id == book.id }) else { return }
        updated.position = position
        updated.lastOpenedAt = Date()
        persist(updated)
    }

    public func markOpened(_ book: Book) {
        guard var updated = allBooks.first(where: { $0.id == book.id }) else { return }
        updated.lastOpenedAt = Date()
        persist(updated)
    }

    /// Recycle bin.
    public func moveToBin(_ book: Book) {
        guard var updated = allBooks.first(where: { $0.id == book.id }) else { return }
        updated.deletedAt = Date()
        persist(updated)
    }

    public func restore(_ book: Book) {
        guard var updated = allBooks.first(where: { $0.id == book.id }) else { return }
        updated.deletedAt = nil
        persist(updated)
    }

    public func purge(_ book: Book) {
        try? FileManager.default.removeItem(at: Self.bookDirectory(book.id))
        allBooks.removeAll { $0.id == book.id }
        shelfVersion += 1
    }

    public func emptyRecycleBin() {
        for book in deletedBooks {
            try? FileManager.default.removeItem(at: Self.bookDirectory(book.id))
        }
        allBooks.removeAll { $0.isDeleted }
        shelfVersion += 1
    }

    /// The one write path: patch the in-memory shelf, rewrite that book's
    /// manifest atomically. Never touches other books' files.
    private func persist(_ updated: Book) {
        guard let index = allBooks.firstIndex(where: { $0.id == updated.id }) else { return }
        allBooks[index] = updated
        do {
            let data = try JSONEncoder().encode(updated)
            try data.write(to: Self.manifestURL(updated.id), options: .atomic)
        } catch {
            Log.error("BooksStore: manifest write failed: \(error)")
        }
        shelfVersion += 1
    }
}
