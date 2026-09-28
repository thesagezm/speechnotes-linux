import CEspeakNG
import Foundation

/// espeak-ng voice metadata, as reported by `espeak_ListVoices`.
public struct EspeakVoiceInfo: Equatable {
    public let name: String
    public let identifier: String
    public let languages: [String]
    public let gender: String

    init(voice: espeak_VOICE) {
        name = voice.name.map { String(cString: $0) } ?? ""
        identifier = voice.identifier.map { String(cString: $0) } ?? ""
        // The languages field is a repeated (priority byte, NUL-terminated
        // name) sequence terminated by a lone zero byte.
        var langs: [String] = []
        if let base = voice.languages {
            var p = base
            while true {
                let priority = p.pointee
                if priority == 0 { break }
                p = p.advanced(by: 1)  // skip the priority byte
                var bytes: [UInt8] = []
                while true {
                    let byte = p.pointee
                    if byte == 0 { break }
                    bytes.append(UInt8(bitPattern: byte))
                    p = p.advanced(by: 1)
                }
                if !bytes.isEmpty {
                    langs.append(String(decoding: bytes, as: UTF8.self))
                }
                p = p.advanced(by: 1)  // skip the name's NUL
            }
        }
        languages = langs
        switch voice.gender {
        case 1: gender = "male"
        case 2: gender = "female"
        default: gender = "none"
        }
    }
}

/// Errors from the espeak-ng bridge.
public enum EspeakError: Error, Equatable {
    case initializationFailed(Int32)
    case synthesisFailed(Int32)

    public var message: String {
        switch self {
        case .initializationFailed(let code):
            return "espeak_Initialize failed (\(code))"
        case .synthesisFailed(let code):
            return "espeak_Synth failed (\(code))"
        }
    }
}

/// Thin Swift bridge over libespeak-ng, used the way dsnote uses it:
/// `AUDIO_OUTPUT_RETRIEVAL` so the synth callback hands us PCM buffers, one
/// `espeak_Synth` call per text chunk, `espeak_Synchronize` to drain.
///
/// State for the in-flight synthesis rides in `event->user_data`, which
/// espeak passes back on every callback — no globals needed.
public final class EspeakBridge {

    /// espeak-ng's fixed output rate, learned at initialization.
    public private(set) var sampleRate: Int = 0

    public init() {}

    /// Loads espeak-ng and prepares it for retrieval-mode synthesis.
    public func initialize() throws {
        // buflength 0 → espeak's default 60 ms buffers.
        let rate = espeak_Initialize(AUDIO_OUTPUT_RETRIEVAL, 0, nil, 0)
        if rate == -1 { throw EspeakError.initializationFailed(rate) }
        sampleRate = Int(rate)
    }

    /// The library's voices, for the picker.
    public func listVoices() -> [EspeakVoiceInfo] {
        guard let array = espeak_ListVoices(nil) else { return [] }
        var voices: [EspeakVoiceInfo] = []
        var i = 0
        while let v = array[i] {
            voices.append(EspeakVoiceInfo(voice: v.pointee))
            i += 1
        }
        return voices
    }

    /// Selects a voice by its espeak name (e.g. "english", "en-us").
    /// Last voice this bridge applied (used by phonemes() to guarantee the
    /// library has a voice selected — TextToPhonemes requires it).
    private var appliedVoice: String?

    @discardableResult
    public func setVoice(_ name: String) -> Bool {
        let ok = espeak_SetVoiceByName(name) == EE_OK
        if ok { appliedVoice = name }
        return ok
    }

    /// Speaking rate in words per minute (80…450).
    public func setRate(_ wpm: Int) {
        espeak_SetParameter(espeakRATE, Int32(max(80, min(450, wpm))), 0)
    }

    public var currentRate: Int {
        Int(espeak_GetParameter(espeakRATE, 0))
    }

    /// Synthesizes one text chunk and returns its mono 16-bit PCM at
    /// `sampleRate`. `abort` lets a caller stop mid-synthesis (the callback
    /// returns 1, mirroring dsnote's shutdown check).
    public func synthesize(_ text: String, abort: @escaping () -> Bool = { false }) throws -> [Int16] {
        let session = Session(abort: abort)
        defer { session.dispose() }

        espeak_SetSynthCallback(espeakSynthCallback)

        let result = text.withCString { cstr in
            espeak_Synth(
                UnsafeMutablePointer(mutating: cstr),
                text.utf8.count + 1,
                0,
                POS_CHARACTER,
                0,
                UInt32(espeakCHARS_AUTO),
                nil,
                session.userData
            )
        }
        // Retrieval mode runs the synthesis inline; Synchronize just drains.
        espeak_Synchronize()

        guard result.rawValue == EE_OK.rawValue else {
            throw EspeakError.synthesisFailed(result.rawValue)
        }
        return session.samples
    }

    /// IPA phonemes for one text, clause by clause — the phonemization
    /// front end for Piper/Kokoro. espeak advances the pointer past each
    /// translated clause and returns its phonemes; NULL means end of text.
    /// The library requires BOTH an initialization and a selected voice
    /// before the first TextToPhonemes call (the latter segfaults without
    /// it), so this self-initializes and falls back to en-us when the
    /// caller hasn't selected one.
    public func phonemes(for text: String) -> [String] {
        if sampleRate <= 0 {
            try? initialize()
        }
        if appliedVoice == nil {
            _ = setVoice("en-us")  // any voice; the caller overrides before speaking
        }
        var clauses: [String] = []
        text.withCString { base in
            var cursor: UnsafeRawPointer? = UnsafeRawPointer(base)
            while let current = cursor {
                // espeak returns one clause's phonemes at a time and
                // advances `cursor`, setting it NULL at end of text.
                guard let ph = espeak_TextToPhonemes(&cursor, Int32(espeakCHARS_AUTO), 0x02) else {
                    break
                }
                let clause = String(cString: ph).trimmingCharacters(in: .whitespaces)
                if clause.isEmpty { break }
                clauses.append(clause)
                if cursor == current { break }  // no progress — stop
                if cursor == nil { break }      // end of text (documented)
            }
        }
        return clauses
    }
}

/// Per-synthesis state, passed to espeak as the callback's `user_data`.
private final class Session {
    var samples: [Int16] = []
    let abort: () -> Bool
    /// Retained reference held until `dispose()`; the C callback borrows it
    /// via `event->user_data`.
    private var box: Unmanaged<Session>?

    init(abort: @escaping () -> Bool) {
        self.abort = abort
        self.box = Unmanaged<Session>.passRetained(self)
    }

    var userData: UnsafeMutableRawPointer { box!.toOpaque() }

    func dispose() {
        box?.release()
        box = nil
    }
}

/// The C entry point espeak calls per audio buffer. `wav == nil` marks the
/// end of synthesis, matching dsnote's `synth_callback` structure.
private func espeakSynthCallback(
    _ wav: UnsafeMutablePointer<Int16>?,
    _ numsamples: Int32,
    _ event: UnsafeMutablePointer<espeak_EVENT>?
) -> Int32 {
    guard let event, let userData = event.pointee.user_data else { return 0 }
    let session = Unmanaged<Session>.fromOpaque(userData).takeUnretainedValue()

    guard let wav else { return 0 }  // end-of-synthesis marker

    if session.abort() { return 1 }

    let count = Int(numsamples)
    if count > 0 {
        let buffer = UnsafeBufferPointer(start: wav, count: count)
        session.samples.append(contentsOf: buffer)
    }
    return 0
}
