//! ASCII → 中文标点 mapping for the `ascii_punct = 1` mode (SPEC §3).
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

/// Whether the key's 中文标点 text differs from its ASCII character, i.e.
/// whether correcting a released press would change what stands on screen.
/// `/`, `` ` `` and `~` map to themselves, so pressing them twice is two
/// characters rather than one corrected one.
#[must_use]
pub fn is_correctable(ascii: u8) -> bool {
    match ascii {
        b'\'' | b'"' => true,
        _ => DIRECT
            .iter()
            .find(|(key, _)| *key == ascii)
            .is_some_and(|(_, text)| *text != char::from(ascii).to_string()),
    }
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

    /// The published table (SPEC §3.1): every key and the text it emits.
    /// `/`, `~` and backtick stay halfwidth on purpose; the engine claims the
    /// key either way, so the mapping text is what reaches the host.
    const DOCUMENTED: &[(u8, &str)] = &[
        (b'!', "！"),
        (b'$', "￥"),
        (b'(', "（"),
        (b')', "）"),
        (b',', "，"),
        (b'.', "。"),
        (b'/', "/"),
        (b':', "："),
        (b';', "；"),
        (b'<', "《"),
        (b'>', "》"),
        (b'?', "？"),
        (b'[', "【"),
        (b'\\', "、"),
        (b']', "】"),
        (b'^', "……"),
        (b'`', "`"),
        (b'{', "「"),
        (b'}', "」"),
        (b'~', "~"),
    ];

    /// The symbols the table leaves out, so no punctuation is ever committed
    /// for them. `-` and `=` are the paging keys: `Processor` swallows them
    /// while composing and releases them only when idle (SPEC §3).
    const RELEASED: &[u8] = b"#%&*+-=@_|";

    /// Every documented key maps to its documented text.
    #[test]
    fn maps_the_documented_table() {
        let mut quotes = QuotePair::default();
        for (key, text) in DOCUMENTED {
            assert_eq!(map(*key, &mut quotes), Some(*text), "key {key:?}");
            assert!(is_mapped(*key));
        }
    }

    /// A key listed twice can never emit its second text: [`map`] keeps the
    /// first match, so the later row is dead weight that reads as a mapping.
    #[test]
    fn table_has_no_duplicate_keys() {
        let mut keys: Vec<u8> = DIRECT.iter().map(|(key, _)| *key).collect();
        keys.sort_unstable();
        let unique = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), unique, "DIRECT lists a punctuation key twice");
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

    /// The table's claim set is exactly the documented keys and the quotes:
    /// no other printable symbol may be claimed, or it would stop reaching
    /// the host (SPEC §3).
    #[test]
    fn unmapped_keys_have_no_text() {
        let claimed: Vec<u8> = (b'!'..=b'~')
            .filter(|key| is_symbol(*key))
            .filter(|key| !is_mapped(*key))
            .collect();
        assert_eq!(claimed.as_slice(), RELEASED);

        let mut quotes = QuotePair::default();
        for key in RELEASED.iter().chain(b"01a") {
            assert_eq!(map(*key, &mut quotes), None, "key {key:?}");
            assert!(!is_mapped(*key), "key {key:?} must pass through");
        }
    }

    /// The correction applies only where 中文标点 differs from the ASCII
    /// character. `/`, `` ` `` and `~` map to themselves, so a second press
    /// types a second character instead of rewriting the first.
    #[test]
    fn only_keys_with_a_distinct_mapping_are_correctable() {
        for key in DIRECT.iter().map(|(key, _)| *key).chain([b'\'', b'"']) {
            let differs = DIRECT
                .iter()
                .find(|(k, _)| *k == key)
                .is_none_or(|(_, text)| *text != char::from(key).to_string());
            assert_eq!(is_correctable(key), differs, "key {key:?}");
        }
        for key in [b'/', b'`', b'~'] {
            assert!(!is_correctable(key), "key {key:?} maps to itself");
        }
        for key in [b':', b',', b';', b'?', b'^', b'\'', b'"'] {
            assert!(is_correctable(key), "key {key:?} corrects to 中文标点");
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
