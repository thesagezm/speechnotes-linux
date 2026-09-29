import Foundation
import SwiftCrossUI
import Data
import TTSEngine

/// The book reader: chapter text (epub spine extracted straight from the
/// archive), TOC navigation, and the speak bar driving the TTS tier per
/// chapter. The reader owns the BookPosition lifecycle — chapter changes
/// and stops persist it into the manifest, Resume picks it back up.
struct BookReaderPane: View {
    let book: Book
    let books: BooksStore

    @State private var tts = TTSController.shared
    @State private var prefs = Prefs.shared
    @State private var chapterText: String?
    @State private var chapterIndex: Int = 0
    @State private var showToc = false
    @State private var loadFailed = false

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                Button(book.position == nil ? "" : "↩ Resume") {
                    if let position = book.position {
                        open(chapter: position.chapterIndex, resume: true)
                    }
                }
                .disabled(book.position == nil)
                Button("◂ Chapter") { open(chapter: max(0, chapterIndex - 1)) }
                Text(chapterLabel)
                Button("Chapter ▸") { open(chapter: min(chapterCount - 1, chapterIndex + 1)) }
                Spacer()
                Button(showToc ? "Hide TOC" : "TOC") { showToc.toggle() }
            }
            if showToc {
                ScrollView {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(Array((book.toc ?? []).enumerated()), id: \.offset) { _, entry in
                            Button(entry.label) {
                                if let spineIndex = entry.spineIndex {
                                    open(chapter: spineIndex)
                                }
                            }
                        }
                    }
                }
                .frame(maxHeight: 160)
            }
            HStack(spacing: 8) {
                if tts.isPlaying(noteId: book.id) {
                    if tts.state == .paused {
                        Button("▶ Resume") { tts.resume() }
                    } else {
                        Button("❙❙ Pause") { tts.pause() }
                    }
                    Button("■ Stop") {
                        tts.stop()
                        savePosition(fraction: lastFraction)
                    }
                } else {
                    Button("▶ Speak chapter") { speakCurrent() }
                }
                if let error = tts.lastError {
                    Text(error).foregroundColor(.orange)
                }
                Spacer()
                if let pos = tts.position, pos.noteId == book.id {
                    Text("\(Int((pos.fraction * 100).rounded()))%")
                }
                Button("🗑") {
                    tts.stop()
                    books.moveToBin(book)
                }
            }
            if tts.isPlaying(noteId: book.id), let sentence = tts.currentSentence {
                Text("▸ \(sentence)").foregroundColor(.gray)
            }
            if loadFailed {
                Text("This chapter could not be extracted from the archive.")
                    .foregroundColor(.orange)
            }
            if book.format == .audio {
                Text("Audiobook playback arrives with the audio phase — the book is shelved and its chapters are listed.")
                    .foregroundColor(.gray)
                audioChapterList
            } else {
                ScrollView {
                    Text(chapterText ?? "")
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .padding(8)
        .task { initialOpen() }
    }

    private var audioChapterList: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array((book.audioChapters ?? []).enumerated()), id: \.offset) { index, chapter in
                    Text("\(index + 1). \(chapter.title.isEmpty ? "Chapter \(index + 1)" : chapter.title)")
                }
            }
        }
    }

    private var chapterCount: Int {
        max(1, book.spineCount ?? book.spine?.count ?? 1)
    }

    private var chapterLabel: String {
        "Chapter \(chapterIndex + 1) / \(chapterCount)"
    }

    private var lastFraction: Double {
        tts.position?.fraction ?? book.position?.chapterFraction ?? 0
    }

    private func initialOpen() {
        if let position = book.position, position.chapterIndex < chapterCount {
            open(chapter: position.chapterIndex)
        } else {
            open(chapter: 0)
        }
    }

    private func open(chapter: Int, resume: Bool = false) {
        let clamped = min(max(chapter, 0), chapterCount - 1)
        chapterIndex = clamped
        loadFailed = false
        guard book.format == .epub, let spine = book.spine, clamped < spine.count else {
            chapterText = nil
            return
        }
        do {
            let data = try Data(contentsOf: BooksStore.epubArchiveURL(book), options: .mappedIfSafe)
            chapterText = BooksStore.chapterText(book: book, chapterIndex: clamped, archiveData: data)
            if chapterText == nil { loadFailed = true }
        } catch {
            chapterText = nil
            loadFailed = true
        }
        books.markOpened(book)
        if !resume {
            savePosition(fraction: 0)
        }
    }

    private func speakCurrent() {
        guard let text = chapterText, !text.isEmpty else { return }
        tts.playText(
            id: book.id,
            text: text,
            engineKind: EngineKind(rawValue: prefs.engineKind) ?? .espeak,
            speed: Float(prefs.rateMultiplier),
            voice: prefs.voice,
            bookmarkKey: BookmarkStore.bookKey(book.id.uuidString, chapter: chapterIndex)
        )
    }

    private func savePosition(fraction: Double) {
        books.updatePosition(book, position: BookPosition(chapterIndex: chapterIndex, chapterFraction: fraction))
    }
}
