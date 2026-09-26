import Foundation
import SwiftCrossUI
import AppPaths
import Log

/// User preferences, persisted as JSON under XDG config.
///
/// SwiftCrossUI has no `@AppStorage`, so this ObservableObject is the stand-in.
/// Key names match the iOS app's UserDefaults keys verbatim so the two apps
/// share a vocabulary.
@MainActor
final class Prefs: ObservableObject {
    static let shared = Prefs()

    @Published var hasOnboarded: Bool { didSet { save() } }
    @Published var activeNotebookScope: String { didSet { save() } }  // "all" or notebook UUID
    @Published var notesSortOrder: String { didSet { save() } }        // SortOrder.rawValue
    @Published var renderMarkdown: Bool { didSet { save() } }
    @Published var readAlongEnabled: Bool { didSet { save() } }
    @Published var engineKind: String { didSet { save() } }
    @Published var voice: String { didSet { save() } }
    @Published var rateMultiplier: Double { didSet { save() } }

    private init() {
        let dict = (try? JSONSerialization.jsonObject(
            with: Data(contentsOf: Self.fileURL)
        )) as? [String: Any] ?? [:]
        hasOnboarded = dict["hasOnboarded"] as? Bool ?? false
        activeNotebookScope = dict["activeNotebookScope"] as? String ?? "all"
        notesSortOrder = dict["notesSortOrder"] as? String ?? "edited"
        renderMarkdown = dict["renderMarkdown"] as? Bool ?? false
        readAlongEnabled = dict["readAlongEnabled"] as? Bool ?? true
        engineKind = dict["engineKind"] as? String ?? "espeak"
        voice = dict["voice"] as? String ?? ""
        rateMultiplier = dict["rateMultiplier"] as? Double ?? 1.0
    }

    private static var fileURL: URL {
        AppPaths.configDir.appendingPathComponent("prefs.json")
    }

    private var saveTask: Task<Void, Never>?

    /// Debounced write with a guaranteed trailing flush (the iOS
    /// BookmarkStore lesson: a pure debounce starves under sustained writes).
    private func save() {
        saveTask?.cancel()
        saveTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 400_000_000)
            guard !Task.isCancelled else { return }
            self?.flushNow()
        }
    }

    /// Synchronous write — used by the debounce timer and at shutdown.
    func flushNow() {
        saveTask?.cancel()
        saveTask = nil
        let snapshot: [String: Any] = [
            "hasOnboarded": hasOnboarded,
            "activeNotebookScope": activeNotebookScope,
            "notesSortOrder": notesSortOrder,
            "renderMarkdown": renderMarkdown,
            "readAlongEnabled": readAlongEnabled,
            "engineKind": engineKind,
            "voice": voice,
            "rateMultiplier": rateMultiplier,
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: snapshot, options: [.sortedKeys])
            try data.write(to: Self.fileURL, options: .atomic)
        } catch {
            Log.error("Prefs: save failed: \(error)")
        }
    }
}
