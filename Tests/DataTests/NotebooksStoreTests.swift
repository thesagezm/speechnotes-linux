import XCTest
@testable import Data
@testable import AppPaths

/// Notebook container rules: unique non-empty names, rename guards, and
/// corrupt-file quarantine (Joplin-parity containers).
///
/// Linux XCTest has no isolated setUp overrides, so each test claims its own
/// throwaway XDG home (AppPaths re-reads XDG_*_HOME on every access) and
/// cleans up with `defer`.
final class NotebooksStoreTests: XCTestCase {

    private func freshHome() throws -> URL {
        let home = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("speechnotes-nb-tests-\(UUID().uuidString)", isDirectory: true)
        setenv("XDG_DATA_HOME", home.path, 1)
        setenv("XDG_CONFIG_HOME", home.path, 1)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        return home
    }

    @MainActor
    func testCreateRejectsBlankAndDuplicateNames() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()

        let store = NotebooksStore()
        XCTAssertNil(store.create(name: "   "))
        let inbox = store.create(name: "Inbox")
        XCTAssertNotNil(inbox)
        XCTAssertNil(store.create(name: "inbox"), "names are unique case-insensitively")
        XCTAssertNil(store.create(name: "Inbox"))
        XCTAssertEqual(store.notebooks.count, 1)
    }

    @MainActor
    func testRenameGuardsAndDelete() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()

        let store = NotebooksStore()
        let a = store.create(name: "Work")!
        let b = store.create(name: "Home")!

        store.rename(a, to: "   ")
        XCTAssertEqual(store.name(for: a.id), "Work")

        store.rename(a, to: "home") // conflicts with b
        XCTAssertEqual(store.name(for: a.id), "Work")

        store.rename(a, to: "Office")
        XCTAssertEqual(store.name(for: a.id), "Office")

        store.delete(b)
        XCTAssertNil(store.name(for: b.id))
        XCTAssertEqual(store.notebooks.count, 1)
    }

    @MainActor
    func testCorruptNotebooksJSONIsQuarantined() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }
        _ = AppPaths.ensureDirectories()

        let url = AppPaths.dataDir.appendingPathComponent("notebooks.json")
        try Data("{broken".utf8).write(to: url)

        let store = NotebooksStore()
        XCTAssertTrue(store.notebooks.isEmpty)
        let quarantined = try FileManager.default.contentsOfDirectory(atPath: AppPaths.dataDir.path)
            .filter { $0.hasPrefix("notebooks.json.corrupt-") }
        XCTAssertEqual(quarantined.count, 1)
    }
}
