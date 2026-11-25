//! Full-pinyin word lexicon (SPEC §8.2).
//!
//! Rows are `候选\t连写全拼[\t词频]` — the shipped table is the `.hdict`
//! container at `data/schemes/pinyin/py.hdict`. A code is one concatenated
//! full-pinyin string (`开发\tkaifa`), so a query covers every word whose
//! code starts with the typed letters. That is exactly the 全拼 reading with
//! no abbreviations: `nih` reaches `你好`, `xian` reaches both `先` and
//! `西安`, and `kaif` reaches `开发`. Syllable segmentation is only needed
//! for two things, both on the small result path: finding word prefixes that
//! end on a syllable boundary (`开` while typing `kaif`) and showing the
//! display split (`ni hao`).
//!
//! The index is one flat vector sorted by code, so loading is a single sort
//! rather than a million tree insertions, and a query is two binary searches
//! plus a bounded top-K scan.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::hdict;

use super::syllables::{self, MAX_CODE_LEN, MAX_QUERY_SEGMENTATIONS};

/// One ranked pinyin hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinyinMatch {
    /// Word to commit.
    pub text: String,
    /// Display split of the matched code, e.g. `ni hao`; longest syllables
    /// first.
    pub key: String,
    /// The code itself (concatenated, normalized `lve` / `nve`).
    pub code: String,
    /// The typed code equals the word's whole code (no extra syllables).
    pub exact: bool,
    /// Distance between the word's syllable count and the number of syllable
    /// positions the query spells; 0 is the reference's first ranking rule
    /// (`kaifa` puts 开发 ahead of 开发者, and `n` puts 你 ahead of 你好).
    pub syllable_gap: usize,
    /// Number of query letters this hit accounts for: the query length for
    /// prefix matches, the code length for a word prefix.
    pub coverage: usize,
    /// Table frequency; higher comes first.
    pub weight: i64,
    /// 1-based table order (the tie-breaker behind weight).
    pub rank: usize,
}

/// A pinyin table that is loaded on first use.
///
/// The table is 11 MB of compressed rows and needs a full sort to index, which
/// must not happen while a session is being created — a process that only ever
/// types 形码 pays nothing for it. The first 反查 (or the first 全拼 keystroke)
/// pays the load once; every later caller shares the result. A load failure is
/// remembered too, so a broken table is reported once instead of per keystroke.
pub struct SharedPinyin {
    path: PathBuf,
    loaded: OnceLock<Result<Arc<PinyinLexicon>, String>>,
}

impl SharedPinyin {
    /// Point at a table file; nothing is read until [`Self::get`].
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), loaded: OnceLock::new() }
    }

    /// Wrap a table that is already loaded (tests, shells with their own
    /// loader). The path is a descriptive placeholder.
    #[must_use]
    pub fn from_loaded(lexicon: Arc<PinyinLexicon>) -> Self {
        let shared = Self { path: PathBuf::from("<loaded>"), loaded: OnceLock::new() };
        let _ = shared.loaded.set(Ok(lexicon));
        shared
    }

    /// The table, loading it on first call. `None` when the file is missing or
    /// malformed; the reason goes to stderr once.
    #[must_use]
    pub fn get(&self) -> Option<Arc<PinyinLexicon>> {
        let outcome = self.loaded.get_or_init(|| {
            PinyinLexicon::load(&self.path).map(Arc::new).map_err(|e| {
                eprintln!("hime-core: failed to load pinyin table {:?}: {e}", self.path);
                e.to_string()
            })
        });
        match outcome {
            Ok(lexicon) if !lexicon.is_empty() => Some(Arc::clone(lexicon)),
            _ => None,
        }
    }

    /// The file this table reads.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Debug for SharedPinyin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedPinyin")
            .field("path", &self.path)
            .field("loaded", &self.loaded.get().is_some())
            .finish()
    }
}

impl PartialEq for SharedPinyin {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Eq for SharedPinyin {}

/// One table row. `code` is normalized so `lue` / `nue` answer both ways.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    text: String,
    code: String,
    /// Fewest syllables `code` can be read as; 0 when it cannot be read at
    /// all (kept at the end of the ranking).
    syllables: usize,
    weight: i64,
    rank: usize,
}

/// Pinyin word table sorted by code.
#[derive(Debug, Default)]
pub struct PinyinLexicon {
    rows: Vec<Row>,
    entry_count: usize,
}

impl PinyinLexicon {
    /// Read a `.hdict` container or a plain `候选\t连写全拼[\t词频]` text file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, hdict::HDictError> {
        let text = hdict::load_table_text(path)?;
        Ok(Self::parse(&text))
    }

    /// Build the index from table text already in memory.
    #[must_use]
    pub fn parse(content: &str) -> Self {
        let mut rows: Vec<Row> = Vec::new();
        for (index, raw) in content.lines().enumerate() {
            let line = raw.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut cols = line.split('\t');
            let (Some(text), Some(code)) = (cols.next(), cols.next()) else { continue };
            let text = text.trim();
            let code = code.trim();
            let weight = cols.next().and_then(|w| w.trim().parse::<i64>().ok()).unwrap_or(0);
            if text.is_empty()
                || code.is_empty()
                || code.len() > MAX_CODE_LEN
                || !code.bytes().all(|b| b.is_ascii_lowercase())
            {
                continue;
            }
            let code = normalize_code(code);
            rows.push(Row {
                text: text.to_owned(),
                syllables: syllables::min_syllables(&code).unwrap_or(0),
                code,
                weight,
                rank: index + 1,
            });
        }
        // Code first: a query is a binary-searched code range. Inside one code
        // the most frequent word leads (the builder's own order).
        rows.sort_by(|a, b| {
            a.code
                .cmp(&b.code)
                .then_with(|| b.weight.cmp(&a.weight))
                .then_with(|| a.rank.cmp(&b.rank))
                .then_with(|| a.text.cmp(&b.text))
        });
        // A hand-written table may list the same (word, code) twice; the first
        // after sorting is the best one.
        rows.dedup_by(|a, b| a.code == b.code && a.text == b.text);
        let entry_count = rows.len();
        Self { rows, entry_count }
    }

    /// Number of usable table rows.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Number of distinct codes.
    #[must_use]
    pub fn code_count(&self) -> usize {
        self.rows
            .iter()
            .map(|row| row.code.as_str())
            .collect::<HashSet<_>>()
            .len()
    }

    /// Whether the lexicon holds nothing usable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Candidates for a typed full-pinyin string, best first, at most
    /// `limit`.
    #[must_use]
    pub fn lookup(&self, input: &str, limit: usize) -> Vec<PinyinMatch> {
        if input.is_empty()
            || limit == 0
            || input.len() > MAX_CODE_LEN
            || !input.bytes().all(|b| b.is_ascii_lowercase())
        {
            return Vec::new();
        }
        let query = normalize_code(input);
        // The query's own syllable count: the first ranking rule compares a
        // word's syllable count against it (reference rule 1).
        let positions = query_positions(input);
        // A range wider than one page can be large (`z`); keep only the best
        // few candidates seen so far instead of materializing every row.
        let capacity = limit.saturating_mul(4).max(limit);
        let mut top = TopK::new(capacity);

        // 1. Every word whose code starts with the typed letters.
        let upper = format!("{query}{{");
        let start = self.rows.partition_point(|row| row.code.as_str() < query.as_str());
        let end = self.rows.partition_point(|row| row.code.as_str() < upper.as_str());
        for row in &self.rows[start..end] {
            top.offer(scored(row, query.len(), row.code == query, positions));
        }

        // 2. Words covering an earlier, complete-syllable prefix of the query
        //    (`开` while typing `kaifa`).
        let mut seen_prefixes: HashSet<String> = HashSet::new();
        for form in syllables::segmentations(input, MAX_QUERY_SEGMENTATIONS) {
            let mut end = 0;
            let mut positions = 0;
            for pattern in form {
                if !pattern.complete {
                    break;
                }
                end += pattern.text.len();
                positions += 1;
                let prefix = normalize_code(&input[..end]);
                if !seen_prefixes.insert(prefix.clone()) {
                    continue;
                }
                for row in self.exact_rows(&prefix) {
                    top.offer(scored(row, prefix.len(), false, positions));
                }
            }
        }

        let mut scored = top.into_sorted();
        // One text can appear under several codes (readings); keep the best.
        let mut seen_text: HashSet<&str> = HashSet::new();
        scored.retain(|item| seen_text.insert(item.text));
        scored.truncate(limit);
        scored
            .into_iter()
            .map(|item| PinyinMatch {
                key: display_split(item.code),
                text: item.text.to_owned(),
                code: item.code.to_owned(),
                exact: item.exact,
                syllable_gap: item.syllable_gap,
                coverage: item.coverage,
                weight: item.weight,
                rank: item.rank,
            })
            .collect()
    }

    /// The rows whose whole code is `code`, best first.
    fn exact_rows(&self, code: &str) -> &[Row] {
        let start = self.rows.partition_point(|row| row.code.as_str() < code);
        let end = self.rows.partition_point(|row| row.code.as_str() <= code);
        &self.rows[start..end]
    }
}

/// Syllable positions a query spells: the longest-syllable reading's count
/// (`xian` → 1, `kaif` → 2, `nihao` → 2). One is the floor, so a code that
/// cannot be segmented at all still gets a comparable number.
fn query_positions(input: &str) -> usize {
    syllables::segmentations(input, 1)
        .first()
        .map(|form| form.len())
        .unwrap_or(1)
}

/// `lue` / `nue` and `lve` / `nve` are the same syllables; normalize the
/// concatenated code so a table written with either form answers both.
fn normalize_code(code: &str) -> String {
    if code.contains("lue") || code.contains("nue") {
        code.replace("lue", "lve").replace("nue", "nve")
    } else {
        code.to_owned()
    }
}

/// The display split of a code: its longest-syllable reading joined by
/// spaces (`nihao` → `ni hao`), falling back to the raw code.
fn display_split(code: &str) -> String {
    syllables::syllabifications(code, 1)
        .into_iter()
        .next()
        .map(|form| form.join(" "))
        .unwrap_or_else(|| code.to_owned())
}

/// One hit built from a row plus the query it was found for.
fn scored<'a>(row: &'a Row, coverage: usize, exact: bool, positions: usize) -> Scored<'a> {
    Scored {
        text: &row.text,
        code: &row.code,
        exact,
        syllable_gap: row.syllables.abs_diff(positions),
        coverage,
        weight: row.weight,
        rank: row.rank,
    }
}

/// A hit before its display split is computed. Borrowed from the table while
/// the top-K heap is filled, then sorted and materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Scored<'a> {
    text: &'a str,
    code: &'a str,
    exact: bool,
    syllable_gap: usize,
    coverage: usize,
    weight: i64,
    rank: usize,
}

impl Scored<'_> {
    fn is_better_than(&self, other: &Self) -> bool {
        (
            self.syllable_gap,
            !self.exact,
            Reverse(self.coverage),
            Reverse(self.weight),
            self.rank,
            self.text.len(),
            self.text,
            self.code,
        )
            .cmp(&(
                other.syllable_gap,
                !other.exact,
                Reverse(other.coverage),
                Reverse(other.weight),
                other.rank,
                other.text.len(),
                other.text,
                other.code,
            ))
            == Ordering::Less
    }
}

/// `Ord` is inverted: the "greatest" entry is the worst kept one, so a
/// max-heap's root is the first to evict when a better row arrives.
impl Ord for Scored<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.is_better_than(other) {
            Ordering::Less
        } else if other.is_better_than(self) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }
}

impl PartialOrd for Scored<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Bounded best-first collector.
struct TopK<'a> {
    capacity: usize,
    heap: BinaryHeap<Scored<'a>>,
}

impl<'a> TopK<'a> {
    fn new(capacity: usize) -> Self {
        Self { capacity, heap: BinaryHeap::with_capacity(capacity + 1) }
    }

    fn offer(&mut self, item: Scored<'a>) {
        if self.capacity == 0 {
            return;
        }
        if self.heap.len() < self.capacity {
            self.heap.push(item);
        } else if let Some(worst) = self.heap.peek() {
            if item < *worst {
                self.heap.pop();
                self.heap.push(item);
            }
        }
    }

    fn into_sorted(self) -> Vec<Scored<'a>> {
        let mut list = self.heap.into_vec();
        list.sort();
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lexicon() -> PinyinLexicon {
        PinyinLexicon::parse(
            "你好\tnihao\t200\n\
             你\tni\t100\n\
             呢\tni\t90\n\
             泥\tni\t90\n\
             开发\tkaifa\t100\n\
             开\tkai\t90\n\
             先\txian\t100\n\
             西安\txian\t80\n\
             略\tlve\t50\n\
             绿色\tlvse\t40\n",
        )
    }

    fn texts(list: &[PinyinMatch]) -> Vec<&str> {
        list.iter().map(|m| m.text.as_str()).collect()
    }

    #[test]
    fn exact_code_comes_first_then_longer_words() {
        let list = lexicon().lookup("ni", 5);
        assert_eq!(texts(&list), vec!["你", "呢", "泥", "你好"]);
        assert!(list[..3].iter().all(|hit| hit.exact));
        assert!(!list[3].exact, "你好 needs a longer query");
    }

    #[test]
    fn a_prefix_of_the_code_matches_the_next_syllable() {
        let list = lexicon().lookup("nih", 5);
        assert_eq!(texts(&list)[0], "你好");
        assert!(!list[0].exact);
        // Words covering only the completed `ni` stay available behind it.
        assert_eq!(texts(&list)[1..], ["你", "呢", "泥"]);
    }

    #[test]
    fn prefix_words_rank_behind_words_covering_more_letters() {
        let list = lexicon().lookup("kaif", 5);
        assert_eq!(texts(&list), vec!["开发", "开"]);
        assert_eq!(list[0].coverage, 4);
        assert_eq!(list[1].coverage, 3);
        assert_eq!(list[0].key, "kai fa");
        assert_eq!(list[1].key, "kai");
    }

    #[test]
    fn ambiguous_syllables_share_one_code() {
        let list = lexicon().lookup("xian", 5);
        assert!(texts(&list).contains(&"先"), "{list:?}");
        assert!(texts(&list).contains(&"西安"), "{list:?}");
        assert_eq!(list[0].text, "先", "weight 100 beats 西安's 80");
        assert_eq!(list[0].key, "xian");
        assert_eq!(
            list.iter().find(|hit| hit.text == "西安").unwrap().key,
            "xian",
            "the display prefers the longest syllable reading"
        );
    }

    #[test]
    fn ue_spellings_are_interchangeable() {
        for query in ["lve", "lue"] {
            let list = lexicon().lookup(query, 3);
            assert_eq!(texts(&list)[0], "略", "query {query}");
            assert_eq!(list[0].key, "lve");
        }
        assert_eq!(texts(&lexicon().lookup("lvse", 3)), vec!["绿色"]);
    }

    #[test]
    fn table_order_breaks_weight_ties() {
        let list = lexicon().lookup("ni", 4);
        assert_eq!(
            texts(&list)[..3],
            ["你", "呢", "泥"],
            "呢 and 泥 both weigh 90; the earlier table row wins"
        );
    }

    #[test]
    fn empty_or_invalid_queries_return_nothing() {
        let lex = lexicon();
        assert!(lex.lookup("", 5).is_empty());
        assert!(lex.lookup("ni", 0).is_empty());
        assert!(lex.lookup("ni3", 5).is_empty());
        assert!(lex.lookup("zzz", 5).is_empty());
    }

    #[test]
    fn malformed_rows_are_skipped() {
        let lex =
            PinyinLexicon::parse("# comment\n\n缺码\n坏码\tXYZ\n坏码\t123\n好\tni\t1\n");
        assert_eq!(lex.entry_count(), 1);
        assert_eq!(texts(&lex.lookup("ni", 5)), vec!["好"]);
    }

    #[test]
    fn the_top_k_keeps_the_best_when_the_range_is_wide() {
        // 200 rows all matching `a`, with increasing weight; only the last
        // few are worth returning. The heap capacity is 4 * limit.
        let rows: String = (0..200u32)
            .map(|i| {
                let first = char::from(b'a' + (i / 26) as u8);
                let second = char::from(b'a' + (i % 26) as u8);
                format!("词{i}\ta{first}{second}\t{i}\n")
            })
            .collect();
        let lex = PinyinLexicon::parse(&rows);
        let list = lex.lookup("a", 2);
        assert_eq!(texts(&list), vec!["词199", "词198"]);
    }

    /// The shared table reads nothing until first use, answers from then on,
    /// and reports a broken file once instead of per keystroke.
    #[test]
    fn a_shared_table_loads_on_first_use() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/schemes/pinyin/py.hdict");
        let shared = SharedPinyin::new(path);
        assert_eq!(shared.path(), Path::new(path));

        let lexicon = shared.get().expect("the shipped table loads");
        assert!(lexicon.entry_count() > 900_000);
        assert_eq!(
            Arc::as_ptr(&lexicon),
            Arc::as_ptr(&shared.get().expect("second call answers from the cache")),
            "later callers share one loaded table"
        );

        let missing = SharedPinyin::new("/nonexistent/hime-pinyin.hdict");
        assert!(missing.get().is_none());
        assert!(missing.get().is_none(), "a failure is remembered, not retried");

        let loaded = SharedPinyin::from_loaded(Arc::new(lexicon_for_tests()));
        assert_eq!(loaded.get().map(|l| l.entry_count()), Some(1));
    }

    fn lexicon_for_tests() -> PinyinLexicon {
        PinyinLexicon::parse("你\tni\t1\n")
    }

    #[test]
    fn loads_the_shipped_pinyin_table() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/schemes/pinyin/py.hdict");
        let lex = PinyinLexicon::load(path).expect("shipped pinyin table loads");
        assert!(lex.entry_count() > 900_000, "{} entries", lex.entry_count());
        assert!(lex.code_count() > 500_000, "{} codes", lex.code_count());

        assert_eq!(texts(&lex.lookup("nihao", 5))[0], "你好");
        assert_eq!(texts(&lex.lookup("kaifa", 5))[0], "开发");
        assert_eq!(texts(&lex.lookup("de", 5))[0], "的");
        assert!(texts(&lex.lookup("zhuang", 10)).contains(&"装"), "zhuang hits");
    }
}
