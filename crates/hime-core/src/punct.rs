//! ASCII → 中文标点 mapping for the `ascii_punct = 1` mode (SPEC_MVP §3).
//!
//! Every mapping is a fixed string, so a punctuation key needs no retrieval.
//! Quote pairing is the only state: `'` and `"` alternate between their
//! opening and closing glyph, and the owning [`Session`](crate::Session)
//! keeps the [`QuotePair`]. Lookups stay pure functions of key plus that
//! state, so the decision table remains testable without one.

/// Which side of each quote pair the next press types; owned by the Session
/// and kept across compositions (a composition cleared mid-quote does not
/// flip a quote back to its opening glyph).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct QuotePair {
    single_open: bool,
    double_open: bool,
}

impl QuotePair {
    fn next_single(&mut self) -> &'static str {
        let text = if self.single_open { "’" } else { "‘" };
        self.single_open = !self.single_open;
        text
    }

    fn next_double(&mut self) -> &'static str {
        let text = if self.double_open { "”" } else { "“" };
        self.double_open = !self.double_open;
        text
    }
}

/// The single-shot mappings; quotes pair and live in [`QuotePair`].
const DIRECT: &[(u8, &str)] = &[
    (b'~', "~"),
    (b'`', "`"),
    (b',', "，"),
    (b'.', "。"),
    (b';', "；"),
    (b':', "："),
    (b'?', "？"),
    (b'!', "！"),
    (b'$', "￥"),
    (b'^', "……"),
    (b'(', "（"),
    (b')', "）"),
    (b'[', "【"),
    (b']', "】"),
    (b'{', "「"),
    (b'}', "」"),
    (b'<', "《"),
    (b'>', "》"),
    (b'/', "/"),
    (b'\\', "、"),
    (b'-', "——"),
    (b'~', "……"),
    (b'`', "·"),
];

/// The full-width text one ASCII punctuation key produces in Chinese mode.
/// `'` and `"` advance the caller's quote pairing. `None` for a key the
/// mapping does not cover — such keys pass through to the host.
#[must_use]
pub fn map(ascii: u8, quotes: &mut QuotePair) -> Option<&'static str> {
    match ascii {
        b'\'' => Some(quotes.next_single()),
        b'"' => Some(quotes.next_double()),
        _ => DIRECT
            .iter()
            .find(|(key, _)| *key == ascii)
            .map(|(_, text)| *text),
    }
}

/// Whether [`map`] covers this key (the state-free question the
/// [`Processor`](crate::Processor) asks before asking for text).
#[must_use]
pub fn is_mapped(ascii: u8) -> bool {
    matches!(ascii, b'\'' | b'"') || DIRECT.iter().any(|(key, _)| *key == ascii)
}

/// Printable ASCII punctuation and symbol keys (nothing letters or digits
/// use): the class the engine may release to the host instead of swallowing,
/// in idle or mid-composition.
#[must_use]
pub fn is_symbol(ascii: u8) -> bool {
    matches!(ascii, 33..=47 | 58..=64 | 91..=96 | 123..=126)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published table: every key maps to its documented full-width text.
    #[test]
    fn maps_the_documented_table() {
        let mut quotes = QuotePair::default();
        let expected: &[(u8, &str)] = &[
            (b',', "，"),
            (b'.', "。"),
            (b';', "；"),
            (b':', "："),
            (b'?', "？"),
            (b'!', "！"),
            (b'(', "（"),
            (b')', "）"),
            (b'[', "【"),
            (b']', "】"),
            (b'<', "《"),
            (b'>', "》"),
            (b'/', "、"),
            (b'-', "——"),
            (b'~', "……"),
            (b'`', "·"),
        ];
        for (key, text) in expected {
            assert_eq!(map(*key, &mut quotes), Some(*text), "key {key:?}");
            assert!(is_mapped(*key));
        }
    }

    /// Quotes alternate open/close, each kind on its own counter.
    #[test]
    fn quotes_alternate_per_kind() {
        let mut quotes = QuotePair::default();
        assert_eq!(map(b'\'', &mut quotes), Some("‘"));
        assert_eq!(map(b'\'', &mut quotes), Some("’"));
        assert_eq!(map(b'\'', &mut quotes), Some("‘"));

        assert_eq!(map(b'"', &mut quotes), Some("“"));
        assert_eq!(map(b'"', &mut quotes), Some("”"));

        // Interleaving the two kinds does not disturb either sequence.
        assert_eq!(map(b'\'', &mut quotes), Some("’"));
        assert_eq!(map(b'"', &mut quotes), Some("“"));
    }

    #[test]
    fn unmapped_keys_have_no_text() {
        let mut quotes = QuotePair::default();
        for key in *b"01a%@#$*\\|{}" {
            assert_eq!(map(key, &mut quotes), None, "key {key:?}");
            assert!(!is_mapped(key), "key {key:?} must pass through");
        }
    }

    /// The symbol class covers every printable punctuation/symbol key and
    /// excludes letters and digits, whose keys keep their own meanings.
    #[test]
    fn symbol_class_covers_punctuation_only() {
        for key in b'!'..=b'~' {
            let letter_or_digit = key.is_ascii_alphanumeric();
            assert_eq!(is_symbol(key), !letter_or_digit, "key {key:?}");
        }
        assert!(!is_symbol(b' '));
    }
}
