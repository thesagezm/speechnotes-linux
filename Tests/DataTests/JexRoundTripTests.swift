import XCTest
@testable import Data
@testable import AppPaths
import SpeechLogic

/// JEX round-trip through the real stores: export the library, wipe it,
/// import the archive back — notes, notebooks, titles and timestamps
/// survive; re-importing merges instead of duplicating.
final class JexRoundTripTests: XCTestCase {

    private func freshHome() throws -> URL {
        let home = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("speechnotes-jex-tests-\(UUID().uuidString)", isDirectory: true)
        setenv("XDG_DATA_HOME", home.path, 1)
        setenv("XDG_CONFIG_HOME", home.path, 1)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        return home
    }

    @MainActor
    func testExportThenImportRestoresLibrary() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()

        // --- Build a small library ---
        let notebooks = NotebooksStore()
        let notes = NotesStore()
        let work = try XCTUnwrap(notebooks.create(name: "Work"))
        var a = notes.createNote(notebookId: work.id)
        a.text = "Alpha note body. Two sentences."
        a.explicitTitle = "Alpha"
        notes.update(a)
        var b = notes.createNote()
        b.text = "Beta note body, unfiled."
        notes.update(b)

        // --- Export ---
        let payloads = JexPayloads.payloads(notes: notes.notes, notebooks: notebooks.notebooks)
        let archive = try JexExport.buildArchive(notes: payloads.notes, notebooks: payloads.notebooks)
        XCTAssertGreaterThan(archive.count, 100)

        // --- Import into a FRESH library (simulates restore) ---
        // A genuinely different home — the restore target.
        let home2 = home.deletingLastPathComponent()
            .appendingPathComponent("restore-\(UUID().uuidString)", isDirectory: true)
        setenv("XDG_DATA_HOME", home2.path, 1)
        setenv("XDG_CONFIG_HOME", home2.path, 1)
        defer { try? FileManager.default.removeItem(at: home2) }
        _ = AppPaths.ensureDirectories()

        let freshNotes = NotesStore()
        let freshNotebooks = NotebooksStore()
        let outcome = try JexImporter.importArchive(
            data: archive, into: freshNotes, notebooksStore: freshNotebooks
        )
        XCTAssertEqual(outcome.notesCreated, 2)
        XCTAssertEqual(outcome.notebooksCreated, 1)
        XCTAssertEqual(freshNotes.notes.count, 2)
        XCTAssertEqual(freshNotebooks.notebooks.first?.name, "Work")

        // Titles, bodies, notebook placement and timestamps survive.
        let restoredAlpha = try XCTUnwrap(freshNotes.notes.first { $0.explicitTitle == "Alpha" })
        XCTAssertTrue(restoredAlpha.text.hasPrefix("Alpha note body."))
        XCTAssertEqual(restoredAlpha.notebookId, freshNotebooks.notebooks.first?.id)
        XCTAssertEqual(restoredAlpha.id, a.id, "Joplin ids restore exactly")
        XCTAssertEqual(restoredAlpha.createdAt.timeIntervalSince1970,
                       a.createdAt.timeIntervalSince1970, accuracy: 1.0)

        // --- Re-import merges, never duplicates ---
        let second = try JexImporter.importArchive(
            data: archive, into: freshNotes, notebooksStore: freshNotebooks
        )
        XCTAssertEqual(second.notebooksMerged, 1, "existing notebook name merges")
        XCTAssertEqual(second.notesSkipped, 2, "same-id notes are skipped, not duplicated")
        XCTAssertEqual(freshNotes.notes.count, 2, "re-import never duplicates")
    }
}
