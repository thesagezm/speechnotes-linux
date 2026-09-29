import XCTest
@testable import Data
@testable import AppPaths

/// Prefs persistence, focused on the appearance keys added alongside the
/// 2026-09-29 UI pass: new keys must round-trip and default sensibly.
final class PrefsTests: XCTestCase {

    @MainActor
    private func freshHome() throws -> URL {
        let home = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("speechnotes-prefs-tests-\(UUID().uuidString)", isDirectory: true)
        setenv("XDG_DATA_HOME", home.path, 1)
        setenv("XDG_CONFIG_HOME", home.path, 1)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        _ = AppPaths.ensureDirectories()
        return home
    }

    @MainActor
    func testAppearanceRoundTripsThroughFlush() throws {
        let home = try freshHome()
        defer { try? FileManager.default.removeItem(at: home) }

        let prefs = Prefs.shared
        prefs.appearance = "dark"
        prefs.accentChoice = "teal"
        prefs.readerTextScale = 1.25
        prefs.flushNow()

        let data = try Data(contentsOf: AppPaths.configDir.appendingPathComponent("prefs.json"))
        let dict = try XCTUnwrap(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(dict["appearance"] as? String, "dark")
        XCTAssertEqual(dict["accentChoice"] as? String, "teal")
        XCTAssertEqual(dict["readerTextScale"] as? Double, 1.25)
    }
}
