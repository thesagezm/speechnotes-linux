import Foundation
import SwiftCrossUI
import Data
import SpeechLogic
import Log

/// MainActor facade over audiobook playback: one chapter at a time, auto-
/// advancing into the next chapter on a clean finish (continuous listening),
/// resuming from per-chapter second bookmarks, and mutually exclusive with
/// the TTS tier — starting either stops the other, one sound at a time.
@MainActor
public final class AudioBookController: ObservableObject {
    public static let shared = AudioBookController()

    @Published public private(set) var state: TTSState = .idle
    /// The chapter currently sounding and the audible clock inside it.
    @Published public private(set) var position: (bookId: UUID, chapterIndex: Int, seconds: Double, fraction: Double)?
    @Published public private(set) var lastError: String?

    private var flags = ControlFlags()
    private var worker: Task<Void, Never>?
    private var playingBookId: UUID?
    private var activeBookmarkKey: String?
    /// Book snapshot the current run resolves chapters against.
    private var currentBook: Book?
    private var currentChapterIndex: Int = 0

    private init() {}

    public var isBusy: Bool { state == .speaking || state == .paused }

    public func isPlaying(bookId: UUID) -> Bool {
        isBusy && playingBookId == bookId
    }

    /// Plays one chapter (or resumes at `startSeconds`). Auto-advances into
    /// `book.audioChapters[chapterIndex + 1]` when the chapter plays out.
    /// Soft-fails like the TTS tier: never worse than silence.
    public func play(
        book: Book,
        chapterIndex: Int,
        startSeconds: Double = 0
    ) {
        stop()
        guard book.format == .audio else { return }
        guard let chapters = book.audioChapters, chapterIndex >= 0, chapterIndex < chapters.count else {
            return
        }

        TTSController.shared.stop()

        let fileURL = BooksStore.resolveAudioOriginalURL(book: book)
        guard FileManager.default.fileExists(atPath: fileURL.path) else {
            lastError = "audiobook file missing (\(fileURL.lastPathComponent))"
            Log.error("AudioBook: \(lastError ?? "?")")
            return
        }

        let flags = ControlFlags()
        self.flags = flags
        currentBook = book
        currentChapterIndex = chapterIndex
        playingBookId = book.id
        state = .speaking
        position = nil
        lastError = nil
        activeBookmarkKey = BookmarkStore.bookKey(book.id.uuidString, chapter: chapterIndex)

        worker = Task.detached(priority: .userInitiated) { [weak self] in
            await self?.runChapter(
                book: book,
                fileURL: fileURL,
                chapters: chapters,
                chapterIndex: chapterIndex,
                startSeconds: startSeconds,
                flags: flags
            )
        }
    }

    public func stop() {
        flags.stop()
        worker?.cancel()
        worker = nil
        // A manual stop keeps the position — Resume picks it up.
        if let pos = position, let key = activeBookmarkKey {
            BookmarkStore.shared.set(
                key,
                PlaybackBookmark(noteId: pos.bookId, textOffset: Int(pos.seconds * 1000))
            )
        }
        state = .idle
        position = nil
        playingBookId = nil
        currentBook = nil
        activeBookmarkKey = nil
    }

    public func pause() {
        guard state == .speaking else { return }
        flags.setPaused(true)
        state = .paused
    }

    public func resume() {
        guard state == .paused else { return }
        flags.setPaused(false)
        state = .speaking
    }

    /// The worker body: play the requested chapter, then auto-advance on a
    /// clean finish. Guards on the book id so a replaced worker can't
    /// clobber a newer run.
    private nonisolated func runChapter(
        book: Book,
        fileURL: URL,
        chapters: [AudioChapter],
        chapterIndex: Int,
        startSeconds: Double,
        flags: ControlFlags
    ) async {
        let player = BookAudioPlayer()
        let chapter = chapters[chapterIndex]
        let span = max(0.1, chapter.endSeconds - max(chapter.startSeconds, startSeconds))
        let bookId = book.id

        do {
            try player.play(
                fileURL: fileURL,
                startSeconds: max(chapter.startSeconds, startSeconds),
                durationSeconds: span,
                flags: flags,
                onPosition: { [weak self] seconds, fraction in
                    Task { @MainActor [weak self] in
                        self?.position = (bookId, chapterIndex, seconds, fraction)
                        // BookmarkStore throttles with a trailing write.
                        BookmarkStore.shared.set(
                            BookmarkStore.bookKey(bookId.uuidString, chapter: chapterIndex),
                            PlaybackBookmark(noteId: bookId, textOffset: Int(seconds * 1000))
                        )
                    }
                }
            )
        } catch {
            Log.error("AudioBook chapter \(chapterIndex): \(error)")
            await self.finish(bookId: bookId, error: "chapter \(chapterIndex + 1): \(error)")
            return
        }

        if flags.isStopped {
            await self.finish(bookId: bookId, error: nil)
            return
        }
        // Clean chapter end: park the bookmark at the next chapter's start
        // and keep listening.
        let next = chapterIndex + 1
        if next < chapters.count {
            await self.advance(to: next)
        } else {
            // The book played to the end — the resume bookmark served its
            // purpose.
            await BookmarkStore.shared.remove(
                BookmarkStore.bookKey(bookId.uuidString, chapter: chapterIndex)
            )
            await self.finish(bookId: bookId, error: nil)
        }
    }

    /// Chapter rollover inside the same worker — replaces the run in place.
    private func advance(to nextChapter: Int) {
        guard let book = currentBook, playingBookId == book.id,
              let chapters = book.audioChapters, nextChapter < chapters.count else {
            finish(bookId: playingBookId ?? UUID(), error: nil)
            return
        }
        let flags = ControlFlags()
        self.flags = flags
        currentChapterIndex = nextChapter
        activeBookmarkKey = BookmarkStore.bookKey(book.id.uuidString, chapter: nextChapter)
        let fileURL = BooksStore.resolveAudioOriginalURL(book: book)
        worker = Task.detached(priority: .userInitiated) { [weak self] in
            await self?.runChapter(
                book: book,
                fileURL: fileURL,
                chapters: chapters,
                chapterIndex: nextChapter,
                startSeconds: chapters[nextChapter].startSeconds,
                flags: flags
            )
        }
    }

    private func finish(bookId: UUID, error: String?) {
        guard playingBookId == bookId else { return }
        if let error {
            lastError = error
        }
        state = .idle
        position = nil
        playingBookId = nil
        currentBook = nil
        activeBookmarkKey = nil
    }
}
