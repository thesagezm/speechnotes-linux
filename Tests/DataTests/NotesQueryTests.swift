import XCTest
@testable import Data

/// The notes list's scope → filter → sort pipeline. This is the logic the
/// Linux list pane runs on every rebuild, and it is the only part of the
/// list that can be exercised without a display server.
final class NotesQueryTests: XCTestCase {
    private func makeNote(
        _ text: String,
        title: String? = nil,
        notebook: UUID? = nil,
        pinned: Bool = false,
        created: Date = Date(timeIntervalSince1970: 0),
        updated: Date = Date(timeIntervalSince1970: 0)
    ) -> Note {
        var note = Note()
        note.id = UUID()
        note.text = text
        note.explicitTitle = title
        note.notebookId = notebook
        note.isPinned = pinned
        note.createdAt = created
        note.updatedAt = updated
        return note
    }

    // MARK: - Scope

    func testAllNotebooksIncludesUnfiled() {
        let notebook = UUID()
        let filed = makeNote("filed body", notebook: notebook)
        let unfiled = makeNote("unfiled body")
        let result = NotesQuery.apply(
            [filed, unfiled], notebookId: nil, allNotebooks: true, query: "", sort: .edited
        )
        XCTAssertEqual(Set(result.map(\.id)), Set([filed.id, unfiled.id]))
    }

    func testNotebookScopeExcludesUnfiled() {
        let notebook = UUID()
        let filed = makeNote("filed body", notebook: notebook)
        let unfiled = makeNote("unfiled body")
        let result = NotesQuery.apply(
            [filed, unfiled], notebookId: notebook, allNotebooks: false, query: "", sort: .edited
        )
        XCTAssertEqual(result.map(\.id), [filed.id])
    }

    func testUnfiledScopeIsNilNotebook() {
        let notebook = UUID()
        let filed = makeNote("filed body", notebook: notebook)
        let unfiled = makeNote("unfiled body")
        let result = NotesQuery.apply(
            [filed, unfiled], notebookId: nil, allNotebooks: false, query: "", sort: .edited
        )
        XCTAssertEqual(result.map(\.id), [unfiled.id])
    }

    // MARK: - Search

    func testQueryMatchesBodyCaseInsensitively() {
        let note = makeNote("The Quick Brown Fox")
        XCTAssertEqual(
            NotesQuery.apply([note], notebookId: nil, allNotebooks: true, query: "QUICK", sort: .edited)
                .map(\.id),
            [note.id]
        )
        XCTAssertTrue(
            NotesQuery.apply([note], notebookId: nil, allNotebooks: true, query: "zebra", sort: .edited)
                .isEmpty
        )
    }

    /// A search box that cannot find "cafe" when the note says "café" reads
    /// as broken to anyone typing quickly.
    func testQueryIgnoresDiacritics() {
        let note = makeNote("Rendez-vous à un café demain")
        XCTAssertEqual(
            NotesQuery.apply([note], notebookId: nil, allNotebooks: true, query: "cafe", sort: .edited)
                .map(\.id),
            [note.id]
        )
    }

    func testEmptyQueryKeepsEverything() {
        let notes = [makeNote("a"), makeNote("b"), makeNote("c")]
        XCTAssertEqual(
            NotesQuery.apply(notes, notebookId: nil, allNotebooks: true, query: "", sort: .edited).count,
            3
        )
    }

    // MARK: - Ordering

    /// Pinned-first is the one rule that holds across every sort key.
    func testPinnedNotesComeFirstUnderEverySort() {
        for sort in NotesQuery.Sort.allCases {
            let old = makeNote(
                "old", title: "aaa", pinned: true,
                created: Date(timeIntervalSince1970: 0),
                updated: Date(timeIntervalSince1970: 0)
            )
            let new = makeNote(
                "new", title: "zzz", pinned: false,
                created: Date(timeIntervalSince1970: 10_000),
                updated: Date(timeIntervalSince1970: 10_000)
            )
            let result = NotesQuery.apply(
                [new, old], notebookId: nil, allNotebooks: true, query: "", sort: sort
            )
            XCTAssertEqual(result.first?.id, old.id, "pinned note sank under sort \(sort.rawValue)")
        }
    }

    func testEditedSortIsNewestFirst() {
        let older = makeNote("a", updated: Date(timeIntervalSince1970: 100))
        let newer = makeNote("b", updated: Date(timeIntervalSince1970: 200))
        let result = NotesQuery.apply(
            [older, newer], notebookId: nil, allNotebooks: true, query: "", sort: .edited
        )
        XCTAssertEqual(result.map(\.id), [newer.id, older.id])
    }

    func testCreatedSortIsNewestFirst() {
        let older = makeNote("a", created: Date(timeIntervalSince1970: 100), updated: Date(timeIntervalSince1970: 999))
        let newer = makeNote("b", created: Date(timeIntervalSince1970: 200), updated: Date(timeIntervalSince1970: 1))
        let result = NotesQuery.apply(
            [older, newer], notebookId: nil, allNotebooks: true, query: "", sort: .created
        )
        XCTAssertEqual(result.map(\.id), [newer.id, older.id])
    }

    func testTitleSortIsAlphabeticalAndCaseInsensitive() {
        let a = makeNote("1", title: "apple")
        let b = makeNote("2", title: "Banana")
        let c = makeNote("3", title: "cherry")
        let result = NotesQuery.apply(
            [c, b, a], notebookId: nil, allNotebooks: true, query: "", sort: .title
        )
        XCTAssertEqual(result.map(\.id), [a.id, b.id, c.id])
    }

    /// Without an explicit title the derived one is used, so a note is
    /// still findable by sort when the user never named it.
    func testTitleSortFallsBackToDerivedTitle() {
        let zzzFirst = makeNote("Alpha is the first sentence")
        let aaaFirst = makeNote("Zulu is the first sentence")
        let result = NotesQuery.apply(
            [zzzFirst, aaaFirst], notebookId: nil, allNotebooks: true, query: "", sort: .title
        )
        XCTAssertEqual(result.map(\.id), [zzzFirst.id, aaaFirst.id])
    }

    // MARK: - Stability

    /// Sorting must not depend on the input order, or the list would
    /// reshuffle itself on an unrelated store update.
    func testResultIsIndependentOfInputOrder() {
        let notes = (0..<12).map { index in
            makeNote(
                "body \(index)",
                title: "note \(index)",
                created: Date(timeIntervalSince1970: Double(index) * 100),
                updated: Date(timeIntervalSince1970: Double(index) * 100)
            )
        }
        let forwards = NotesQuery.apply(
            notes, notebookId: nil, allNotebooks: true, query: "body", sort: .edited
        ).map(\.id)
        let backwards = NotesQuery.apply(
            notes.reversed(), notebookId: nil, allNotebooks: true, query: "body", sort: .edited
        ).map(\.id)
        XCTAssertEqual(forwards, backwards)
    }

    // MARK: - Scope decoding

    /// "unfiled" is not a UUID; decoding it as one would silently show the
    /// whole library (or nothing) instead of the unfiled notes.
    func testScopeDecoding() {
        let notebook = UUID()

        let all = NotesQuery.notebookId(forScope: "all")
        XCTAssertEqual(all?.all, true)
        XCTAssertNil(all?.id)

        let unfiled = NotesQuery.notebookId(forScope: "unfiled")
        XCTAssertEqual(unfiled?.all, false)
        XCTAssertNil(unfiled?.id)

        let one = NotesQuery.notebookId(forScope: notebook.uuidString)
        XCTAssertEqual(one?.all, false)
        XCTAssertEqual(one?.id, notebook)

        // A stale preference naming a deleted notebook must report nil, not
        // be silently read as "unfiled".
        XCTAssertNil(NotesQuery.notebookId(forScope: "not-a-uuid"))
        XCTAssertNil(NotesQuery.notebookId(forScope: ""))
    }
}
