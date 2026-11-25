//! The whole-sentence word graph (port of the reference `SentenceLexicon`).
//!
//! A shape-code table (`候选\t编码`, the same layout as a scheme table) is read
//! into `code → ranked edges`. Rank is the row's position under that code
//! (1-based, file order); like the 虎整句 runtime, rank comes from the table
//! alone, with no user-word overlay. The graph also records the set of distinct
//! code lengths, every proper prefix of every code (used to recognise an
//! incomplete tail), and an `optimal_single` flag marking a single-character
//! edge that uses that character's shortest code.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use super::config::MAX_CODE_LENGTH;

/// One word graph edge: a lexicon entry that consumes `code.len()` keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentenceEdge {
    /// The text the edge emits (one or more characters).
    pub text: String,
    /// This entry's rank under its code (1-based, file order).
    pub rank: u16,
    /// Whether this is a single character reached by its shortest code.
    pub optimal_single: bool,
}

/// Word graph for whole-sentence decoding: `code → ranked edges`.
#[derive(Debug, Default)]
pub struct SentenceLexicon {
    codes: HashMap<String, Vec<SentenceEdge>>,
    lengths: Vec<usize>,
    proper_prefixes: HashSet<String>,
    max_code_length: usize,
    entry_count: usize,
    /// `text → complete code`, for the 反查 window (SPEC §8.3): the longest
    /// code a text carries, the smaller string among equals.
    reverse: HashMap<String, String>,
}

impl SentenceLexicon {
    /// Read a sentence codes table from disk.
    pub fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let text = fs::read_to_string(path)?;
        Ok(Self::parse(&text))
    }

    /// Build the graph from table text already in memory.
    #[must_use]
    pub fn parse(content: &str) -> Self {
        let mut codes: HashMap<String, Vec<SentenceEdge>> = HashMap::new();
        let mut length_values: HashSet<usize> = HashSet::new();
        let mut entry_count = 0usize;
        // Shortest code seen per single-character text (the code string itself,
        // not just its length), for optimal_single. Ties keep the first seen.
        let mut optimal_single_code: HashMap<String, String> = HashMap::new();

        for line in content.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // `候选\t编码` — ignore any trailing frequency column.
            let mut cols = line.split('\t');
            let (Some(text), Some(code)) = (cols.next(), cols.next()) else {
                continue;
            };
            let text = text.trim();
            let code = code.trim();
            if text.is_empty() || !is_simple_code(code) {
                continue;
            }
            let len = code.chars().count();
            if !(1..=MAX_CODE_LENGTH).contains(&len) {
                continue;
            }

            let edges = codes.entry(code.to_string()).or_default();
            if edges.iter().any(|e| e.text == text) {
                continue; // same (code, text): keep the first rank only
            }
            let rank = edges.len() as u16 + 1;
            edges.push(SentenceEdge { text: text.to_string(), rank, optimal_single: false });
            length_values.insert(len);
            entry_count += 1;

            if text.chars().count() == 1 {
                match optimal_single_code.get_mut(text) {
                    // Replace only on a strictly shorter code; ties keep the first.
                    Some(existing) if len < existing.chars().count() => {
                        *existing = code.to_string();
                    }
                    Some(_) => {}
                    None => {
                        optimal_single_code.insert(text.to_string(), code.to_string());
                    }
                }
            }
        }

        let max_code_length = *length_values.iter().max().unwrap_or(&1);

        // Flag single-character edges that sit on the character's shortest code
        // (only multi-key codes can carry the whole-input single reward).
        for (code, edges) in codes.iter_mut() {
            if code.chars().count() < 2 {
                continue;
            }
            for edge in edges.iter_mut() {
                edge.optimal_single = edge.text.chars().count() == 1
                    && optimal_single_code.get(&edge.text).map(String::as_str) == Some(code.as_str());
            }
        }

        // Proper prefixes of every code (a partial key the table could extend).
        let mut proper_prefixes: HashSet<String> = HashSet::new();
        for code in codes.keys() {
            let chars: Vec<char> = code.chars().collect();
            if chars.len() < 2 {
                continue;
            }
            let mut prefix = String::new();
            for ch in &chars[..chars.len() - 1] {
                prefix.push(*ch);
                proper_prefixes.insert(prefix.clone());
            }
        }

        let mut lengths: Vec<usize> = length_values.into_iter().collect();
        lengths.sort_unstable();

        // Reverse index for 反查: the same rule as a word table — the longest
        // code is the complete one, ties take the smaller string.
        let mut reverse: HashMap<String, String> = HashMap::new();
        for (code, edges) in &codes {
            for edge in edges {
                match reverse.get(&edge.text) {
                    Some(existing) if (code.len(), existing) > (existing.len(), code) => {
                        reverse.insert(edge.text.clone(), code.clone());
                    }
                    Some(_) => {}
                    None => {
                        reverse.insert(edge.text.clone(), code.clone());
                    }
                }
            }
        }

        SentenceLexicon {
            codes,
            lengths,
            proper_prefixes,
            max_code_length,
            entry_count,
            reverse,
        }
    }

    /// The complete code a text carries in this table (SPEC §8.3), or `None`
    /// when the table has no such entry.
    #[must_use]
    pub fn code_for(&self, text: &str) -> Option<&str> {
        self.reverse.get(text).map(String::as_str)
    }

    /// Edges for an exact code, ranked ascending, or `None` if the code is absent.
    #[must_use]
    pub fn edges(&self, code: &str) -> Option<&[SentenceEdge]> {
        self.codes.get(code).map(Vec::as_slice)
    }

    /// Whether `code` is a proper prefix of some table code (could extend).
    #[must_use]
    pub fn is_proper_prefix(&self, code: &str) -> bool {
        self.proper_prefixes.contains(code)
    }

    /// The distinct code lengths present, ascending.
    #[must_use]
    pub fn lengths(&self) -> &[usize] {
        &self.lengths
    }

    /// The longest code length in the table.
    #[must_use]
    pub fn max_code_length(&self) -> usize {
        self.max_code_length
    }

    /// Number of edges loaded.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Whether the graph has any edges.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }
}

/// A code of ASCII letters only (the sentence graph ignores digit/punct codes;
/// the decoder handles selection digits separately).
fn is_simple_code(code: &str) -> bool {
    !code.is_empty() && code.bytes().all(|b| b.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(edges: &[SentenceEdge]) -> Vec<&str> {
        edges.iter().map(|e| e.text.as_str()).collect()
    }

    /// Ranks follow file order; the same (code, text) listed twice is kept once.
    #[test]
    fn ranks_follow_file_order_and_drop_repeats() {
        let lex = SentenceLexicon::parse("的\td\n是\td\n的\td\n了\td\n");
        assert_eq!(texts(lex.edges("d").unwrap()), vec!["的", "是", "了"]);
        let ranks: Vec<u16> = lex.edges("d").unwrap().iter().map(|e| e.rank).collect();
        assert_eq!(ranks, vec![1, 2, 3]);
    }

    /// A single character reached by its shortest code is flagged optimal; a
    /// longer code for the same character is not. The single-key code never
    /// carries the flag (the reward is for whole multi-key inputs).
    #[test]
    fn optimal_single_marks_shortest_multi_key_code() {
        // 一 also has the 1-key code `f`; the 2-key `fd` is not its shortest, so
        // it is not optimal. 好 has codes hd (2) and hgd (3): hd is shortest.
        let lex = SentenceLexicon::parse("一\tf\n一\tfd\n好\thd\n好\thgd\n女\tv\n");
        let fd = lex.edges("fd").unwrap();
        assert!(!fd[0].optimal_single, "fd is longer than the shortest code f");
        assert!(lex.edges("hd").unwrap()[0].optimal_single);
        assert!(!lex.edges("hgd").unwrap()[0].optimal_single);
    }

    /// 反查 asks for the complete code: the longest a text carries, and the
    /// smaller string when two equally long codes exist.
    #[test]
    fn code_for_reports_the_longest_code() {
        let lex = SentenceLexicon::parse("你\tfl\n你\tflll\n好\tolgu\n好\tol\n");
        assert_eq!(lex.code_for("你"), Some("flll"));
        assert_eq!(lex.code_for("好"), Some("olgu"));
        assert_eq!(lex.code_for("无"), None);

        let tie = SentenceLexicon::parse("在\tbcd\n在\tacd\n");
        assert_eq!(tie.code_for("在"), Some("acd"));
    }

    /// Two equally-short multi-key codes for one character: only the first seen
    /// is optimal (matching the reference, which stores the shortest code string itself).
    #[test]
    fn optimal_single_ties_keep_the_first_code() {
        let lex = SentenceLexicon::parse("好\thd\n好\thx\n");
        assert!(lex.edges("hd").unwrap()[0].optimal_single);
        assert!(!lex.edges("hx").unwrap()[0].optimal_single);
    }

    /// The whole input is one edge (整段单边) for a code that exists.
    #[test]
    fn lengths_and_proper_prefixes() {
        let lex = SentenceLexicon::parse("的\td\n时候\tdh\n时候长\tdhx\n");
        assert_eq!(lex.lengths(), &[1, 2, 3]);
        assert_eq!(lex.max_code_length(), 3);
        assert!(lex.is_proper_prefix("d"));
        assert!(lex.is_proper_prefix("dh"));
        assert!(!lex.is_proper_prefix("dhx"), "a full code is not its own prefix");
        assert_eq!(lex.entry_count(), 3);
    }

    #[test]
    fn comments_and_malformed_rows_are_skipped() {
        let lex = SentenceLexicon::parse(
            "#name=x\n#\n\n好的\tdx\nbadrow\ndigit123\td2\n",
        );
        // only `好的 dx` is a valid 2-column letters-only code row
        assert_eq!(lex.entry_count(), 1);
        assert!(lex.edges("dx").is_some());
    }
}
