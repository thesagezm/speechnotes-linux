import Foundation
import SwiftCrossUI
import Data

/// The note editor: optional explicit title, the body TextEditor, and the
/// pin/star/delete controls. SwiftCrossUI's TextEditor is a plain
/// Binding<String> (no attributed ranges), so formatting flows into the text
/// at the end — same constraint the plan recorded for dsnote parity.
struct NoteEditorPane: View {
    let note: Note
    let notes: NotesStore

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                Button(note.isPinned ? "Unpin" : "Pin") {
                    notes.setPinned(!note.isPinned, noteId: note.id)
                }
                Button(note.isFavorite ? "Unstar" : "Star") {
                    notes.setFavorite(!note.isFavorite, noteId: note.id)
                }
                Spacer()
                Text(summary)
                Button("Delete") {
                    notes.delete(noteId: note.id)
                }
            }
            TextField("Title (optional)", text: titleBinding)
            TextEditor(text: textBinding)
        }
        .padding(8)
    }

    private var summary: String {
        var parts = ["\(note.wordCount) words"]
        if let minutes = note.estimatedListenMinutes {
            parts.append("~\(minutes) min listen")
        }
        return parts.joined(separator: " · ")
    }

    /// Writes through the store so every keystroke bumps updatedAt and hits
    /// the debounced save — the editor never owns note state.
    private var textBinding: Binding<String> {
        Binding(
            get: {
                notes.allNotes.first(where: { $0.id == note.id })?.text ?? ""
            },
            set: { newText in
                guard var current = notes.allNotes.first(where: { $0.id == note.id }) else {
                    return
                }
                current.text = newText
                notes.update(current)
            }
        )
    }

    /// Empty string clears the explicit title (falls back to derived).
    private var titleBinding: Binding<String> {
        Binding(
            get: {
                notes.allNotes.first(where: { $0.id == note.id })?.explicitTitle ?? ""
            },
            set: { newTitle in
                guard var current = notes.allNotes.first(where: { $0.id == note.id }) else {
                    return
                }
                let trimmed = newTitle.trimmingCharacters(in: .whitespacesAndNewlines)
                current.explicitTitle = trimmed.isEmpty ? nil : trimmed
                notes.update(current)
            }
        )
    }
}
