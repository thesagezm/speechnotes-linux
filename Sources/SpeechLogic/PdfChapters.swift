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
