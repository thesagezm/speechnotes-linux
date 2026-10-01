import Foundation
import SwiftCrossUI
import BookDrop
import Data
import SpeechLogic
import Log

/// BookDrop bring-up. Split out of BooksPane because the receiver has to be
/// wired and started at launch: the router is what turns a landed file into
/// an imported book or note, and installing it lazily from the Books pane
/// meant the feature only worked if the user happened to visit that pane
/// before a transfer arrived.
@MainActor
enum BookDropService {
    private static let enabledKey = "bookDropEnabled"
    private static let autoAcceptKey = "bookDropAutoAccept"

    private static var didConfigure = false

    /// Called once from the app shell. Idempotent.
    static func configure() {
        guard !didConfigure else { return }
        didConfigure = true

        let receiver = LocalSendReceiver.shared
        receiver.autoAccept = UserDefaults.standard.object(forKey: autoAcceptKey) as? Bool ?? true
        receiver.router = { url in
            await route(url)
        }

        if UserDefaults.standard.bool(forKey: enabledKey) {
            Log.info("BookDrop: enabled by preference, starting receiver")
            receiver.start()
        }
    }

    static var isEnabled: Bool {
        UserDefaults.standard.bool(forKey: enabledKey)
    }

    static func setEnabled(_ on: Bool) {
        UserDefaults.standard.set(on, forKey: enabledKey)
        configure()
        LocalSendReceiver.shared.setEnabled(on)
    }

    static var autoAccept: Bool {
        get { UserDefaults.standard.object(forKey: autoAcceptKey) as? Bool ?? true }
        set {
            UserDefaults.standard.set(newValue, forKey: autoAcceptKey)
            LocalSendReceiver.shared.autoAccept = newValue
        }
    }

    /// One landed file → the right import pipeline. Runs on the main actor
    /// (the router contract) but the heavy work is already off-thread inside
    /// BooksStore.importBook / the JEX decode.
    private static func route(_ url: URL) async {
        let receiver = LocalSendReceiver.shared
        let name = url.lastPathComponent
        let bytes = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.size]
            as? Int64 ?? 0

        if url.pathExtension.lowercased() == "jex" {
            do {
                let data = try Data(contentsOf: url, options: .mappedIfSafe)
                let outcome = try JexImporter.importArchive(
                    data: data,
                    into: NotesStore.shared,
                    notebooksStore: NotebooksStore.shared
                )
                receiver.reportImport(
                    name: name,
                    size: bytes,
                    outcome: .imported("\(outcome.notesCreated) note(s)")
                )
                Log.info("BookDrop: imported \(name) → \(outcome.notesCreated) note(s)")
            } catch {
                receiver.reportImport(name: name, size: bytes, outcome: .failed("\(error)"))
                Log.error("BookDrop: JEX import failed for \(name): \(error)")
            }
            try? FileManager.default.removeItem(at: url)
            return
        }

        let books = BooksStore.shared
        if let book = await books.importBook(from: url) {
            receiver.reportImport(name: book.title, size: bytes, outcome: .imported("added to shelf"))
            Log.info("BookDrop: imported \(name) as “\(book.title)”")
        } else {
            let reason = books.importError ?? "import failed"
            receiver.reportImport(name: name, size: bytes, outcome: .failed(reason))
            Log.error("BookDrop: import failed for \(name): \(reason)")
        }
    }

    static func outcomeLabel(_ outcome: BookDropRecord.Outcome) -> String {
        switch outcome {
        case .imported(let detail): return detail
        case .failed(let reason): return "failed (\(reason))"
        case .rejected: return "rejected"
        }
    }
}
