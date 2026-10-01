import Foundation
import SwiftCrossUI
import Data
import SpeechLogic
import TTSEngine
import Appearance

/// The book reader: chapter text (epub spine extracted straight from the
/// archive), TOC navigation, and the speak bar driving the TTS tier per
/// chapter. The reader owns the BookPosition lifecycle — chapter changes
/// and stops persist it into the manifest, Resume picks it back up.
struct BookReaderPane: View {
    let book: Book
    let books: BooksStore

    @State private var tts = TTSController.shared
    @State private var prefs = Prefs.shared
    @State private var theme = ThemeController.shared
    @State private var chapterText: String?
    @State private var chapterIndex: Int = 0
    @State private var showToc = false
    @State private var loadFailed = false
    @State private var selectedAudioChapter = 0

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                Button("↩ Resume") {
                    if let position = book.position {
                        open(chapter: position.chapterIndex, resume: true)
                    }
                }
                .buttonStyle(.bordered)
                .disabled(book.position == nil)
                Button("◂ Chapter") { open(chapter: max(0, chapterIndex - 1)) }
                    .buttonStyle(.bordered)
                Text(chapterLabel)
                    .font(.callout)
                    .foregroundColor(theme.text)
                Button("Chapter ▸") { open(chapter: min(chapterCount - 1, chapterIndex + 1)) }
                    .buttonStyle(.bordered)
                Spacer()
                Button(showToc ? "Hide TOC" : "TOC") { showToc.toggle() }
                    .buttonStyle(.borderless)
            }
            if showToc {
                ScrollView {
                    VStack(alignment: .leading, spacing: 2) {
                        ForEach(Array((book.toc ?? []).enumerated()), id: \.offset) { _, entry in
                            Button {
                                if let spineIndex = entry.spineIndex {
                                    open(chapter: spineIndex)
                                }
                            } label: {
                                HStack {
                                    Text(entry.label)
                                        .font(.callout)
                                        .foregroundColor(entry.spineIndex == chapterIndex
                                            ? theme.accent : .gray)
                                    Spacer(minLength: 0)
                                }
                                .padding(.horizontal, 8)
                                .padding(.vertical, 3)
                                .background(theme.accent.opacity(entry.spineIndex == chapterIndex ? 0.12 : 0))
                                .cornerRadius(5)
                            }
                            .buttonStyle(.borderless)
                        }
                    }
                }
                .frame(maxHeight: 160)
            }
            HStack(spacing: 8) {
                if tts.isPlaying(noteId: book.id) {
                    if tts.state == .paused {
                        Button("▶ Resume") { tts.resume() }
                            .buttonStyle(.bordered)
                    } else {
                        Button("❙❙ Pause") { tts.pause() }
                            .buttonStyle(.bordered)
                    }
                    Button("■ Stop") {
                        tts.stop()
                        savePosition(fraction: lastFraction)
                    }
                    .buttonStyle(.bordered)
                } else {
                    Button("▶ Speak chapter") { speakCurrent() }
                        .buttonStyle(.bordered)
                        .foregroundColor(theme.accent)
                }
                if let error = tts.lastError {
                    Text(error).foregroundColor(.orange)
                }
                Spacer()
                if let pos = tts.position, pos.noteId == book.id {
                    Text("\(Int((pos.fraction * 100).rounded()))%")
                        .foregroundColor(theme.text)
                }
                Button("🗑") {
                    tts.stop()
                    books.moveToBin(book)
                }
                .buttonStyle(.borderless)
            }
            if tts.isPlaying(noteId: book.id), let sentence = tts.currentSentence {
                Text("▸ \(sentence)")
                    .font(.callout)
                    .foregroundColor(theme.accent)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8)
                    .background(theme.accent.opacity(0.08))
                    .cornerRadius(6)
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
                        .font(.system(size: readerFontSize))
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .padding(12)
        .task { initialOpen() }
    }

    /// Reader body text tracks the appearance text-size setting.
    private var readerFontSize: Double {
        16.0 * min(1.5, max(0.75, prefs.readerTextScale))
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
                            .buttonStyle(.bordered)
                    } else {
                        Button("❙❙ Pause") { audio.pause() }
                            .buttonStyle(.bordered)
                    }
                    Button("■ Stop") { audio.stop() }
                        .buttonStyle(.bordered)
                } else {
                    Button("▶ Play") {
                        let start = resumeSeconds()
                        audio.play(book: book, chapterIndex: selectedAudioChapter, startSeconds: start)
                    }
                    .buttonStyle(.bordered)
                    .foregroundColor(theme.accent)
                    if resumeSeconds() > 0 {
                        Button("↩ Resume chapter \(selectedAudioChapter + 1)") {
                            audio.play(book: book, chapterIndex: selectedAudioChapter, startSeconds: resumeSeconds())
                        }
                        .buttonStyle(.bordered)
                    }
                }
                if let error = audio.lastError {
                    Text(error).foregroundColor(.orange)
                }
                Spacer()
                if let pos = audio.position, pos.bookId == book.id {
                    Text("\(Self.clock(pos.seconds)) / \(Self.clock(chapterDuration))")
                        .foregroundColor(theme.text)
                }
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Array((book.audioChapters ?? []).enumerated()), id: \.offset) { index, chapter in
                        // SwiftCrossUI on Linux has no Color.primary; the
                        // accent tint distinguishes the active chapter.
                        Button {
                            selectedAudioChapter = index
                            if audio.isPlaying(bookId: book.id) {
                                audio.play(book: book, chapterIndex: index, startSeconds: chapter.startSeconds)
                            }
                        } label: {
                            HStack {
                                Text("\(index + 1). \(chapter.title)")
                                    .font(.callout)
                                    .foregroundColor(index == selectedAudioChapter
                                        ? theme.accent : .gray)
                                Spacer(minLength: 0)
                            }
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(theme.accent.opacity(index == selectedAudioChapter ? 0.12 : 0))
                            .cornerRadius(5)
                        }
                        .buttonStyle(.borderless)
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
            // Off the main actor: a large EPUB is a few MB of zip to walk,
            // and decompressing a chapter took visible time on the UI thread
            // (the whole pane re-laid out behind the freeze).
            let book = self.book
            let extraction = Task.detached(priority: .userInitiated) { () -> String? in
                guard let data = try? Data(
                    contentsOf: BooksStore.epubArchiveURL(book), options: .mappedIfSafe
                ) else { return nil }
                return BooksStore.chapterText(book: book, chapterIndex: index, archiveData: data)
            }
            loadFailed = false
            Task {
                let result = await extraction.value
                // A newer chapter may have been opened while this one
                // decompressed; only the live index may write the text.
                guard chapterIndex == index else { return }
                chapterText = result
                loadFailed = result == nil
            }
        case .pdf:
            // PDF chapters are page ranges; pdftotext extracts the range.
            // It is a process spawn with a waitUntilExit, so it too runs
            // off the main actor.
            guard let chapters = book.pdfChapters, index < chapters.count else {
                chapterText = nil
                return
            }
            let chapter = chapters[index]
            let pdfPath = BooksStore.bookDirectory(book.id)
                .appendingPathComponent("original.pdf").path
            let extraction = Task.detached(priority: .userInitiated) {
                PdfTextLinux.text(
                    pages: chapter.startPage + 1,
                    to: chapter.endPage + 1,
                    pdfPath: pdfPath
                )
            }
            loadFailed = false
            Task {
                let result = await extraction.value
                guard chapterIndex == index else { return }
                chapterText = result
                loadFailed = result == nil
            }
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
            voice: prefs.voiceForEngine(EngineKind(rawValue: prefs.engineKind) ?? .espeak),
            bookmarkKey: BookmarkStore.bookKey(book.id.uuidString, chapter: chapterIndex)
        )
    }

    private func savePosition(fraction: Double) {
        books.updatePosition(book, position: BookPosition(chapterIndex: chapterIndex, chapterFraction: fraction))
    }
}
