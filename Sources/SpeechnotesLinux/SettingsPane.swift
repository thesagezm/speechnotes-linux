import Foundation
import SwiftCrossUI
import Data
import SpeechLogic
import TTSEngine
import Appearance

/// Preferences, grouped like the iOS Settings tabs: Speech, Appearance,
/// Models, Data. Switches and menu pickers instead of cycling buttons; every
/// model row carries its own install state and status line.
struct SettingsPane: View {
    let prefs: Prefs
    let notes: NotesStore
    let notebooks: NotebooksStore

    @State private var theme = ThemeController.shared
    @Environment(\.chooseFile) private var chooseFile
    @Environment(\.chooseFileSaveDestination) private var chooseFileSaveDestination

    @State private var jexStatus: String?
    @State private var modelStatus: [String: String] = [:]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 4) {
                header("Speech")
                section {
                    row("Engine") {
                        Picker(
                            of: allEngineKinds.map(\.displayName),
                            selection: engineSelection
                        )
                        .pickerStyle(.menu)
                    }
                    row("Voice") {
                        Picker(
                            of: voiceOptions.map(\.displayName),
                            selection: voiceSelection
                        )
                        .pickerStyle(.menu)
                    }
                    sliderRow(
                        title: "Speech rate",
                        value: rateBinding,
                        range: 0.5...2.0,
                        display: rateLabel
                    )
                    Toggle("Read along while speaking", isOn: readAlongBinding)
                        .toggleStyle(.switch)
                }

                header("Appearance")
                section {
                    row("Theme") {
                        Picker(
                            of: ["System", "Light", "Dark"],
                            selection: themeSelection
                        )
                        .pickerStyle(.menu)
                    }
                    row("Accent") {
                        Picker(
                            of: AccentChoice.allCases.map(\.displayName),
                            selection: accentSelection
                        )
                        .pickerStyle(.menu)
                    }
                    sliderRow(
                        title: "Text size",
                        value: textScaleBinding,
                        range: 0.75...1.5,
                        display: "\(Int((prefs.readerTextScale * 100).rounded()))%"
                    )
                    Toggle("Render Markdown in the editor", isOn: renderMarkdownBinding)
                        .toggleStyle(.switch)
                }

                header("Models")
                section {
                    modelRow(
                        key: "piper",
                        title: "Piper voice",
                        subtitle: "\(PiperModelManager.curatedVoice), ~63 MB",
                        installed: !PiperModelManager.installedVoices().isEmpty
                    ) {
                        Task { await download("piper", "Piper") {
                            _ = try await PiperModelManager.download()
                        } }
                    }
                    modelRow(
                        key: "kokoro",
                        title: "Kokoro",
                        subtitle: "82M uint8 tier, ~190 MB",
                        installed: KokoroModelManager.modelFilesAreValid()
                    ) {
                        Task { await download("kokoro", "Kokoro") {
                            try await KokoroModelManager.download()
                        } }
                    }
                    modelRow(
                        key: "supertonic",
                        title: "Supertonic",
                        subtitle: "full set, ~260 MB",
                        installed: !SupertonicModelManager.installedVoices().isEmpty
                    ) {
                        Task { await download("supertonic", "Supertonic") {
                            try await SupertonicModelManager.download()
                        } }
                    }
                }

                header("Data")
                section {
                    row("Backups") {
                        HStack(spacing: 8) {
                            Button("Export library (JEX)…") {
                                Task { await exportLibrary() }
                            }
                            .buttonStyle(.bordered)
                            Button("Import JEX…") {
                                Task { await importJex() }
                            }
                            .buttonStyle(.bordered)
                        }
                    }
                    row("Save now") {
                        Button("Save everything now") {
                            notes.flushNow()
                            prefs.flushNow()
                            BookmarkStore.shared.persistNow()
                            jexStatus = "Saved."
                        }
                        .buttonStyle(.bordered)
                    }
                    if let status = jexStatus {
                        Text(status)
                            .font(.footnote)
                            .foregroundColor(theme.text)
                    }
                }

                Text(libraryStats)
                    .font(.footnote)
                    .foregroundColor(theme.text)
                    .padding(.top, 12)
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    // MARK: - Building blocks

    private func header(_ title: String) -> some View {
        Text(title)
            .font(.title3.weight(.semibold))
            .padding(.top, 14)
            .padding(.bottom, 4)
    }

    private func section(@ViewBuilder content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            content()
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(themeColor.card)
        .cornerRadius(8)
    }

    private func row(_ title: String, @ViewBuilder control: () -> some View) -> some View {
        HStack(spacing: 12) {
            Text(title)
            Spacer()
            control()
        }
    }

    private func sliderRow(
        title: String,
        value: Binding<Double>,
        range: ClosedRange<Double>,
        display: String
    ) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(title)
                Spacer()
                Text(display)
                    .foregroundColor(theme.text)
                    .font(.callout)
            }
            Slider(value: value, in: range)
        }
    }

    private func modelRow(
        key: String,
        title: String,
        subtitle: String,
        installed: Bool,
        onDownload: @escaping () -> Void
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                    Text(subtitle)
                        .font(.footnote)
                        .foregroundColor(theme.text)
                }
                Spacer()
                if installed {
                    Text("Installed ✓")
                        .foregroundColor(theme.text)
                } else {
                    Button("Download") {
                        modelStatus[key] = "downloading…"
                        onDownload()
                    }
                    .buttonStyle(.bordered)
                }
            }
            if let status = modelStatus[key] {
                Text(status)
                    .font(.footnote)
                    .foregroundColor(theme.text)
            }
        }
    }

    private var themeColor: SurfaceStyle { theme.surface }

    // MARK: - Bindings

    private var engineSelection: Binding<String?> {
        Binding(
            get: { EngineKind(rawValue: prefs.engineKind)?.displayName },
            set: { name in
                if let kind = allEngineKinds.first(where: { $0.displayName == name }) {
                    prefs.engineKind = kind.rawValue
                }
            }
        )
    }

    private var allEngineKinds: [EngineKind] {
        [.espeak, .piper, .pico, .kokoro, .supertonic]
    }

    private var currentKind: EngineKind {
        EngineKind(rawValue: prefs.engineKind) ?? .espeak
    }

    private var voiceOptions: [VoiceCatalog.Voice] {
        VoiceCatalog.voices(for: currentKind)
    }

    private var voiceSelection: Binding<String?> {
        Binding(
            get: {
                let id = prefs.voiceForEngine(currentKind)
                if id.isEmpty { return "(default)" }
                return voiceOptions.first(where: { $0.id == id })?.displayName ?? "(default)"
            },
            set: { name in
                if name == "(default)" || name == nil {
                    prefs.setVoice("", for: currentKind)
                } else if let id = voiceOptions.first(where: { $0.displayName == name })?.id {
                    prefs.setVoice(id, for: currentKind)
                }
            }
        )
    }

    private var themeSelection: Binding<String?> {
        Binding(
            get: {
                switch prefs.appearance {
                case "light": return "Light"
                case "dark": return "Dark"
                default: return "System"
                }
            },
            set: { name in
                switch name {
                case "Light": prefs.appearance = "light"
                case "Dark": prefs.appearance = "dark"
                default: prefs.appearance = "system"
                }
            }
        )
    }

    private var accentSelection: Binding<String?> {
        Binding(
            get: { AccentChoice(rawValue: prefs.accentChoice)?.displayName },
            set: { name in
                if let choice = AccentChoice.allCases.first(where: { $0.displayName == name }) {
                    prefs.accentChoice = choice.rawValue
                }
            }
        )
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

    private var textScaleBinding: Binding<Double> {
        Binding(
            get: { prefs.readerTextScale },
            set: { prefs.readerTextScale = min(1.5, max(0.75, $0)) }
        )
    }

    private var rateLabel: String {
        String(format: "%.2f×", prefs.rateMultiplier)
    }

    private var libraryStats: String {
        "\(notes.notes.count) note(s), \(notes.deletedNotes.count) in bin, "
            + "\(notebooks.notebooks.count) notebook(s)"
    }

    // MARK: - Actions

    private func download(_ key: String, _ label: String, _ body: @escaping () async throws -> Void) async {
        do {
            try await body()
            VoiceCatalog.invalidateCache()
            modelStatus[key] = "installed ✓"
        } catch {
            modelStatus[key] = "\(label) download failed: \(error)"
        }
    }

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
}
