import Foundation
import SwiftCrossUI
import Data
import TTSEngine
import Appearance

/// The Books shelf: the imported books, the import buttons (GTK file
/// picker via SwiftCrossUI's chooseFile action), and bin/delete controls.
/// Rows show title, author/format, and progress — the iOS BookRowView's
/// information, desktop-arranged.
struct BooksPane: View {
    let books: BooksStore
    let onSelect: (Book) -> Void

    @Environment(\.chooseFile) private var chooseFile
    @State private var tts = TTSController.shared

    init(books: BooksStore, onSelect: @escaping (Book) -> Void) {
        self.books = books
        self.onSelect = onSelect
    }

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                Text("Books")
                Spacer()
                if books.isImporting {
                    Text("Importing…")
                }
                Button("Import Book…") {
                    Task { await importBook() }
                }
            }
            if let error = books.importError {
                Text("Import failed: \(error)")
                    .foregroundColor(.orange)
            }
            if books.books.isEmpty {
                Spacer()
                Text("No books yet — import an EPUB, PDF or audiobook file.")
                    .foregroundColor(.gray)
                Spacer()
            } else {
                ScrollView {
                    VStack(spacing: 6) {
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
            }
        }
        .padding(8)
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
                Text(book.title)
                    .font(.system(size: 14, weight: isPlaying ? .medium : .regular))
                HStack(spacing: 8) {
                    Text(book.authorOrFormat)
                        .font(.footnote)
                        .foregroundColor(.gray)
                    if let error = book.importError, !error.isEmpty {
                        Text("⚠︎ \(error)")
                            .font(.footnote)
                            .foregroundColor(.orange)
                    } else if let position = book.position {
                        Text("at chapter \(position.chapterIndex + 1)")
                            .font(.footnote)
                            .foregroundColor(.gray)
                    } else if isPlaying {
                        Text("playing")
                            .font(.footnote)
                            .foregroundColor(theme.accent)
                    }
                }
            }
            Spacer()
            Button("🗑") { onBin() }
                .buttonStyle(.borderless)
        }
        .padding(6)
    }
}
