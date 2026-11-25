// JSON mirror of hime-core's EngineResponse (SPEC §2.2).
//
// The Rust session owns all typing state; this Swift layer only decodes the
// per-keystroke response and renders it. Keep these fields in step with
// crates/hime-core/src/types.rs.

import Foundation

/// How one run of candidate text differs from the first candidate.
enum EngineDiffKind: String, Decodable {
    case equal
    case modified
    case deleted
    case inserted
}

/// One colored run of candidate text.
struct EngineDiffSpan: Decodable {
    let text: String
    let kind: EngineDiffKind
}

/// One selectable entry in the candidate window.
struct EngineCandidate: Decodable {
    /// Text committed when this candidate is selected.
    let text: String
    /// Short annotation the engine attaches to this entry: the remaining
    /// coding (`~d`) for a table prefix match, the active scheme's complete
    /// code for a 反查 result. Empty means nothing to draw.
    let comment: String
    /// Word-separated code split behind a whole-sentence or pinyin candidate.
    let code: String
    /// Diff runs relative to the first candidate; empty = one normal run.
    let spans: [EngineDiffSpan]

    /// The annotation drawn after the text. The shell only renders it; what it
    /// says is entirely the engine's decision (AGENTS.md §1).
    var panelNote: String { comment }
}

/// Everything the shell needs to render after one keystroke.
struct EngineResponse: Decodable {
    let handled: Bool
    let commitText: String?
    /// Raw encoding as typed.
    let preedit: String
    /// Display line: the top candidate's word-separated split when it covers
    /// the raw encoding, otherwise `preedit`.
    let codeLine: String
    /// Render `codeLine` in the candidate window instead of inline.
    let codeInWindow: Bool
    let candidates: [EngineCandidate]
    /// Zero-based focused candidate in `candidates`; Space commits it.
    let focus: Int
    /// One-based page number; 0 when no candidate stands.
    let page: Int
    /// Pages the retrieved window spans; 0 when no candidate stands.
    let pageCount: Int

    enum CodingKeys: String, CodingKey {
        case handled
        case commitText = "commit_text"
        case preedit
        case codeLine = "code_line"
        case codeInWindow = "code_in_window"
        case candidates
        case focus
        case page
        case pageCount = "page_count"
    }
}
