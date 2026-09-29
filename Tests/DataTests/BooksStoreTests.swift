import XCTest
@testable import Data
@testable import AppPaths
import SpeechLogic

/// Books data layer: iOS-byte-compatible manifests, the per-book directory
/// layout, EPUB import (fixture archive), position persistence, and the
/// books recycle bin lifecycle.
///
/// Linux XCTest has no isolated setUp overrides, so each test claims its own
/// throwaway XDG home (AppPaths re-reads XDG_*_HOME on every access) and
/// cleans up with `defer`.
final class BooksStoreTests: XCTestCase {

    private func freshHome() throws -> URL {
        let home = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("speechnotes-books-tests-\(UUID().uuidString)", isDirectory: true)
        setenv("XDG_DATA_HOME", home.path, 1)
        setenv("XDG_CONFIG_HOME", home.path, 1)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        return home
    }

    /// The manifest struct must decode a manifest written by the iOS app —
    /// field names, optional absences and all.
    func testManifestDecodesiOSManifest() throws {
        // Shape mirrors speechnotes-ios Book: camelCase fields, absent
        // optionals, format raw values, ISO-independent Date coding
        // (timeIntervalSinceReferenceDate doubles).
        let json = """
        {
          "id": "2F1C4E10-1234-4ABC-9DEF-001122334455",
          "title": "A Scanner Darkly",
          "author": "Philip K. Dick",
          "format": "epub",
          "originalFileName": "scanner.epub",
          "addedAt": 780000000.0,
          "lastOpenedAt": 781000000.0,
          "spineCount": 2,
          "spine": ["OEBPS/ch1.xhtml", "OEBPS/ch2.xhtml"],
          "hasCover": true,
          "toc": [
            {"label": "One", "href": "OEBPS/ch1.xhtml", "spineIndex": 0, "depth": 0}
          ],
          "position": {"chapterIndex": 1, "chapterFraction": 0.5},
          "deletedAt": null
        }
        """.data(using: .utf8)!
        let book = try JSONDecoder().decode(Book.self, from: json)
        XCTAssertEqual(book.title, "A Scanner Darkly")
        XCTAssertEqual(book.format, .epub)
        XCTAssertEqual(book.spine?.count, 2)
        XCTAssertEqual(book.toc?.first?.depth, 0)
        XCTAssertEqual(book.position?.chapterFraction ?? 0, 0.5, accuracy: 0.0001)
        XCTAssertFalse(book.isDeleted)
    }

    /// Audio manifests carry chapter + duration metadata.
    func testManifestDecodesAudioBookFields() throws {
        let json = """
        {
          "id": "2F1C4E10-1234-4ABC-9DEF-AABBCCDDEEFF",
          "title": "Dune",
          "format": "audio",
          "originalFileName": "dune.m4b",
          "addedAt": 780000000.0,
          "hasCover": false,
          "audioChapters": [
            {"title": "Book One", "startSeconds": 0, "endSeconds": 120.5},
            {"title": "Book Two", "startSeconds": 120.5, "endSeconds": 300}
          ],
          "audioChapterSource": "chpl",
          "audioDuration": 300,
          "deletedAt": 782000000.0
        }
        """.data(using: .utf8)!
        let book = try JSONDecoder().decode(Book.self, from: json)
        XCTAssertTrue(book.isDeleted)
        XCTAssertEqual(book.audioChapters?.count, 2)
        XCTAssertEqual(book.audioChapterSource, "chpl")
        XCTAssertEqual(book.audioChapters?[1].endSeconds ?? 0, 300, accuracy: 0.001)
    }

    /// Round-trip: encode → decode → equal.
    func testManifestRoundTrip() throws {
        let book = Book(
            id: UUID(),
            title: "Round Trip",
            author: "Author",
            format: .epub,
            originalFileName: "rt.epub",
            spineCount: 1,
            spine: ["A/B.xhtml"],
            toc: [BookTocEntry(label: "A", href: "A/B.xhtml", spineIndex: 0)],
            position: BookPosition(chapterIndex: 0, chapterFraction: 0.25)
        )
        let data = try JSONEncoder().encode(book)
        let back = try JSONDecoder().decode(Book.self, from: data)
        XCTAssertEqual(back, book)
    }

    /// EPUB import end-to-end against the SpeechLogic sample archive: the
    /// directory layout (original.epub, manifest.json, cover), the parsed
    /// spine/TOC, and shelf refresh.
    @MainActor
    func testImportEpub() async throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()

        let fixtureURL = Bundle.module
            .url(forResource: "sample", withExtension: "epub", subdirectory: "Fixtures")
        guard let fixtureURL else {
            throw XCTSkip("sample.epub fixture not available to the Data test bundle")
        }

        let store = BooksStore()
        XCTAssertTrue(store.books.isEmpty)
        let bookOpt = await store.importBook(from: fixtureURL)
        let imported = try XCTUnwrap(bookOpt)
        XCTAssertNil(imported.importError)
        XCTAssertEqual(imported.format, .epub)
        XCTAssertEqual(imported.originalFileName, "sample.epub")
        XCTAssertNotNil(imported.spine, "import snapshots the spine")
        XCTAssertEqual(imported.spineCount, imported.spine?.count)

        let dir = BooksStore.bookDirectory(imported.id)
        XCTAssertTrue(FileManager.default.fileExists(atPath: dir.appendingPathComponent("original.epub").path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: BooksStore.manifestURL(imported.id).path))
        XCTAssertTrue(store.books.contains { $0.id == imported.id })

        // Chapter text extraction runs straight off the stored archive.
        let archive = try Data(contentsOf: BooksStore.epubArchiveURL(imported))
        let text = BooksStore.chapterText(book: imported, chapterIndex: 0, archiveData: archive)
        XCTAssertNotEqual(text?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, true)
    }

    /// Position persistence lands in the manifest and survives a reload.
    @MainActor
    func testPositionPersistsAcrossReload() async throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()

        let fixtureURL = Bundle.module
            .url(forResource: "sample", withExtension: "epub", subdirectory: "Fixtures")
        guard let fixtureURL else {
            throw XCTSkip("sample.epub fixture not available to the Data test bundle")
        }

        let store = BooksStore()
        let bookOpt = await store.importBook(from: fixtureURL)
        let book = try XCTUnwrap(bookOpt)
        store.updatePosition(book, position: BookPosition(chapterIndex: 2, chapterFraction: 0.75))

        let manifestPath = BooksStore.manifestURL(book.id).path
        print("DEBUG manifest exists:", FileManager.default.fileExists(atPath: manifestPath))
        if let raw = try? Data(contentsOf: URL(fileURLWithPath: manifestPath)) {
            print("DEBUG manifest:", String(decoding: raw, as: UTF8.self))
        }
        print("DEBUG inMemory position:", store.allBooks.first(where: { $0.id == book.id })?.position as Any)
        let reloaded = BooksStore()
        XCTAssertEqual(reloaded.books.first(where: { $0.id == book.id })?.position?.chapterIndex, 2)
    }

    /// Bin → restore → purge lifecycle, and expired bins prune.
    @MainActor
    func testRecycleBinLifecycle() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()

        let store = BooksStore()
        let book = Book(id: UUID(), title: "Binned", format: .epub, originalFileName: "b.epub")
        let dir = BooksStore.bookDirectory(book.id)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let manifest = try JSONEncoder().encode(book)
        try manifest.write(to: BooksStore.manifestURL(book.id))
        try "x".write(to: dir.appendingPathComponent("original.epub"), atomically: true, encoding: .utf8)
        store.refresh()
        XCTAssertEqual(store.books.count, 1)

        store.moveToBin(book)
        XCTAssertTrue(store.books.isEmpty)
        XCTAssertEqual(store.deletedBooks.count, 1)

        store.restore(book)
        XCTAssertEqual(store.books.count, 1)
        XCTAssertTrue(store.deletedBooks.isEmpty)

        store.moveToBin(book)
        store.purge(book)
        XCTAssertTrue(store.books.isEmpty && store.deletedBooks.isEmpty)
        XCTAssertFalse(FileManager.default.fileExists(atPath: dir.path))
    }

    /// The retention pruner must only ever eat books whose deletedAt is
    /// actually past the window — an active book (no stamp) survives every
    /// reload. Regression: the nil-coalesced filter once treated active
    /// books as binned at .distantPast and deleted their directories at
    /// store init.
    @MainActor
    func testPruneEatsOnlyExpiredBinnedBooks() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()

        let active = Book(id: UUID(), title: "Active", format: .epub, originalFileName: "a.epub")
        let freshBinned = Book(
            id: UUID(), title: "Fresh Binned", format: .epub, originalFileName: "f.epub",
            deletedAt: Date()
        )
        let expiredBinned = Book(
            id: UUID(), title: "Expired Binned", format: .epub, originalFileName: "e.epub",
            deletedAt: Date().addingTimeInterval(-Double(Book.recycleRetentionDays + 1) * 86_400)
        )
        for candidate in [active, freshBinned, expiredBinned] {
            let dir = BooksStore.bookDirectory(candidate.id)
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try JSONEncoder().encode(candidate).write(to: BooksStore.manifestURL(candidate.id))
            try "x".write(to: dir.appendingPathComponent("original.epub"), atomically: true, encoding: .utf8)
        }

        let store = BooksStore()
        XCTAssertTrue(store.books.contains { $0.id == active.id }, "active book survives init")
        XCTAssertTrue(store.deletedBooks.contains { $0.id == freshBinned.id }, "fresh bin survives")
        XCTAssertFalse(store.deletedBooks.contains { $0.id == expiredBinned.id }, "expired bin purged")
        XCTAssertTrue(
            FileManager.default.fileExists(
                atPath: BooksStore.bookDirectory(active.id).appendingPathComponent("original.epub").path
            ),
            "active book's files survive"
        )
    }
}
