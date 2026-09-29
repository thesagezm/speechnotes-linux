import Foundation
import SwiftCrossUI
import Data
import SpeechLogic
import TTSEngine

/// Preferences (Prefs parity): speech toggles and rate, plus a save-now
/// button and library stats. The engine picker joins in Phase 3 when the
/// engine tier exists to back it.
struct SettingsPane: View {
    let prefs: Prefs
    let notes: NotesStore
    let notebooks: NotebooksStore

    @State private var piperStatus = ""
    @Environment(\.chooseFile) private var chooseFile
    @Environment(\.chooseFileSaveDestination) private var chooseFileSaveDestination

    var body: some View {
        VStack(spacing: 12) {
            HStack(spacing: 8) {
                Text("Settings")
                Spacer()
            }
            HStack(spacing: 8) {
                Button("Engine: \(EngineKind(rawValue: prefs.engineKind)?.displayName ?? prefs.engineKind)") {
                    switch EngineKind(rawValue: prefs.engineKind) {
                    case .espeak: prefs.engineKind = EngineKind.piper.rawValue
                    case .piper: prefs.engineKind = EngineKind.kokoro.rawValue
                    case .kokoro: prefs.engineKind = EngineKind.supertonic.rawValue
                    case .supertonic: prefs.engineKind = EngineKind.pico.rawValue
                    default: prefs.engineKind = EngineKind.espeak.rawValue
                    }
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
                Spacer()
            }
            HStack(spacing: 8) {
                Button("Download Kokoro (82M, ~190 MB)") {
                    piperStatus = "downloading Kokoro…"
                    Task { @MainActor in
                        do {
                            _ = try await KokoroModelManager.download()
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
                Button("Download Supertonic (~260 MB)") {
                    piperStatus = "downloading Supertonic…"
                    Task { @MainActor in
                        do {
                            _ = try await SupertonicModelManager.download()
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
            HStack(spacing: 8) {
                Button("Export library (JEX)…") {
                    Task { await exportLibrary() }
                }
                Button("Import JEX…") {
                    Task { await importJex() }
                }
                Spacer()
            }
            if let status = jexStatus {
                Text(status).foregroundColor(.gray)
            }
            Spacer()
            HStack(spacing: 8) {
                Text(stats)
                Spacer()
            }
        }
        .padding(12)
    }

    @State private var jexStatus: String?

    private func exportLibrary() async {
        guard let url = await chooseFileSaveDestination(
            title: "Export library as JEX",
            defaultButtonLabel: "Export",
            defaultFileName: "speechnotes.jex"
        ) else { return }
        do {
            let payloads = JexPayloads.payloads(notes: notes.notes, notebooks: notebooks.notebooks)
            let data = try JexExport.buildArchive(notes: payloads.notes, notebooks: payloads.notebooks)
            try data.write(to: url, options: .atomic)
            jexStatus = "Exported \(notes.notes.count) note(s) → \(url.lastPathComponent)"
        } catch {
            jexStatus = "Export failed: \(error)"
        }
    }

    private func importJex() async {
        guard let url = await chooseFile(
            title: "Import JEX",
            message: "Joplin-compatible archive",
            defaultButtonLabel: "Import"
        ) else { return }
        do {
            let data = try Data(contentsOf: url, options: .mappedIfSafe)
            let outcome = try JexImporter.importArchive(data: data, into: notes, notebooksStore: notebooks)
            jexStatus = "Imported \(outcome.notesCreated) note(s), \(outcome.notebooksCreated) notebook(s)"
                + (outcome.notesSkipped > 0 ? ", \(outcome.notesSkipped) skipped" : "")
        } catch {
            jexStatus = "\(error)"
        }
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
