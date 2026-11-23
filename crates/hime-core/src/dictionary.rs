//! Dictionary loading and candidate retrieval (SPEC_MVP §4).
//!
//! The lexicon is a TSV file of `code<TAB>text<TAB>weight` rows, one entry
//! per line; blank lines are ignored. Codes are restricted to lowercase ASCII
//! letters so prefix bounds stay byte-safe.
//!
//! Retrieval supports prefix matching: for input `a`, entries whose code is
//! exactly `a` come first (with an empty `comment`), followed by entries whose
//! code merely starts with `a` (e.g. `aa`, `ad`), whose `comment` carries the
//! remaining required coding as `~a`, `~d`. Within each group, higher weight
//! wins first; a repeated text is dropped, and results are truncated to the
//! requested limit.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs;
use std::path::Path;

use crate::types::Candidate;

/// A single lexicon row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    code: String,
    text: String,
    weight: i64,
}

/// Errors produced while reading or parsing a lexicon file.
#[derive(Debug)]
pub enum DictionaryError {
    Io(std::io::Error),
    Format { line: usize, message: String },
}

impl fmt::Display for DictionaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DictionaryError::Io(e) => write!(f, "io error: {e}"),
            DictionaryError::Format { line, message } => write!(f, "line {line}: {message}"),
        }
    }
}

impl std::error::Error for DictionaryError {}

impl From<std::io::Error> for DictionaryError {
    fn from(e: std::io::Error) -> Self {
        DictionaryError::Io(e)
    }
}

/// Lexicon grouped by encoding code.
///
/// Entries sharing a code stay in file order; lookups sort each group by
/// weight with a stable sort, so equal weights keep their file order.
#[derive(Debug, Default, Clone)]
pub struct Dictionary {
    by_code: BTreeMap<String, Vec<Entry>>,
    total_entries: usize,
}

impl Dictionary {
    /// Parse TSV content already in memory.
    pub fn parse_str(content: &str) -> Result<Self, DictionaryError> {
        let mut by_code: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        let mut total_entries = 0usize;
        for (idx, raw) in content.lines().enumerate() {
            let line = idx + 1;
            if raw.trim().is_empty() {
                continue;
            }
            let mut cols = raw.split('\t');
            let code = cols.next().unwrap_or_default();
            let text = cols.next().unwrap_or_default();
            let weight_str = cols.next().unwrap_or_default();
            let fail = |message: String| DictionaryError::Format { line, message };
            if code.is_empty() {
                return Err(fail("empty code".into()));
            }
            if !code.bytes().all(|b| b.is_ascii_lowercase()) {
                return Err(fail(format!("code {code:?} must use letters a-z")));
            }
            if text.is_empty() {
                return Err(fail("empty text".into()));
            }
            let weight: i64 = weight_str.trim().parse().map_err(|_| {
                fail(format!("invalid weight {weight_str:?}"))
            })?;
            if cols.next().is_some() {
                return Err(fail("too many columns".into()));
            }
            by_code
                .entry(code.to_string())
                .or_default()
                .push(Entry { code: code.to_string(), text: text.to_string(), weight });
            total_entries += 1;
        }
        Ok(Dictionary { by_code, total_entries })
    }

    /// Read and parse a TSV file from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, DictionaryError> {
        let content = fs::read_to_string(path)?;
        Self::parse_str(&content)
    }

    /// Number of valid entries loaded.
    pub fn entry_count(&self) -> usize {
        self.total_entries
    }

    /// Prefix-match lookup. Exact-code candidates come first, then
    /// longer-code candidates whose code starts with `input`; at most
    /// `limit` candidates are returned.
    ///
    /// One text is offered at most once: the same word can sit under several
    /// codes (`a`, `aaa`, `aaaa` all map to `工`), and a candidate window
    /// cannot label duplicates distinctly. The first occurrence — the exact
    /// code or the highest weight — wins, and the freed slot is filled by the
    /// next match.
    pub fn lookup(&self, input: &str, limit: usize) -> Vec<Candidate> {
        if input.is_empty() || limit == 0 {
            return Vec::new();
        }

        // Codes starting with `input` form a contiguous BTreeMap range; the
        // `starts_with` check keeps classification explicit.
        let mut exact: Vec<&Entry> = Vec::new();
        let mut prefix: Vec<&Entry> = Vec::new();
        for (_code, group) in self
            .by_code
            .range(input.to_string()..prefix_upper_bound(input))
        {
            for entry in group {
                if entry.code == input {
                    exact.push(entry);
                } else if entry.code.starts_with(input) {
                    prefix.push(entry);
                }
            }
        }

        // Stable descending-weight sort within each group.
        exact.sort_by_key(|e| Reverse(e.weight));
        prefix.sort_by_key(|e| Reverse(e.weight));

        let mut out: Vec<Candidate> = Vec::with_capacity(limit);
        let mut seen: HashSet<&str> = HashSet::with_capacity(limit);
        for e in exact {
            if out.len() >= limit {
                return out;
            }
            if !seen.insert(e.text.as_str()) {
                continue;
            }
            out.push(Candidate { text: e.text.clone(), comment: String::new() });
        }
        for e in prefix {
            if out.len() >= limit {
                return out;
            }
            if !seen.insert(e.text.as_str()) {
                continue;
            }
            // `input` is ASCII lowercase, so this byte offset is a char boundary.
            let remaining = &e.code[input.len()..];
            out.push(Candidate { text: e.text.clone(), comment: format!("~{remaining}") });
        }
        out
    }
}

/// First string strictly greater than every string starting with `input`.
///
/// Codes only contain `[a-z]`, so the bound is `input` with its last
/// non-`z` byte incremented (trailing `z`s dropped); an input that is all
/// `z`s gets a sentinel byte (`{`) that sorts after every lowercase code.
fn prefix_upper_bound(input: &str) -> String {
    let mut bytes = input.as_bytes().to_vec();
    while let Some(last) = bytes.last_mut() {
        if *last < b'z' {
            *last += 1;
            return String::from_utf8(bytes).expect("input is [a-z]");
        }
        bytes.pop();
    }
    "{".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Dictionary {
        Dictionary::parse_str(
            "a\t工\t100\n\
             a\t戈\t90\n\
             aa\t式\t100\n\
             aaa\t工\t100\n\
             aaaa\t工\t100\n\
             aaaa\t㠭\t80\n\
             ad\t匿\t100\n\
             g\t王\t100\n\
             gg\t五\t100\n\
             b\t了\t100",
        )
        .unwrap()
    }

    fn cand(text: &str, comment: &str) -> Candidate {
        Candidate { text: text.into(), comment: comment.into() }
    }

    #[test]
    fn parses_sample_rows() {
        assert_eq!(sample().entry_count(), 10);
    }

    #[test]
    fn lookup_a_returns_exact_then_prefix_in_stable_weight_order() {
        assert_eq!(
            sample().lookup("a", 5),
            vec![
                cand("工", ""),      // exact, weight 100
                cand("戈", ""),      // exact, weight 90
                cand("式", "~a"),    // aa,   weight 100
                cand("匿", "~d"),    // ad,   weight 100 (工 under aaa/aaaa is a repeat)
                cand("㠭", "~aaa"),  // aaaa, weight 80
            ]
        );
    }

    #[test]
    fn lookup_aa() {
        assert_eq!(
            sample().lookup("aa", 5),
            vec![cand("式", ""), cand("工", "~a"), cand("㠭", "~aa")]
        );
    }

    /// A text listed under several codes must occupy exactly one slot, so the
    /// shell's candidate numbering stays one-to-one with what it can select.
    #[test]
    fn repeated_text_is_offered_once_and_fills_from_later_matches() {
        let d = Dictionary::parse_str("a\t一\t10\nab\t一\t9\nabc\t一\t8\n").unwrap();
        assert_eq!(d.lookup("a", 5), vec![cand("一", "")]);

        // The exact-match copy wins, so the surviving comment is the exact one.
        let d = Dictionary::parse_str("a\t一\t1\nab\t一\t9\nac\t二\t5\n").unwrap();
        assert_eq!(d.lookup("a", 5), vec![cand("一", ""), cand("二", "~c")]);
    }

    #[test]
    fn lookup_never_returns_duplicate_texts() {
        for input in ["a", "aa", "aaa", "aaaa"] {
            let got = sample().lookup(input, 5);
            let unique: HashSet<&str> = got.iter().map(|c| c.text.as_str()).collect();
            assert_eq!(unique.len(), got.len(), "duplicate candidate for {input:?}: {got:?}");
        }
    }

    #[test]
    fn empty_input_or_limit_returns_nothing() {
        assert!(sample().lookup("", 5).is_empty());
        assert!(sample().lookup("a", 0).is_empty());
    }

    #[test]
    fn respects_limit() {
        assert_eq!(sample().lookup("a", 3).len(), 3);
        assert_eq!(sample().lookup("a", 1), vec![cand("工", "")]);
    }

    #[test]
    fn miss_returns_empty() {
        assert!(sample().lookup("z", 5).is_empty());
        // Codes must start with the input, not the other way around.
        assert!(sample().lookup("aaaaa", 5).is_empty());
    }

    #[test]
    fn rejects_malformed_rows() {
        for bad in [
            "a\t工",              // missing weight
            "a\t工\t100\tx",      // extra column
            "\t工\t100",          // empty code
            "a\t\t100",           // empty text
            "a\t工\thigh",        // weight not numeric
            "A\t工\t100",         // uppercase code
            "a1\t工\t100",        // digit in code
        ] {
            let err = Dictionary::parse_str(bad).unwrap_err();
            assert!(matches!(err, DictionaryError::Format { .. }), "should reject {bad:?}: {err}");
        }
    }

    #[test]
    fn ignores_blank_lines_and_accepts_weight_zero() {
        let d = Dictionary::parse_str("a\t工\t0\n\n   \nb\t了\t50\n").unwrap();
        assert_eq!(d.entry_count(), 2);
        assert_eq!(d.lookup("a", 5), vec![cand("工", "")]);
    }

    #[test]
    fn loads_shipped_sample_dictionary() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/xingma_sample.tsv");
        let dict = Dictionary::load(path).expect("shipped dictionary must load");
        assert!(dict.entry_count() >= 10);
        assert_eq!(dict.lookup("a", 1), vec![cand("工", "")]);
        assert_eq!(dict.lookup("g", 1), vec![cand("王", "")]);
    }

    #[test]
    fn load_reports_missing_file() {
        let err = Dictionary::load("/nonexistent/hime-dict.tsv").unwrap_err();
        assert!(matches!(err, DictionaryError::Io(_)));
    }
}
