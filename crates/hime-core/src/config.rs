//! Engine configuration (SPEC §2.3).
//!
//! [`Config`] carries the knobs the user can change from the preferences
//! window; a selected scheme supplies its defaults from `metadata.toml`
//! (see [`crate::scheme`]). It is a plain value: the FFI layer stores one
//! global copy, hands it to every [`Session`](crate::session::Session), and
//! each session consults it when it decides key meanings or cuts a candidate
//! page. Defaults equal the compiled-in constants, so behavior without a
//! preferences change is unchanged.

use serde::{Deserialize, Serialize};

use crate::{MAX_CANDIDATES, MAX_CODE_LEN};

/// Largest page size the key model can honor: selection keys are digits
/// `1`-`9`, so a bigger page would hold candidates no key can reach.
pub const PAGE_SIZE_MAX: usize = 9;

/// Largest encoding cap worth accepting. A 形码 scheme needs 4, but a 全拼
/// scheme's `max_code` is the length of one whole pinyin run, which the
/// shipped table declares as 32 (the pinyin code cap, SPEC §8.2).
pub const MAX_CODE_LEN_LIMIT: usize = 32;

/// How encodings are read, from the `type` key of a scheme's `metadata.toml`
/// (0 全拼 / 1 双拼 / 2 形码 / 3 形码整句). Crosses the FFI boundary as that
/// same number; unknown numbers clamp to the largest variant at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "u8", into = "u8")]
pub enum CodeType {
    /// 全拼: full pinyin syllables.
    Quanpin,
    /// 双拼: one key per initial and final.
    Shuangpin,
    /// 形码: shape (grapheme) codes; the retrieval source is a table.
    Xingma,
    /// 形码整句: shape codes covering whole sentences.
    XingmaSentence,
}

impl CodeType {
    /// The `metadata.toml` / `Config` number for this type.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Quanpin => 0,
            Self::Shuangpin => 1,
            Self::Xingma => 2,
            Self::XingmaSentence => 3,
        }
    }

    /// Number to type; values above the last known type clamp upward.
    #[must_use]
    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Quanpin,
            1 => Self::Shuangpin,
            2 => Self::Xingma,
            _ => Self::XingmaSentence,
        }
    }
}

impl From<u8> for CodeType {
    fn from(code: u8) -> Self {
        Self::from_code(code)
    }
}

impl From<CodeType> for u8 {
    fn from(ty: CodeType) -> u8 {
        ty.code()
    }
}

/// The settings exposed by the preferences window; a scheme's
/// `metadata.toml` supplies their defaults (see [`crate::scheme`]).
///
/// Deserialization is field-wise partial (missing fields keep their
/// defaults), which lets a shell update one knob without naming the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Candidates one page holds and the shell puts behind digit keys.
    pub page_size: usize,
    /// Maximum letters in one encoding.
    pub max_code_len: usize,
    /// How encodings are read; selects the candidate sources a future
    /// pipeline wires up (the engine retrieves from the table either way).
    pub code_type: CodeType,
    /// Commit automatically once [`Self::auto_commit_length`] letters are
    /// typed and the candidate list holds exactly one entry.
    pub auto_commit: bool,
    /// Code length [`Self::auto_commit`] needs; inert while auto-commit is
    /// off.
    pub auto_commit_length: usize,
    /// The N-th letter commits the candidate of the first N-1 letters and
    /// starts a fresh encoding with itself; `0` turns the rule off.
    pub commit_next_code: usize,
    /// Punctuation output, the `metadata.toml` `ascii_punct` key: `0` English
    /// (ASCII) punctuation, `1` Chinese punctuation. A number, never a bool.
    pub ascii_punct: u8,
    /// Release the first punctuation key after English letters or digits to
    /// the host as ASCII instead of mapping it to 中文标点 (the `12:30`
    /// case). Schemes do not define it; it survives a scheme switch.
    pub english_punct_after_alnum: bool,
    /// While a candidate is on screen, `;` selects the 2nd candidate and
    /// `'` the 3rd, clamped to the last candidate of the rendered page; the
    /// 2nd/3rd-select rule. Schemes do not define it; it survives a scheme
    /// switch.
    pub semicolon_quote_select: bool,
    /// Show the encoding in the candidate window (the non-embedded preedit
    /// mode) instead of inline in the input field. Schemes do not define it;
    /// it survives a scheme switch.
    pub show_code_in_window: bool,
    /// Candidate window orientation: `false` lays candidates in one
    /// horizontal row, `true` stacks them in a vertical column. The key model
    /// follows it: horizontal candidates move focus with Left/Right, vertical
    /// ones with Up/Down. Schemes do not define it; it survives a scheme
    /// switch.
    pub vertical_candidates: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            page_size: MAX_CANDIDATES,
            max_code_len: MAX_CODE_LEN,
            code_type: CodeType::Xingma,
            auto_commit: false,
            auto_commit_length: MAX_CODE_LEN,
            commit_next_code: 0,
            ascii_punct: 0,
            english_punct_after_alnum: false,
            semicolon_quote_select: false,
            show_code_in_window: false,
            vertical_candidates: false,
        }
    }
}

impl Config {
    /// Clamp into the range the key model can honor. Out-of-range requests
    /// are clamped rather than rejected, so a bad stored value degrades
    /// instead of breaking typing.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.page_size = self.page_size.clamp(1, PAGE_SIZE_MAX);
        self.max_code_len = self.max_code_len.clamp(1, MAX_CODE_LEN_LIMIT);
        self.auto_commit_length = self.auto_commit_length.clamp(1, MAX_CODE_LEN_LIMIT);
        // The rule needs a first code to keep, so 1 is meaningless and 0
        // means "off"; both end up off.
        self.commit_next_code =
            if self.commit_next_code < 2 { 0 } else { self.commit_next_code.min(MAX_CODE_LEN_LIMIT) };
        // The punctuation flag is binary: other numbers clamp to the top.
        self.ascii_punct = self.ascii_punct.min(1);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_compiled_in_constants() {
        let config = Config::default();
        assert_eq!(config.page_size, MAX_CANDIDATES);
        assert_eq!(config.max_code_len, MAX_CODE_LEN);
        assert_eq!(config.code_type, CodeType::Xingma, "the engine retrieves a 形码 table");
        assert!(!config.auto_commit);
        assert_eq!(config.auto_commit_length, MAX_CODE_LEN);
        assert_eq!(config.commit_next_code, 0, "overflow commit is off by default");
        assert_eq!(config.ascii_punct, 0, "English punctuation is the default");
        assert!(!config.english_punct_after_alnum, "the rule is opt-in");
        assert!(!config.semicolon_quote_select, "; and ' stay punctuation by default");
        assert!(!config.show_code_in_window, "the encoding is inline by default");
        assert!(!config.vertical_candidates, "candidates are horizontal by default");
    }

    /// The punctuation switches, preedit placement and candidate layout cross
    /// JSON as plain bools; a JSON that names none of them keeps them off.
    #[test]
    fn preference_switches_cross_json_as_bools() {
        let config: Config = serde_json::from_str(
            r#"{"english_punct_after_alnum":true,"semicolon_quote_select":true,"show_code_in_window":true,"vertical_candidates":true}"#,
        )
        .unwrap();
        assert!(config.english_punct_after_alnum);
        assert!(config.semicolon_quote_select);
        assert!(config.show_code_in_window);
        assert!(config.vertical_candidates);

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""english_punct_after_alnum":true"#), "{json}");
        assert!(json.contains(r#""semicolon_quote_select":true"#), "{json}");
        assert!(json.contains(r#""show_code_in_window":true"#), "{json}");
        assert!(json.contains(r#""vertical_candidates":true"#), "{json}");

        let off: Config = serde_json::from_str("{}").unwrap();
        assert!(!off.english_punct_after_alnum);
        assert!(!off.semicolon_quote_select);
        assert!(!off.show_code_in_window);
        assert!(!off.vertical_candidates);
    }

    #[test]
    fn sanitize_clamps_both_ends_into_range() {
        let low = Config { page_size: 0, max_code_len: 0, ..Config::default() }.sanitized();
        assert_eq!(low.page_size, 1);
        assert_eq!(low.max_code_len, 1);

        let high = Config { page_size: 42, max_code_len: 999, ..Config::default() }.sanitized();
        assert_eq!(high.page_size, PAGE_SIZE_MAX);
        assert_eq!(high.max_code_len, MAX_CODE_LEN_LIMIT);

        let ok = Config { page_size: 9, max_code_len: 16, ..Config::default() };
        assert_eq!(ok.sanitized(), ok);
    }

    #[test]
    fn sanitize_normalizes_the_scheme_behavior_keys() {
        let config = Config {
            auto_commit_length: 0,
            commit_next_code: 1,
            ..Config::default()
        }
        .sanitized();
        assert_eq!(config.auto_commit_length, 1);
        assert_eq!(config.commit_next_code, 0, "1 is not a usable overflow position");

        let config = Config {
            auto_commit_length: 99,
            commit_next_code: 99,
            ..Config::default()
        }
        .sanitized();
        assert_eq!(config.auto_commit_length, MAX_CODE_LEN_LIMIT);
        assert_eq!(config.commit_next_code, MAX_CODE_LEN_LIMIT);

        let config = Config { commit_next_code: 5, ..Config::default() }.sanitized();
        assert_eq!(config.commit_next_code, 5, "the 5th code stays usable");
    }

    /// `ascii_punct` crosses JSON as the metadata.toml number (0 英文标点 /
    /// 1 中文标点), never as a bool; other numbers clamp to the top.
    #[test]
    fn ascii_punct_crosses_json_as_its_number() {
        let config: Config = serde_json::from_str(r#"{"ascii_punct":1}"#).unwrap();
        assert_eq!(config.ascii_punct, 1);
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""ascii_punct":1"#), "{json}");

        let clamped = Config { ascii_punct: 7, ..Config::default() }.sanitized();
        assert_eq!(clamped.ascii_punct, 1, "unknown numbers clamp to 中文标点");
    }

    #[test]
    fn json_may_set_either_field_and_ignores_unknown_ones() {
        let config: Config = serde_json::from_str(r#"{"page_size":3,"ignored":1}"#).unwrap();
        assert_eq!(config.page_size, 3);
        assert_eq!(config.max_code_len, MAX_CODE_LEN, "missing field keeps its default");
    }

    /// The scheme metadata and the preferences panel exchange the code type
    /// as the metadata.toml number (0 全拼 / 1 双拼 / 2 形码 / 3 形码整句).
    #[test]
    fn code_type_crosses_json_as_its_number() {
        let config: Config =
            serde_json::from_str(r#"{"code_type":0,"auto_commit":true,"commit_next_code":5}"#)
                .unwrap();
        assert_eq!(config.code_type, CodeType::Quanpin);
        assert!(config.auto_commit);
        assert_eq!(config.commit_next_code, 5);

        let json = serde_json::to_string(&Config::default()).unwrap();
        assert!(json.contains(r#""code_type":2"#), "{json}");

        let clamped: Config = serde_json::from_str(r#"{"code_type":9}"#).unwrap();
        assert_eq!(clamped.code_type, CodeType::XingmaSentence, "unknown numbers clamp up");

        for (number, ty) in [
            (0u8, CodeType::Quanpin),
            (1, CodeType::Shuangpin),
            (2, CodeType::Xingma),
            (3, CodeType::XingmaSentence),
        ] {
            assert_eq!(CodeType::from_code(number), ty);
        }
    }
}
