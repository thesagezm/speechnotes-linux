import Foundation
import SpeechLogic

/// One note. Field names and semantics match the iOS app's `Note` exactly so
/// both apps can read each other's `notes.json`.
public struct Note: Identifiable, Codable, Equatable, Sendable {
    public var id: UUID = UUID()
    /// User-set title. nil (or blank) → title derives from the first sentence
    /// of `text`.
    public var explicitTitle: String?
    public var text: String = ""
    public var createdAt: Date = Date()
    public var updatedAt: Date = Date()
    /// Recycle bin: nil = active; set = moment binned (30-day retention,
    /// iOS parity). nil for every note decoded from pre-v1.3 JSON.
    public var deletedAt: Date?
    /// Notebook this note is filed into (nil = Unfiled, pre-v1.4 state).
    public var notebookId: UUID?
    public var isPinned: Bool = false
    public var isFavorite: Bool = false

    public static let recycleRetentionDays = 30

    enum CodingKeys: String, CodingKey {
        case id, explicitTitle, text, createdAt, updatedAt, deletedAt
        case notebookId, isPinned, isFavorite
    }

    public init() {}

    /// Decodes notes.json written by older versions (no explicitTitle /
    /// deletedAt / notebook keys) and tolerates missing fields entirely.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(UUID.self, forKey: .id) ?? UUID()
        explicitTitle = try c.decodeIfPresent(String.self, forKey: .explicitTitle)
        text = try c.decodeIfPresent(String.self, forKey: .text) ?? ""
        createdAt = try c.decodeIfPresent(Date.self, forKey: .createdAt) ?? Date()
        updatedAt = try c.decodeIfPresent(Date.self, forKey: .updatedAt) ?? Date()
        deletedAt = try c.decodeIfPresent(Date.self, forKey: .deletedAt)
        notebookId = try c.decodeIfPresent(UUID.self, forKey: .notebookId)
        isPinned = try c.decodeIfPresent(Bool.self, forKey: .isPinned) ?? false
        isFavorite = try c.decodeIfPresent(Bool.self, forKey: .isFavorite) ?? false
    }

    public var title: String {
        if let explicit = explicitTitle?
            .trimmingCharacters(in: .whitespacesAndNewlines), !explicit.isEmpty {
            return String(explicit.prefix(120))
        }
        // First SENTENCE of the note (not just the first line). Windowed so
        // list rendering stays cheap.
        let window = String(text.prefix(300))
        let firstSentence: String
        if let piece = SentenceChunker.sentencePieces(in: window).first {
            let units = Array(window.utf16)
            firstSentence = String(decoding: units[piece.offset..<min(piece.endOffset, units.count)], as: UTF16.self)
        } else {
            firstSentence = window
        }
        let trimmed = firstSentence.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "Untitled note" : String(trimmed.prefix(60))
    }

    /// Days left before this binned note is auto-purged (nil while active).
    public var recycleDaysRemaining: Int? {
        guard let deletedAt else { return nil }
        let days = Calendar.current.dateComponents([.day], from: deletedAt, to: Date()).day ?? 0
        return max(0, Note.recycleRetentionDays - days)
    }

    /// Whitespace-separated word count of the body (iOS VoiceCatalog parity).
    public var wordCount: Int {
        text.split(whereSeparator: \.isWhitespace).count
    }

    /// Rough listening-time estimate at a spoken pace of ~145 words/minute,
    /// floored at one minute for anything non-empty.
    public var estimatedListenMinutes: Int? {
        guard wordCount > 0 else { return nil }
        return max(1, Int((Double(wordCount) / 145).rounded()))
    }
}
