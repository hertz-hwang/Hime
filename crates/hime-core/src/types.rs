//! Data types crossing the engine/shell boundary (SPEC §2.2).

use serde::{Deserialize, Serialize};

/// One selectable entry in the candidate window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    /// Word to commit, e.g. "工".
    pub text: String,
    /// Hint for the remaining encoding (e.g. `~d`); empty for exact matches.
    pub comment: String,
}

/// Everything a stateless shell needs to render after one keystroke.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineResponse {
    /// `false` means the key was not consumed; the shell must forward it to
    /// the application instead.
    pub handled: bool,
    /// Text to send to the application now, if any.
    pub commit_text: Option<String>,
    /// In-progress encoding shown as inline composition text.
    pub preedit: String,
    /// Candidate window contents (at most `MAX_CANDIDATES`).
    pub candidates: Vec<Candidate>,
}
