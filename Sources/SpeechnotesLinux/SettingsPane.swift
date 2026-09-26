import Foundation
import SwiftCrossUI
import Data
import TTSEngine

/// Preferences (Prefs parity): speech toggles and rate, plus a save-now
/// button and library stats. The engine picker joins in Phase 3 when the
/// engine tier exists to back it.
struct SettingsPane: View {
    let prefs: Prefs
    let notes: NotesStore
    let notebooks: NotebooksStore

    @State private var piperStatus = ""

    var body: some View {
        VStack(spacing: 12) {
            HStack(spacing: 8) {
                Text("Settings")
                Spacer()
            }
            HStack(spacing: 8) {
                Button("Engine: \(EngineKind(rawValue: prefs.engineKind)?.displayName ?? prefs.engineKind)") {
                    prefs.engineKind =
                        prefs.engineKind == EngineKind.espeak.rawValue
                        ? EngineKind.piper.rawValue
                        : EngineKind.espeak.rawValue
                }
                Spacer()
            }
            Toggle("Read along while speaking", isOn: readAlongBinding)
            Toggle("Render Markdown in the editor", isOn: renderMarkdownBinding)
            HStack(spacing: 8) {
                Text("Speech rate")
                Slider(value: rateBinding, in: 0.5...2.0)
                Text(rateLabel)
            }
            HStack(spacing: 8) {
                Button("Download Piper voice (\(PiperModelManager.curatedVoice), ~63 MB)") {
                    piperStatus = "downloading…"
                    Task { @MainActor in
                        do {
                            _ = try await PiperModelManager.download()
                            piperStatus = "installed ✓"
                        } catch {
                            piperStatus = "failed: \(error)"
                        }
                    }
                }
                Text(piperStatus)
                Spacer()
            }
            HStack(spacing: 8) {
                Button("Save everything now") {
                    notes.flushNow()
                    prefs.flushNow()
                    BookmarkStore.shared.persistNow()
                }
                Spacer()
            }
            Spacer()
            HStack(spacing: 8) {
                Text(stats)
                Spacer()
            }
        }
        .padding(12)
    }

    private var stats: String {
        "\(notes.notes.count) note(s), \(notes.deletedNotes.count) in bin, "
            + "\(notebooks.notebooks.count) notebook(s)"
    }

    private var rateLabel: String {
        String(format: "%.2f×", prefs.rateMultiplier)
    }

    private var readAlongBinding: Binding<Bool> {
        Binding(
            get: { prefs.readAlongEnabled },
            set: { prefs.readAlongEnabled = $0 }
        )
    }

    private var renderMarkdownBinding: Binding<Bool> {
        Binding(
            get: { prefs.renderMarkdown },
            set: { prefs.renderMarkdown = $0 }
        )
    }

    private var rateBinding: Binding<Double> {
        Binding(
            get: { prefs.rateMultiplier },
            set: { prefs.rateMultiplier = min(2.0, max(0.5, $0)) }
        )
    }
}
