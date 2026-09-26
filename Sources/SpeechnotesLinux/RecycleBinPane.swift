import SwiftCrossUI
import Data

/// The recycle bin: recover or purge binned notes, or empty the whole bin.
/// iOS swipe actions became explicit buttons (SwiftCrossUI has no swipes).
struct RecycleBinPane: View {
    let notes: NotesStore

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                Text("Recycle Bin")
                Spacer()
                if !notes.deletedNotes.isEmpty {
                    Button("Empty bin") {
                        notes.emptyRecycleBin()
                    }
                }
            }
            if notes.deletedNotes.isEmpty {
                ContentUnavailableView {
                    Text("Bin is empty")
                } description: {
                    Text("Deleted notes wait here for \(Note.recycleRetentionDays) days.")
                }
            } else {
                ScrollView {
                    VStack(spacing: 4) {
                        ForEach(notes.deletedNotes) { note in
                            row(for: note)
                        }
                    }
                }
            }
        }
        .padding(8)
    }

    private func row(for note: Note) -> some View {
        HStack(spacing: 8) {
            VStack(spacing: 2) {
                Text(note.title)
                Text(caption(for: note))
            }
            Spacer()
            Button("Recover") {
                notes.recover(noteId: note.id)
            }
            Button("Delete forever") {
                notes.purge(noteId: note.id)
            }
        }
    }

    private func caption(for note: Note) -> String {
        if let days = note.recycleDaysRemaining {
            return "purges in \(days) day\(days == 1 ? "" : "s")"
        }
        return ""
    }
}
