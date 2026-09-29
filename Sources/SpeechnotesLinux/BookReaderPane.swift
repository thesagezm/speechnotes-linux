import Foundation
import SwiftCrossUI
import Data
import SpeechLogic
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
    @State private var selectedAudioChapter = 0

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
                audioControls
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

    /// Audiobook transport: chapter picker, play/pause/stop, the audible
    /// clock, and resume-at-chapter. Driven by AudioBookController.
    @ViewBuilder
    private var audioControls: some View {
        let audio = AudioBookController.shared
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                if audio.isPlaying(bookId: book.id) {
                    if audio.state == .paused {
                        Button("▶ Resume") { audio.resume() }
                    } else {
                        Button("❙❙ Pause") { audio.pause() }
                    }
                    Button("■ Stop") { audio.stop() }
                } else {
                    Button("▶ Play") {
                        let start = resumeSeconds()
                        audio.play(book: book, chapterIndex: selectedAudioChapter, startSeconds: start)
                    }
                    if resumeSeconds() > 0 {
                        Button("↩ Resume chapter \(selectedAudioChapter + 1)") {
                            audio.play(book: book, chapterIndex: selectedAudioChapter, startSeconds: resumeSeconds())
                        }
                    }
                }
                if let error = audio.lastError {
                    Text(error).foregroundColor(.orange)
                }
                Spacer()
                if let pos = audio.position, pos.bookId == book.id {
                    Text("\(Self.clock(pos.seconds)) / \(Self.clock(chapterDuration))")
                }
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(Array((book.audioChapters ?? []).enumerated()), id: \.offset) { index, chapter in
                        // SwiftCrossUI on Linux has no Color.primary; the
                        // ●/○ marker alone distinguishes the active chapter.
                        Button(index == selectedAudioChapter
                            ? "● \(index + 1). \(chapter.title)"
                            : "○ \(index + 1). \(chapter.title)"
                        ) {
                            selectedAudioChapter = index
                            if audio.isPlaying(bookId: book.id) {
                                audio.play(book: book, chapterIndex: index, startSeconds: chapter.startSeconds)
                            }
                        }
                    }
                }
            }
        }
    }

    private var chapterDuration: Double {
        let chapters = book.audioChapters ?? []
        guard selectedAudioChapter < chapters.count else { return 0 }
        return chapters[selectedAudioChapter].endSeconds - chapters[selectedAudioChapter].startSeconds
    }

    /// The per-chapter second bookmark (stored as ms in textOffset).
    private func resumeSeconds() -> Double {
        let mark = BookmarkStore.shared.get(
            BookmarkStore.bookKey(book.id.uuidString, chapter: selectedAudioChapter)
        )
        return Double(mark?.textOffset ?? 0) / 1000.0
    }

    private static func clock(_ seconds: Double) -> String {
        let s = Int(seconds.rounded())
        return String(format: "%d:%02d", s / 60, s % 60)
    }

    private var chapterCount: Int {
        switch book.format {
        case .epub: return max(1, book.spineCount ?? book.spine?.count ?? 1)
        case .pdf: return max(1, book.pdfChapters?.count ?? 1)
        case .audio: return max(1, book.audioChapters?.count ?? 1)
        }
    }

    private var chapterLabel: String {
        "Chapter \(chapterIndex + 1) / \(chapterCount)"
    }

    private var lastFraction: Double {
        tts.position?.fraction ?? book.position?.chapterFraction ?? 0
    }

    private func initialOpen() {
        if let position = book.position, position.chapterIndex < chapterCount {
            chapterIndex = position.chapterIndex
            selectedAudioChapter = position.chapterIndex
            if book.format == .epub || book.format == .pdf {
                loadChapter(position.chapterIndex)
            }
            books.markOpened(book)
        } else {
            chapterIndex = 0
            selectedAudioChapter = 0
            if book.format == .epub || book.format == .pdf {
                loadChapter(0)
            }
            books.markOpened(book)
        }
    }

    private func open(chapter: Int, resume: Bool = false) {
        let clamped = min(max(chapter, 0), chapterCount - 1)
        chapterIndex = clamped
        selectedAudioChapter = clamped
        loadFailed = false
        guard book.format == .epub || book.format == .pdf else { return }
        loadChapter(clamped)
        books.markOpened(book)
        if !resume {
            savePosition(fraction: 0)
        }
    }

    private func loadChapter(_ index: Int) {
        switch book.format {
        case .epub:
            guard let spine = book.spine, index < spine.count else {
                chapterText = nil
                return
            }
            do {
                let data = try Data(contentsOf: BooksStore.epubArchiveURL(book), options: .mappedIfSafe)
                chapterText = BooksStore.chapterText(book: book, chapterIndex: index, archiveData: data)
                if chapterText == nil { loadFailed = true }
            } catch {
                chapterText = nil
                loadFailed = true
            }
        case .pdf:
            // PDF chapters are page ranges; pdftotext extracts the range.
            guard let chapters = book.pdfChapters, index < chapters.count else {
                chapterText = nil
                return
            }
            let chapter = chapters[index]
            let pdfPath = BooksStore.bookDirectory(book.id).appendingPathComponent("original.pdf").path
            chapterText = PdfTextLinux.text(
                pages: chapter.startPage + 1,
                to: chapter.endPage + 1,
                pdfPath: pdfPath
            )
            if chapterText == nil { loadFailed = true }
        case .audio:
            chapterText = nil
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
