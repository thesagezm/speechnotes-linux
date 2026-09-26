import XCTest
@testable import Data

/// Model-level tests: title derivation, metadata helpers, and tolerant
/// decoding of older notes.json shapes.
final class NoteTests: XCTestCase {

    func testTitleDerivesFromFirstSentence() {
        var note = Note()
        note.text = "Hello world. Second sentence here."
        // iOS parity: the terminal punctuation stays — the title is the
        // first sentence verbatim.
        XCTAssertEqual(note.title, "Hello world.")
    }

    func testExplicitTitleWinsAndIsTruncated() {
        var note = Note()
        note.text = "Body ignored."
        note.explicitTitle = "  My title  "
        XCTAssertEqual(note.title, "My title")
        note.explicitTitle = String(repeating: "x", count: 200)
        XCTAssertEqual(note.title.count, 120)
    }

    func testUntitledFallback() {
        let note = Note()
        XCTAssertEqual(note.title, "Untitled note")
    }

    func testWordCountAndListenMinutes() {
        var note = Note()
        XCTAssertEqual(note.wordCount, 0)
        XCTAssertNil(note.estimatedListenMinutes)

        note.text = "one two three four five"
        XCTAssertEqual(note.wordCount, 5)
        // 5/145 rounds to 0 → floored at 1 minute.
        XCTAssertEqual(note.estimatedListenMinutes, 1)

        note.text = String(repeating: "word ", count: 290).trimmingCharacters(in: .whitespaces)
        XCTAssertEqual(note.wordCount, 290)
        XCTAssertEqual(note.estimatedListenMinutes, 2)
    }

    func testRecycleDaysRemaining() {
        var note = Note()
        XCTAssertNil(note.recycleDaysRemaining)
        note.deletedAt = Date()
        XCTAssertEqual(note.recycleDaysRemaining, Note.recycleRetentionDays)
        note.deletedAt = Date().addingTimeInterval(-29 * 24 * 3600)
        XCTAssertEqual(note.recycleDaysRemaining, 1)
    }

    func testDecodesLegacyNoteWithoutNewKeys() throws {
        // Pre-v1.3 shape: no explicitTitle / deletedAt / notebook / flags.
        let json = """
        {"id":"11111111-1111-1111-1111-111111111111","text":"legacy body","createdAt":0,"updatedAt":0}
        """
        let note = try JSONDecoder().decode(Note.self, from: Data(json.utf8))
        XCTAssertEqual(note.id.uuidString, "11111111-1111-1111-1111-111111111111")
        XCTAssertEqual(note.text, "legacy body")
        XCTAssertNil(note.explicitTitle)
        XCTAssertNil(note.deletedAt)
        XCTAssertNil(note.notebookId)
        XCTAssertFalse(note.isPinned)
        XCTAssertFalse(note.isFavorite)
    }

    func testDecodesEmptyObjectWithGeneratedID() throws {
        let note = try JSONDecoder().decode(Note.self, from: Data("{}".utf8))
        XCTAssertEqual(note.text, "")
        XCTAssertEqual(note.title, "Untitled note")
    }
}
