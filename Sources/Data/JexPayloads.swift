import Foundation
import Data
import SpeechLogic

/// Bridges the stores' models into JexExport payloads — the app-layer glue
/// iOS keeps in BackupExportView. Joplin ids are 32 lowercase hex chars;
/// the app's UUIDs convert losslessly both ways (JexImport does the mirror
/// on import).
public enum JexPayloads {
    public static func joplinId(from uuid: UUID) -> String {
        uuid.uuidString.replacingOccurrences(of: "-", with: "").lowercased()
    }

    public static func payloads(
        notes: [Note],
        notebooks: [Notebook]
    ) -> (notes: [JexExport.NotePayload], notebooks: [JexExport.NotebookPayload]) {
        let notebookPayloads = notebooks.map { notebook in
            JexExport.NotebookPayload(
                id: joplinId(from: notebook.id),
                title: notebook.name,
                parentId: nil,
                createdAt: notebook.createdAt
            )
        }
        let notePayloads = notes.map { note in
            JexExport.NotePayload(
                id: joplinId(from: note.id),
                title: note.explicitTitle ?? note.title,
                markdownBody: note.text,
                notebookId: note.notebookId.map { joplinId(from: $0) },
                createdAt: note.createdAt,
                updatedAt: note.updatedAt,
                images: []
            )
        }
        return (notePayloads, notebookPayloads)
    }
}
