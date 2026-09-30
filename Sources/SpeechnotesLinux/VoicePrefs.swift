import Foundation
import Data
import TTSEngine

/// Per-engine voice selection over Prefs' raw dictionary. The legacy
/// `voice` field stays the espeak selection (iOS vocabulary compatibility);
/// every engine's choice lives in `voicesByEngine` with the engine kind as
/// the key.
extension Prefs {
    public func voiceForEngine(_ kind: EngineKind) -> String {
        if kind == .espeak {
            return voicesByEngine[EngineKind.espeak.rawValue] ?? voice
        }
        return voicesByEngine[kind.rawValue] ?? ""
    }

    public func setVoice(_ id: String, for kind: EngineKind) {
        voicesByEngine[kind.rawValue] = id
    }
}
