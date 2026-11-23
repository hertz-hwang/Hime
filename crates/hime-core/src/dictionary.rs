//! Dictionary loading and table lookup (SPEC §5).
//!
//! Two lexicon row formats are recognized, detected from the column count of
//! the first data row:
//!
//! - [`TableFormat::Weighted`] — `code<TAB>text<TAB>weight`, the format of
//!   the test fixture `data/xingma_sample.tsv`. Codes are lowercase ASCII
//!   letters, and a match group is ranked by descending weight.
//! - [`TableFormat::Scheme`] — `text<TAB>code`, the format of the real
//!   scheme tables under `data/schemes/` (the default scheme is 琉璃v1,
//!   `data/schemes/liuli_v1/liuli_table.txt`). Such tables carry no
//!   frequency; the table itself is sorted in strict lexicographic order
//!   over the codes (shorter code first, then byte order) and that order is
//!   the retrieval rank. Codes may additionally contain digits (琉璃 marks a
//!   few punctuation phrases with `2`).
//!
//! Lines starting with `#` are comments (scheme tables open with a
//! `#name=`/`#describe=` metadata block); blank lines are ignored.
//!
//! Retrieval supports prefix matching in both formats: for input `a`,
//! entries whose code is exactly `a` come first (with an empty `comment`),
//! followed by entries whose code merely starts with `a` (e.g. `aa`, `ad`),
//! whose `comment` carries the remaining required coding as `~a`, `~d`.
//! Within each group the format's own order applies; results are truncated
//! to the requested limit. A text listed under several codes is reported
//! under each of them — deciding what one candidate page shows belongs to
//! [`Filter`](crate::filter::Filter).

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;

use crate::types::Candidate;

/// Row layout of a lexicon file; also fixes how matches are ranked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableFormat {
    /// `code<TAB>text<TAB>weight`; matches rank by descending weight.
    Weighted,
    /// `text<TAB>code` without frequency; matches keep the table's own
    /// lexicographic order (shorter code first, then byte order).
    Scheme,
}

/// A single lexicon row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    code: String,
    text: String,
    /// Weighted tables only; `Scheme` entries carry no frequency and use 0.
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
/// Entries sharing a code stay in file order; lookups apply the ranking of
/// the file's [`TableFormat`].
#[derive(Debug, Clone)]
pub struct Dictionary {
    by_code: BTreeMap<String, Vec<Entry>>,
    total_entries: usize,
    format: TableFormat,
}

impl Dictionary {
    /// Parse lexicon content already in memory; the format is detected from
    /// the column count of the first data row (2 → Scheme, 3 → Weighted).
    pub fn parse_str(content: &str) -> Result<Self, DictionaryError> {
        let mut by_code: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        let mut total_entries = 0usize;

        let mut rows = content
            .lines()
            .enumerate()
            .filter(|(_, raw)| !raw.trim().is_empty() && !raw.starts_with('#'))
            .peekable();
        let mut format = TableFormat::Weighted;
        if let Some((idx, raw)) = rows.peek() {
            format = detect_format(raw, idx + 1)?;
        }

        for (idx, raw) in rows {
            let line = idx + 1;
            let cols: Vec<&str> = raw.split('\t').collect();
            let fail = |message: String| DictionaryError::Format { line, message };
            let (code, text, weight) = match format {
                TableFormat::Weighted => {
                    let weight_str = *cols.get(2).ok_or_else(|| {
                        fail(format!("expected 3 columns for code<TAB>text<TAB>weight, found {}", cols.len()))
                    })?;
                    if cols.len() != 3 {
                        return Err(fail(format!("expected 3 columns, found {}", cols.len())));
                    }
                    let (code, text) = (cols[0], cols[1]);
                    if code.is_empty() {
                        return Err(fail("empty code".into()));
                    }
                    if !code.bytes().all(|b| b.is_ascii_lowercase()) {
                        return Err(fail(format!("code {code:?} must use letters a-z")));
                    }
                    if text.is_empty() {
                        return Err(fail("empty text".into()));
                    }
                    let weight: i64 = weight_str
                        .trim()
                        .parse()
                        .map_err(|_| fail(format!("invalid weight {weight_str:?}")))?;
                    (code, text, weight)
                }
                TableFormat::Scheme => {
                    if cols.len() != 2 {
                        return Err(fail(format!(
                            "expected 2 columns for text<TAB>code, found {}",
                            cols.len()
                        )));
                    }
                    let (text, code) = (cols[0], cols[1]);
                    if text.is_empty() {
                        return Err(fail("empty text".into()));
                    }
                    if code.is_empty() {
                        return Err(fail("empty code".into()));
                    }
                    if !code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
                        return Err(fail(format!("code {code:?} must use letters a-z or digits")));
                    }
                    (code, text, 0)
                }
            };
            by_code
                .entry(code.to_string())
                .or_default()
                .push(Entry { code: code.to_string(), text: text.to_string(), weight });
            total_entries += 1;
        }
        Ok(Dictionary { by_code, total_entries, format })
    }

    /// Read and parse a lexicon file from disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, DictionaryError> {
        let content = fs::read_to_string(path)?;
        Self::parse_str(&content)
    }

    /// The format the file was parsed as; decides match ranking.
    pub fn format(&self) -> TableFormat {
        self.format
    }

    /// Number of valid entries loaded.
    pub fn entry_count(&self) -> usize {
        self.total_entries
    }

    /// Prefix-match lookup. Exact-code candidates come first, then
    /// longer-code candidates whose code starts with `input`; at most
    /// `limit` candidates are returned. Inside each group, weighted tables
    /// rank by descending weight and scheme tables keep the table's own
    /// lexicographic order.
    ///
    /// This is the raw table report: every entry whose code matches is
    /// returned, including a word already listed under another code.
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

        match self.format {
            // Stable descending-weight sort within each group.
            TableFormat::Weighted => {
                exact.sort_by_key(|e| Reverse(e.weight));
                prefix.sort_by_key(|e| Reverse(e.weight));
            }
            // The table is sorted by code length first, then byte order.
            // Range iteration already yields byte order (file order within
            // one code), so a stable sort by code length restores the
            // table's own order; `exact` shares one code and keeps file
            // order untouched.
            TableFormat::Scheme => prefix.sort_by_key(|e| e.code.len()),
        }

        let mut out: Vec<Candidate> = Vec::with_capacity(limit);
        for e in exact {
            if out.len() >= limit {
                return out;
            }
            out.push(Candidate { text: e.text.clone(), comment: String::new() });
        }
        for e in prefix {
            if out.len() >= limit {
                return out;
            }
            // `input` is ASCII lowercase, so this byte offset is a char boundary.
            let remaining = &e.code[input.len()..];
            out.push(Candidate { text: e.text.clone(), comment: format!("~{remaining}") });
        }
        out
    }
}

/// Column count of the first data row decides the format.
fn detect_format(raw: &str, line: usize) -> Result<TableFormat, DictionaryError> {
    match raw.split('\t').count() {
        2 => Ok(TableFormat::Scheme),
        3 => Ok(TableFormat::Weighted),
        n => Err(DictionaryError::Format {
            line,
            message: format!("expected 2 (text<TAB>code) or 3 (code<TAB>text<TAB>weight) columns, found {n}"),
        }),
    }
}

/// First string strictly greater than every string starting with `input`.
///
/// Codes only contain `[a-z]` (weighted) plus digits (scheme tables), so the
/// bound is `input` with its last non-`z` byte incremented (trailing `z`s
/// dropped; a digit or letter successor still exceeds every valid
/// continuation byte); an input that is all `z`s gets a sentinel byte (`{`)
/// that sorts after every valid code byte.
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
        assert_eq!(sample().format(), TableFormat::Weighted);
    }

    #[test]
    fn lookup_a_returns_exact_then_prefix_in_stable_weight_order() {
        assert_eq!(
            sample().lookup("a", 5),
            vec![
                cand("工", ""),      // exact, weight 100
                cand("戈", ""),      // exact, weight 90
                cand("式", "~a"),    // aa,   weight 100
                cand("工", "~aa"),   // aaa,  weight 100
                cand("工", "~aaa"),  // aaaa, weight 100 (file order ahead of ad-匿)
            ]
        );
    }

    #[test]
    fn lookup_aa() {
        assert_eq!(
            sample().lookup("aa", 5),
            vec![cand("式", ""), cand("工", "~a"), cand("工", "~aa"), cand("㠭", "~aa")]
        );
    }

    /// A word listed under several codes is reported once per code: the table
    /// states what the lexicon contains, and deciding one page belongs to the
    /// filter stage.
    #[test]
    fn lookup_reports_a_word_once_per_matching_code() {
        let d = Dictionary::parse_str("a\t一\t10\nab\t一\t9\nabc\t一\t8\n").unwrap();
        assert_eq!(
            d.lookup("a", 5),
            vec![cand("一", ""), cand("一", "~b"), cand("一", "~bc")]
        );
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
            "a\t工",              // 2 columns, then "工" is no code
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
        let dict = Dictionary::load(path).expect("shipped fixture dictionary must load");
        assert!(dict.entry_count() >= 10);
        assert_eq!(dict.lookup("a", 1), vec![cand("工", "")]);
        assert_eq!(dict.lookup("g", 1), vec![cand("王", "")]);
    }

    #[test]
    fn load_reports_missing_file() {
        let err = Dictionary::load("/nonexistent/hime-dict.tsv").unwrap_err();
        assert!(matches!(err, DictionaryError::Io(_)));
    }

    // --- scheme tables (text<TAB>code, no frequency) ---

    #[test]
    fn scheme_rows_are_text_tab_code_and_headers_are_comments() {
        let d = Dictionary::parse_str(
            "#name=琉璃字词\n#describe=单字三码，词组四码\n\n的\td\n、\td\n的时候\td\n",
        )
        .unwrap();
        assert_eq!(d.format(), TableFormat::Scheme);
        assert_eq!(d.entry_count(), 3);
        assert_eq!(
            d.lookup("d", 5),
            vec![cand("的", ""), cand("、", ""), cand("的时候", "")]
        );
    }

    /// Without weights the table's own order is the rank: shorter codes
    /// first, byte order within one length, file order within one code.
    #[test]
    fn scheme_candidates_follow_the_tables_lexicographic_order() {
        let d = Dictionary::parse_str("一\ta\n二\ta\n三\taz\n四\tab\n五\taaa\n").unwrap();
        assert_eq!(
            d.lookup("a", 10),
            vec![
                cand("一", ""),
                cand("二", ""),
                cand("四", "~b"),   // ab, length 2, byte order first
                cand("三", "~z"),   // az, length 2
                cand("五", "~aa"),  // aaa, length 3 — last despite aa < az in pure byte order
            ]
        );
    }

    #[test]
    fn scheme_codes_may_contain_digits() {
        let d = Dictionary::parse_str("呢？\tpzw2\n呢！\tpza2\n").unwrap();
        assert_eq!(d.format(), TableFormat::Scheme);
        assert_eq!(d.lookup("pzw", 5), vec![cand("呢？", "~2")]);
        assert_eq!(d.lookup("pza", 5), vec![cand("呢！", "~2")]);
    }

    #[test]
    fn scheme_rejects_malformed_rows() {
        for bad in [
            "\ta",          // empty text
            "在\t",          // empty code
            "在\tA",        // uppercase code
            "在\ta b",      // space in code
            "在\ta\t100",   // 3 columns, then "在" is no code
            "在\ta\n二\tb\t5", // column count drifts mid-file
        ] {
            let err = Dictionary::parse_str(bad).unwrap_err();
            assert!(matches!(err, DictionaryError::Format { .. }), "should reject {bad:?}: {err}");
        }
    }

    #[test]
    fn weighted_table_rejects_short_rows() {
        let err = Dictionary::parse_str("a\t工\t100\n在\ta\n").unwrap_err();
        assert!(matches!(err, DictionaryError::Format { .. }), "{err}");
    }

    /// The default scheme ships with the repository; this is the regression
    /// test that the real table loads and ranks 的 first behind `d`.
    #[test]
    fn loads_shipped_default_scheme_table() {
        let path =
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/schemes/liuli_v1/liuli_table.txt");
        let dict = Dictionary::load(path).expect("default scheme table must load");
        assert_eq!(dict.format(), TableFormat::Scheme);
        assert!(dict.entry_count() >= 300_000);
        assert_eq!(dict.lookup("d", 1), vec![cand("的", "")]);
        // 单字三码: 的 also hangs under longer codes; a full 4-code phrase
        // must resolve to its exact entry.
        assert!(!dict.lookup("d", 32).is_empty());
    }
}
