import Foundation
import SpeechLogic
import Log

/// Applies a parsed JEX archive into the stores — the Linux port of the
/// iOS JexImporter. Idempotent: existing notebook names merge, notes keep
/// their Joplin ids when free, re-importing never duplicates.
@MainActor
public enum JexImporter {
    public struct Outcome: Equatable {
        public var notesCreated = 0
        public var notesSkipped = 0
        public var notebooksCreated = 0
        public var notebooksMerged = 0
        public var imagesImported = 0
    }

    public enum ImportError: Error, CustomStringConvertible {
        case read(String)
        case parse(String)

        public var description: String {
            switch self {
            case .read(let detail): return "Could not read the file: \(detail)"
            case .parse(let detail): return "Not a valid JEX archive: \(detail)"
            }
        }
    }

    @discardableResult
    public static func importArchive(
        data: Data,
        into notesStore: NotesStore,
        notebooksStore: NotebooksStore
    ) throws -> Outcome {
        let archive: JexImport.Archive
        do {
            archive = try JexImport.parse(data)
        } catch let error as JexImport.ImportError {
            throw ImportError.parse("\(error)")
        } catch {
            throw ImportError.parse("\(error)")
        }
        return try apply(archive, into: notesStore, notebooksStore: notebooksStore)
    }

    @discardableResult
    private static func apply(
        _ archive: JexImport.Archive,
        into notesStore: NotesStore,
        notebooksStore: NotebooksStore
    ) throws -> Outcome {
        var outcome = Outcome()

        // ---- Notebooks (Joplin ids → app UUIDs, existing names merge) ----
        var notebookIDMap: [String: UUID] = [:]
        for imported in archive.notebooks where !imported.id.isEmpty {
            if let existing = notebooksStore.notebooks.first(where: {
                $0.name.caseInsensitiveCompare(imported.title) == .orderedSame
            }) {
                notebookIDMap[imported.id] = existing.id
                outcome.notebooksMerged += 1
                continue
            }
            guard let created = notebooksStore.create(name: imported.title) else {
                continue
            }
            notebookIDMap[imported.id] = created.id
            outcome.notebooksCreated += 1
        }

        var resourcesByID: [String: JexImport.ImportedResource] = [:]
        for resource in archive.resources {
            resourcesByID[resource.id] = resource
        }

        // ---- Notes ----
        for imported in archive.notes {
            let body = imported.body.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !body.isEmpty else {
                outcome.notesSkipped += 1
                continue
            }

            // Joplin semantics: a note whose id already exists is skipped —
            // re-importing an archive never duplicates the library.
            guard let noteID = JexImport.uuid(fromJoplinId: imported.id),
                  !notesStore.allNotes.contains(where: { $0.id == noteID }) else {
                if notesStore.allNotes.contains(where: {
                    $0.text.trimmingCharacters(in: .whitespacesAndNewlines) == body
                }) {
                    outcome.notesSkipped += 1
                }
                continue
            }

            // Rewrite image links: ../resources/<id>.png → local target.
            var markdown = body
            var imageTargets: [String: String] = [:]
            for (resourceID, link) in imported.resources {
                guard let resource = resourcesByID[resourceID] else { continue }
                guard let target = NoteImageStore.importImageData(
                    resource.data,
                    pathExtension: resource.fileExtension,
                    noteId: noteID
                ) else { continue }
                imageTargets[resourceID] = target
                markdown = markdown.replacingOccurrences(
                    of: link,
                    with: target,
                    options: .literal
                )
            }
            outcome.imagesImported += imageTargets.count

            let note = notesStore.createNote(
                id: noteID,
                notebookId: imported.notebookId.flatMap { notebookIDMap[$0] }
            )
            var mutable = note
            mutable.text = markdown
            let trimmedTitle = imported.title.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmedTitle.isEmpty {
                mutable.explicitTitle = trimmedTitle
            }
            mutable.createdAt = imported.createdAt
            mutable.updatedAt = imported.updatedAt
            notesStore.update(mutable)
            outcome.notesCreated += 1
        }

        Log.info(
            "JexImporter: \(outcome.notesCreated) notes, \(outcome.notebooksCreated) notebooks " +
            "(\(outcome.notebooksMerged) merged), \(outcome.imagesImported) images, " +
            "\(outcome.notesSkipped) skipped"
        )
        return outcome
    }
}
