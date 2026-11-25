//! Data types crossing the engine/shell boundary (SPEC §2.2).

use serde::{Deserialize, Serialize};

/// How one run of candidate text differs from the first candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    /// Same characters as the first candidate.
    Equal,
    /// Same-length replacement.
    Modified,
    /// The candidate is shorter over this change block.
    Deleted,
    /// The candidate is longer over this change block.
    Inserted,
}

/// One run of candidate text with a single [`DiffKind`].
///
/// A shell colors each span by kind; [`DiffKind::Equal`] uses the normal text
/// color. Whole-sentence candidates carry these spans relative to the first
/// candidate; table candidates leave them empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSpan {
    /// Consecutive candidate characters with this kind.
    pub text: String,
    /// How the run differs from the first candidate.
    pub kind: DiffKind,
}

/// One selectable entry in the candidate window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    /// Word to commit, e.g. "工".
    pub text: String,
    /// Hint for the remaining encoding (e.g. `~d`); empty for exact matches.
    pub comment: String,
    /// Code split behind a whole-sentence candidate, e.g. `pms sdf gh ds`;
    /// empty for table matches and candidates without a segmentable code.
    #[serde(default)]
    pub code: String,
    /// Colored runs relative to the first candidate of the rendered page;
    /// empty means one uniform run (the base candidate or a table match).
    #[serde(default)]
    pub spans: Vec<DiffSpan>,
}

impl Candidate {
    /// A table/lexicon entry: text plus its remaining-code hint, without a
    /// whole-sentence code split or diff spans.
    #[must_use]
    pub fn new(text: impl Into<String>, comment: impl Into<String>) -> Self {
        Self { text: text.into(), comment: comment.into(), code: String::new(), spans: Vec::new() }
    }
}

/// Everything a stateless shell needs to render after one keystroke.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineResponse {
    /// `false` means the key was not consumed; the shell must forward it to
    /// the application instead.
    pub handled: bool,
    /// Text to send to the application now, if any.
    pub commit_text: Option<String>,
    /// In-progress encoding as typed (the raw letters).
    pub preedit: String,
    /// Encoding to display: the first candidate's code split when it covers
    /// the raw encoding, otherwise `preedit` itself. Spaces mark word
    /// boundaries; a shell shows this in the input field or the candidate
    /// window depending on `code_in_window`.
    pub code_line: String,
    /// Render the encoding in the candidate window (`Config::show_code_in_window`)
    /// instead of inline in the input field.
    pub code_in_window: bool,
    /// Candidate window contents (at most `page_size`).
    pub candidates: Vec<Candidate>,
    /// Zero-based index of the focused candidate within `candidates`; 0 when
    /// the list is empty. Space commits this entry.
    pub focus: usize,
    /// One-based page number of `candidates` within the whole retrieved
    /// window; 0 when there is no candidate.
    pub page: usize,
    /// Pages the whole retrieved window spans; 0 when there is no candidate.
    pub page_count: usize,
}
