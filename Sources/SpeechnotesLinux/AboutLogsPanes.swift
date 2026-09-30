import Foundation
import SwiftCrossUI
import AppPaths
import Appearance

/// The app's first numbered version. Release tags (v*) are the source of
/// truth; this mirrors the current main-line state.
enum AppInfo {
    static let version = "1.0.0"
}

/// About pane: what the app is, the engine tier, license, and where data
/// lives — the desktop equivalent of the iOS AboutView's essentials.
struct AboutPane: View {
    @State private var theme = ThemeController.shared

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Speechnotes")
                        .font(.title.weight(.semibold))
                    Text("Version \(AppInfo.version)")
                        .font(.callout)
                        .foregroundColor(theme.text)
                }
                card {
                    Text("Offline notes and books with text-to-speech. Notes, notebooks, books and speech all stay on this machine — nothing is uploaded. The data layer is byte-compatible with the iOS app, so a JEX round-trip moves a library between the two.")
                        .font(.callout)
                }
                card {
                    VStack(alignment: .leading, spacing: 8) {
                        captionHeader("Speech engines")
                        Text("eSpeak NG, Pico, Piper, Kokoro (ONNX) and Supertonic (ONNX) — every engine runs locally. Kokoro and Supertonic models download once from within the app (Settings → Models).")
                            .font(.callout)
                    }
                }
                card {
                    VStack(alignment: .leading, spacing: 8) {
                        captionHeader("License")
                        Text("GPL-3.0 — the same position dsnote takes, since espeak-ng and piper link into the tier. Kokoro/misaki models are Apache-2.0; the Supertonic model is OpenRAIL-M.")
                            .font(.callout)
                    }
                }
                card {
                    VStack(alignment: .leading, spacing: 8) {
                        captionHeader("Data location")
                        Text(AppPaths.dataDir.path)
                            .font(.system(size: 12, design: .monospaced))
                        Text("Logs: \(AppPaths.logFile.path)")
                            .font(.system(size: 12, design: .monospaced))
                            .foregroundColor(theme.text)
                    }
                }
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func card(@ViewBuilder content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            content()
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(cardTint)
        .cornerRadius(8)
    }

    private var cardTint: Color {
        switch theme.effectiveScheme {
        case .light: return Color(white: 0.0, opacity: 0.035)
        case .dark: return Color(white: 1.0, opacity: 0.045)
        }
    }

    private func captionHeader(_ title: String) -> some View {
        Text(title.uppercased())
            .font(.caption.weight(.medium))
            .foregroundColor(theme.text)
    }
}

/// In-app log tail — the desktop LogsView. Reads the last chunk of the
/// app's own log file; refresh re-reads.
struct LogsPane: View {
    @State private var lines: [String] = []
    @State private var loadFailed = false
    @State private var theme = ThemeController.shared

    var body: some View {
        VStack(spacing: 8) {
            HStack(spacing: 8) {
                Text("Logs")
                    .font(.title3.weight(.semibold))
                Text("last \(lines.count) lines")
                    .font(.footnote)
                    .foregroundColor(theme.text)
                Spacer()
                Button("Refresh") { load() }
                    .buttonStyle(.bordered)
            }
            if loadFailed {
                ContentUnavailableView {
                    Text("No log yet")
                } description: {
                    Text("The log file appears once the app has something to say.")
                }
            } else {
                ScrollView {
                    Text(lines.joined(separator: "\n"))
                        .font(.system(size: 11, design: .monospaced))
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .padding(12)
        .task { load() }
    }

    private func load() {
        guard let tail = try? String(contentsOf: AppPaths.logFile, encoding: .utf8) else {
            lines = []
            loadFailed = true
            return
        }
        loadFailed = false
        lines = tail.split(separator: "\n").suffix(300).map(String.init)
    }
}
