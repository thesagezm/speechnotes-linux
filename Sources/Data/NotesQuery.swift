import Foundation
import Data

/// The notes list's scope → filter → sort pipeline, as a type the store
/// owns and the view merely calls.
///
/// It lives in `Data` rather than in the pane because the pane cannot be
/// exercised headlessly (no display server), and because this is the one
/// piece of list logic with real behaviour: scope filtering, an
/// accent-insensitive query, pinned-first ordering, and three sort keys. All
/// of it is covered by NotesQueryTests.
public struct NotesQuery {
    public enum Sort: String, CaseIterable, Equatable, Sendable {
        case edited
        case created
        case title
    }

    /// Pinned notes always come first, whatever the sort key — the iOS list
    /// behaves the same way, and it is the one ordering rule a user expects
    /// not to have to re-learn.
    public static func apply(
        _ notes: [Note],
        notebookId: UUID?,
        allNotebooks: Bool,
        query: String,
        sort: Sort
    ) -> [Note] {
        var result = allNotebooks ? notes : notes.filter { $0.notebookId == notebookId }
        if !query.isEmpty {
            let needle = folded(query)
            result = result.filter { folded($0.text).contains(needle) }
        }
        result.sort { lhs, rhs in
            if lhs.isPinned != rhs.isPinned { return lhs.isPinned }
            switch sort {
            case .edited: return lhs.updatedAt > rhs.updatedAt
            case .created: return lhs.createdAt > rhs.createdAt
            // Plain < on folded titles: the fold makes it case- and
            // accent-insensitive, and unlike localizedCompare it is a
            // total order, so the sort is stable for equal keys.
            case .title: return folded(lhs.title) < folded(rhs.title)
            }
        }
        return result
    }

    /// The scope string's meaning: which notebook to show, and whether that
    /// means "all of them". `"all"` is every notebook, `"unfiled"` is the
    /// nil notebook, anything else is a notebook UUID.
    ///
    /// Returns nil for a scope that is none of those — a stale preference
    /// naming a notebook that no longer exists, say. Callers must not treat
    /// that as "unfiled": showing the wrong notes silently is worse than
    /// showing an empty list, and the caller can fall back deliberately.
    public static func notebookId(forScope scope: String) -> (id: UUID?, all: Bool)? {
        if scope == "all" { return (nil, true) }
        if scope == "unfiled" { return (nil, false) }
        return UUID(uuidString: scope).map { ($0, false) }
    }

    /// Case-, accent- and width-insensitive, matching what a user expects
    /// from a search box: "cafe" finds "café", "STRASSE" finds "Straße".
    private static func folded(_ value: String) -> String {
        value.folding(
            options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive],
            locale: nil
        )
    }
}
