//! Engine configuration (SPEC_MVP §2.3).
//!
//! [`Config`] carries the numeric knobs the user can change from the
//! preferences window. It is a plain value: the FFI layer stores one global
//! copy, hands it to every [`Session`](crate::session::Session), and each
//! session consults it when it decides key meanings or cuts a candidate page.
//! Defaults equal the compiled-in constants, so behavior without a
//! preferences change is unchanged.

use serde::{Deserialize, Serialize};

use crate::{MAX_CANDIDATES, MAX_CODE_LEN};

/// Largest page size the key model can honor: selection keys are digits
/// `1`-`9`, so a bigger page would hold candidates no key can reach.
pub const PAGE_SIZE_MAX: usize = 9;

/// Largest encoding cap worth accepting; longer than any shipped scheme.
pub const MAX_CODE_LEN_LIMIT: usize = 16;

/// The two numeric settings exposed by the preferences window.
///
/// Deserialization is field-wise partial (missing fields keep their
/// defaults), which lets a shell update one knob without naming the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Candidates one page holds and the shell puts behind digit keys.
    pub page_size: usize,
    /// Maximum letters in one encoding.
    pub max_code_len: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self { page_size: MAX_CANDIDATES, max_code_len: MAX_CODE_LEN }
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
    }

    #[test]
    fn sanitize_clamps_both_ends_into_range() {
        let low = Config { page_size: 0, max_code_len: 0 }.sanitized();
        assert_eq!(low.page_size, 1);
        assert_eq!(low.max_code_len, 1);

        let high = Config { page_size: 42, max_code_len: 999 }.sanitized();
        assert_eq!(high.page_size, PAGE_SIZE_MAX);
        assert_eq!(high.max_code_len, MAX_CODE_LEN_LIMIT);

        let ok = Config { page_size: 9, max_code_len: 16 };
        assert_eq!(ok.sanitized(), ok);
    }

    #[test]
    fn json_may_set_either_field_and_ignores_unknown_ones() {
        let config: Config = serde_json::from_str(r#"{"page_size":3,"ignored":1}"#).unwrap();
        assert_eq!(config.page_size, 3);
        assert_eq!(config.max_code_len, MAX_CODE_LEN, "missing field keeps its default");
    }
}
