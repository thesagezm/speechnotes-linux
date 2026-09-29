import Foundation

/// One speech unit of a PDF: `startPage...endPage` (inclusive). Persisted in
/// the book manifest (`Book.pdfChapters`) so library, reader and TTS agree
/// without re-parsing the document.
///
/// Linux port note: iOS resolves these from PDFKit's outline tree; the Linux
/// port resolves them from poppler's text layout (pdftotext) — same manifest
/// shape, same "outline → headings → page ranges" fallback ladder.
public struct PdfChapter: Codable, Equatable, Hashable, Sendable {
    public var label: String
    public var startPage: Int
    public var endPage: Int

    public init(label: String, startPage: Int, endPage: Int) {
        self.label = label
        self.startPage = startPage
        self.endPage = endPage
    }
}

/// Where a page's text starts inside its chapter's speech text — the
/// read-along page-sync sidecar (`text/NNNN.pages.json`).
public struct PdfPageOffset: Codable, Equatable, Hashable, Sendable {
    public var page: Int
    public var utf16Offset: Int

    public init(page: Int, utf16Offset: Int) {
        self.page = page
        self.utf16Offset = utf16Offset
    }
}

/// PDF → speech-text via poppler's CLI (pdftotext/pdfinfo/pdftoppm) — the
/// Linux stand-in for iOS's PDFKit path. Same contract: the page-range
/// fallback is the floor, so playback always has navigable units.
public enum PdfTextLinux {

    public static let pdfinfoPath = "/usr/bin/pdfinfo"
    public static let pdftotextPath = "/usr/bin/pdftotext"
    public static let pdftoppmPath = "/usr/bin/pdftoppm"

    public static var isAvailable: Bool {
        [pdfinfoPath, pdftotextPath].allSatisfy { FileManager.default.isExecutableFile(atPath: $0) }
    }

    /// Runs a tool, returns stdout (empty on any failure).
    public static func run(_ path: String, _ args: [String]) -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: path)
        process.arguments = args
        let stdout = Pipe()
        process.standardOutput = stdout
        process.standardError = Pipe()
        guard (try? process.run()) != nil else { return "" }
        let data = stdout.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else { return "" }
        return String(decoding: data, as: UTF8.self)
    }

    /// Page count + document metadata from pdfinfo's key/value output.
    public static func documentInfo(pdfPath: String) -> (pageCount: Int, title: String?, author: String?) {
        let out = run(pdfinfoPath, [pdfPath])
        var pageCount = 0
        var title: String?
        var author: String?
        for line in out.split(separator: "\n") {
            let parts = line.split(separator: ":", maxSplits: 1)
            guard parts.count == 2 else { continue }
            let key = parts[0].trimmingCharacters(in: .whitespaces)
            let value = parts[1].trimmingCharacters(in: .whitespaces)
            switch key {
            case "Pages": pageCount = Int(value) ?? 0
            case "Title" where !value.isEmpty: title = value
            case "Author" where !value.isEmpty: author = value
            default: break
            }
        }
        return (pageCount, title, author)
    }

    /// Plain text of an inclusive page range (1-based page numbers, as
    /// pdftotext takes them).
    public static func text(pages startPage: Int, to endPage: Int, pdfPath: String) -> String? {
        guard startPage >= 1, endPage >= startPage else { return nil }
        let out = run(pdftotextPath, [
            "-enc", "UTF-8",
            "-f", String(startPage),
            "-l", String(endPage),
            pdfPath, "-",
        ])
        return out.isEmpty ? nil : out
    }

    /// The page-range floor, identical to iOS's rule: ~10 pages per
    /// navigable unit, labeled "Pages X–Y".
    public static func fallbackChapters(pageCount: Int, groupSize: Int = 10) -> [PdfChapter] {
        guard pageCount > 0 else { return [] }
        var chapters: [PdfChapter] = []
        var start = 0
        while start < pageCount {
            let end = min(start + groupSize - 1, pageCount - 1)
            chapters.append(
                PdfChapter(label: "Pages \(start + 1)–\(end + 1)", startPage: start, endPage: end)
            )
            start = end + 1
        }
        return chapters
    }
}
