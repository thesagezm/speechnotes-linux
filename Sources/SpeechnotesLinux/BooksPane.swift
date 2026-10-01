import Foundation
import SwiftCrossUI
import Data
import TTSEngine
import BookDrop
import SpeechLogic
import Appearance

/// The Books shelf: the imported books, the import buttons (GTK file
/// picker via SwiftCrossUI's chooseFile action), and bin/delete controls.
/// Rows show title, author/format, and progress — the iOS BookRowView's
/// information, desktop-arranged.
///
/// Row layout note: every row's two-line caption is a fixed-height block
/// (see ``BookRowView``). SwiftCrossUI re-measures every Text with Pango on
/// every layout pass — 929 measurements for a single pane switch, measured
/// — and a row whose height depends on its text is a row whose height is
/// re-derived constantly. Pinning it lets the layout cache do its job and
/// stops long titles from making the shelf jump.
struct BooksPane: View {
    let books: BooksStore
    let onSelect: (Book) -> Void

    @Environment(\.chooseFile) private var chooseFile
    @State private var tts = TTSController.shared
    @State private var theme = ThemeController.shared
    @State private var bookDrop = LocalSendReceiver.shared

    init(books: BooksStore, onSelect: @escaping (Book) -> Void) {
        self.books = books
        self.onSelect = onSelect
    }

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                Text("Books")
                    .font(.title3.weight(.semibold))
                Spacer()
                if books.isImporting {
                    HStack(spacing: 6) {
                        ProgressView()
                        Text("Importing…")
                            .font(.footnote)
                            .foregroundColor(theme.text)
                    }
                }
                Button("Import Book…") {
                    Task { await importBook() }
                }
                .buttonStyle(.bordered)
            }
            HStack(spacing: 10) {
                Toggle("BookDrop (receive books from the local network)", isOn: bookDropBinding)
                    .toggleStyle(.switch)
                if bookDrop.isRunning {
                    Text("listening on port \(bookDrop.port)")
                        .font(.footnote)
                        .foregroundColor(theme.text)
                }
                if let error = bookDrop.lastError {
                    Text(error)
                        .font(.footnote)
                        .foregroundColor(.orange)
                }
                if let last = bookDrop.history.first {
                    Text(last.name + ": " + BookDropService.outcomeLabel(last.outcome))
                        .font(.footnote)
                        .foregroundColor(theme.text)
                }
            }
            if let error = books.importError {
                Text("Import failed: \(error)")
                    .foregroundColor(.orange)
            }
            if books.books.isEmpty {
                Spacer()
                Text("No books yet — import an EPUB, PDF or audiobook file.")
                    .foregroundColor(theme.text)
                Spacer()
            } else {
                ScrollView {
                    VStack(spacing: 4) {
                        ForEach(books.books) { book in
                            BookRowView(
                                book: book,
                                isPlaying: tts.isPlaying(noteId: book.id),
                                onOpen: { onSelect(book) },
                                onBin: { books.moveToBin(book) }
                            )
                        }
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            binnedSection
        }
        .padding(8)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
    }

    /// The books recycle bin: same 30-day retention as notes, with
    /// recover/purge/empty — the store side has existed since Phase 8, this
    /// is the UI. Kept as a fixed bottom block (small counts); the shelf
    /// scroll above takes the remaining space.
    @ViewBuilder
    private var binnedSection: some View {
        let binned = books.deletedBooks
        if !binned.isEmpty {
            Divider()
            HStack(spacing: 8) {
                Text("Recycle bin (\(binned.count))")
                    .font(.callout.weight(.medium))
                Spacer()
                Button("Empty bin") { books.emptyRecycleBin() }
                    .buttonStyle(.borderless)
            }
            VStack(spacing: 4) {
                ForEach(binned) { book in
                    BinnedBookRow(
                        book: book,
                        onRecover: { books.restore(book) },
                        onPurge: { books.purge(book) }
                    )
                }
            }
            .padding(.bottom, 4)
        }
    }

    private var bookDropBinding: Binding<Bool> {
        Binding(
            get: { BookDropService.isEnabled },
            set: { BookDropService.setEnabled($0) }
        )
    }

    private func importBook() async {
        guard let url = await chooseFile(
            title: "Import Book",
            message: "EPUB, PDF, M4B/M4A/MP4/MP3",
            defaultButtonLabel: "Import"
        ) else { return }
        _ = await books.importBook(from: url)
    }
}

/// One binned-book row: title, purge countdown, recover / delete forever.
struct BinnedBookRow: View {
    let book: Book
    let onRecover: () -> Void
    let onPurge: () -> Void

    @State private var theme = ThemeController.shared

    var body: some View {
        HStack(spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(book.title)
                    .font(.system(size: 13))
                    .lineLimit(1)
                Text(caption)
                    .font(.footnote)
                    .foregroundColor(theme.text)
                    .lineLimit(1)
            }
            Spacer()
            Button("Recover") { onRecover() }
                .buttonStyle(.bordered)
            Button("Delete forever") { onPurge() }
                .buttonStyle(.borderless)
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
    }

    private var caption: String {
        guard let deletedAt = book.deletedAt else { return "" }
        let days = Int(Date().timeIntervalSince(deletedAt) / 86_400)
        let remaining = max(0, Book.recycleRetentionDays - days)
        return "purges in \(remaining) day\(remaining == 1 ? "" : "s")"
    }
}

/// One shelf row: title, author/format, position marker, open + bin.
struct BookRowView: View {
    let book: Book
    let isPlaying: Bool
    let onOpen: () -> Void
    let onBin: () -> Void

    @State private var theme = ThemeController.shared

    var body: some View {
        HStack(spacing: 10) {
            Button {
                onOpen()
            } label: {
                Text(isPlaying ? "❙❙" : "▸")
                    .font(.system(size: 15, weight: .medium))
                    .foregroundColor(isPlaying ? theme.accent : Color(white: 0.5))
                    .frame(width: 26, height: 26)
                    .background(theme.accent.opacity(isPlaying ? 0.14 : 0))
                    .cornerRadius(13)
            }
            .buttonStyle(.borderless)
            VStack(alignment: .leading, spacing: 2) {
                // Single-line title: the full title is still reachable by
                // opening the book (the reader header shows it), and a
                // wrapping row is a row whose height changes with content.
                Text(book.title)
                    .font(.system(size: 14, weight: isPlaying ? .medium : .regular))
                    .lineLimit(1)
                HStack(spacing: 8) {
                    Text(book.authorOrFormat)
                        .font(.footnote)
                        .foregroundColor(theme.text)
                        .lineLimit(1)
                    if let error = book.importError, !error.isEmpty {
                        Text("⚠︎ \(error)")
                            .font(.footnote)
                            .foregroundColor(.orange)
                            .lineLimit(1)
                    } else if let position = book.position {
                        Text("at chapter \(position.chapterIndex + 1)")
                            .font(.footnote)
                            .foregroundColor(theme.text)
                            .lineLimit(1)
                    } else if isPlaying {
                        Text("playing")
                            .font(.footnote)
                            .foregroundColor(theme.accent)
                            .lineLimit(1)
                    }
                }
            }
            Spacer()
            Button("🗑") { onBin() }
                .buttonStyle(.borderless)
        }
        .padding(.vertical, 2)
    }
}
