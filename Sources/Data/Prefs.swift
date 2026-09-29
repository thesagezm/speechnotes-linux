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
public final class Prefs: ObservableObject {
    public static let shared = Prefs()

    @Published public var hasOnboarded: Bool { didSet { save() } }
    @Published public var activeNotebookScope: String { didSet { save() } }  // "all" or notebook UUID
    @Published public var notesSortOrder: String { didSet { save() } }        // SortOrder.rawValue
    @Published public var renderMarkdown: Bool { didSet { save() } }
    @Published public var readAlongEnabled: Bool { didSet { save() } }
    @Published public var engineKind: String { didSet { save() } }
    @Published public var voice: String { didSet { save() } }
    @Published public var rateMultiplier: Double { didSet { save() } }
    // Appearance ("system"|"light"|"dark", AccentChoice raw values).
    @Published public var appearance: String { didSet { save() } }
    @Published public var accentChoice: String { didSet { save() } }
    // Scales the editor/reader font; 1.0 is the default.
    @Published public var readerTextScale: Double { didSet { save() } }

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
        appearance = dict["appearance"] as? String ?? "system"
        accentChoice = dict["accentChoice"] as? String ?? "blue"
        readerTextScale = dict["readerTextScale"] as? Double ?? 1.0
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
    public func flushNow() {
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
            "appearance": appearance,
            "accentChoice": accentChoice,
            "readerTextScale": readerTextScale,
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: snapshot, options: [.sortedKeys])
            try data.write(to: Self.fileURL, options: .atomic)
        } catch {
            Log.error("Prefs: save failed: \(error)")
        }
    }
}
