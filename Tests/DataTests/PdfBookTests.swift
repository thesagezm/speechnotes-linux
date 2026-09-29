import XCTest
@testable import Data
@testable import AppPaths
import SpeechLogic

/// PDF books through the poppler CLI path: ps2pdf generates a real 3-page
/// PDF, then the manifest gets page count + page-range chapters + a
/// rendered cover, and pdftotext extracts chapter text. Skips when the box
/// lacks the poppler/ghostscript tools (CI installs none of them).
final class PdfBookTests: XCTestCase {

    private func freshHome() throws -> URL {
        let home = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("speechnotes-pdf-tests-\(UUID().uuidString)", isDirectory: true)
        setenv("XDG_DATA_HOME", home.path, 1)
        setenv("XDG_CONFIG_HOME", home.path, 1)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        return home
    }

    private var toolsAvailable: Bool {
        PdfTextLinux.isAvailable
            && FileManager.default.isExecutableFile(atPath: "/usr/bin/ps2pdf")
    }

    @MainActor
    func testImportPdfBuildsPageRangeManifest() async throws {
        guard toolsAvailable else { throw XCTSkip("no poppler/ghostscript on this box") }
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()

        // 3-page PostScript → PDF (Ghostscript).
        let ps = AppPaths.dataDir.appendingPathComponent("book.ps")
        try """
        %!PS-Adobe-3.0
        /Helvetica findfont 12 scalefont setfont
        72 700 moveto (The opening page sets the scene.) show
        showpage
        /Helvetica findfont 12 scalefont setfont
        72 700 moveto (Middle page with body text to speak aloud.) show
        showpage
        /Helvetica findfont 12 scalefont setfont
        72 700 moveto (Final page closes the tiny book.) show
        showpage
        """.write(to: ps, atomically: true, encoding: .utf8)
        let pdf = AppPaths.dataDir.appendingPathComponent("book.pdf")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/ps2pdf")
        process.arguments = [ps.path, pdf.path]
        process.standardOutput = Pipe()
        process.standardError = Pipe()
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw XCTSkip("ps2pdf could not generate the test PDF")
        }

        let store = BooksStore()
        let bookOpt = await store.importBook(from: pdf)
        let book = try XCTUnwrap(bookOpt)

        XCTAssertNil(book.importError)
        XCTAssertEqual(book.format, .pdf)
        XCTAssertEqual(book.pageCount, 3)
        XCTAssertEqual(book.pdfChapterSource, "pages")
        XCTAssertEqual(book.pdfChapters?.count, 1, "3 pages group into one 10-page unit")
        XCTAssertEqual(book.pdfChapters?.first?.startPage, 0)
        XCTAssertEqual(book.pdfChapters?.first?.endPage, 2)
        XCTAssertTrue(
            FileManager.default.fileExists(atPath: BooksStore.bookDirectory(book.id).appendingPathComponent("cover.jpg").path),
            "page 1 renders as the shelf cover"
        )

        // Chapter text extracts through pdftotext.
        let text = PdfTextLinux.text(pages: 2, to: 2, pdfPath: BooksStore.bookDirectory(book.id)
            .appendingPathComponent("original.pdf").path)
        XCTAssertTrue(text?.contains("Middle page") == true, "page 2 text extracted")
    }

    /// The fallback grouping rule matches iOS: 10 pages per chapter.
    func testFallbackChaptersGrouping() {
        let chapters = PdfTextLinux.fallbackChapters(pageCount: 25)
        XCTAssertEqual(chapters.count, 3)
        XCTAssertEqual(chapters[0].label, "Pages 1–10")
        XCTAssertEqual(chapters[1].startPage, 10)
        XCTAssertEqual(chapters[2].endPage, 24)
        XCTAssertTrue(PdfTextLinux.fallbackChapters(pageCount: 0).isEmpty)
    }
}
