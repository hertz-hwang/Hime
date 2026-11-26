//! Pinyin word lexicon with a syllable index (SPEC §8.2).
//!
//! Rows are `候选\t连写全拼[\t词频]` — the shipped table is the `.hdict`
//! container at `data/schemes/pinyin/py.hdict`. The flat code (`开发\tkaifa`)
//! is split into syllables at load time and stored as a packed integer key:
//! one 9-bit id per syllable, most significant first. A query is a sequence
//! of syllable *positions*, each of which is
//!
//! - a complete syllable (`kaifa` → `kai` `fa`), matched by equality;
//! - a 简拼 initial (`kf`, `zhg`), matched against every syllable that starts
//!   with it;
//! - the unfinished tail of a syllable (`zho`, `kaif`'s `f`), matched the
//!   same way.
//!
//! Because ids are packed in reading order, two integer comparisons bound the
//! block of keys that start with a given syllable prefix, so narrowing is a
//! binary search per position instead of a scan. This is what makes `kf`
//! reach `开发` and `nih` reach `你好`; a concatenated-code prefix search can
//! do neither.
//!
//! A code can have several syllable readings (`xian` is `xian` or `xi an`);
//! every reading is indexed, so `西安` answers both `xian` and `xi'an`. Rows
//! are de-duplicated by `(text, code)`; the index keeps one slot per
//! `(key, text)` with the best rank first.

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::hdict;

use super::syllables::{self, Pattern, MAX_CODE_LEN, MAX_QUERY_SEGMENTATIONS, MAX_WORD_SYLLABLES};

/// Syllabifications indexed per table code (`xian` → `xian`, `xi an`; at most
/// this many readings).
const MAX_READINGS: usize = 4;

/// Words offered per span while decoding, before model scores trim them.
const MAX_SPAN_MATCHES: usize = 128;

/// One ranked pinyin hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinyinMatch {
    /// Word to commit.
    pub text: String,
    /// Display split of the matched key, e.g. `ni hao`; longest syllables
    /// first.
    pub key: String,
    /// The code itself (concatenated, normalized `lve` / `nve`).
    pub code: String,
    /// The typed code equals the word's whole code (no extra syllables).
    pub exact: bool,
    /// Distance between the word's syllable count and the number of syllable
    /// positions the query spells; 0 means the reading has exactly as many
    /// syllables as were typed (`kaifa` puts 开发 ahead of 开发者).
    pub syllable_gap: usize,
    /// Number of query letters this hit accounts for: the query length for
    /// whole-query matches, the covered letters for a word prefix.
    pub coverage: usize,
    /// Table frequency; higher comes first.
    pub weight: i64,
    /// 1-based table order (the tie-breaker behind weight).
    pub rank: usize,
    /// 1-based rank of the word inside its syllable key (同码重码次序).
    pub ordinal: usize,
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

/// One distinct syllable key and its word slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeySpan {
    /// Syllable ids packed most significant first.
    packed: u128,
    /// First slot in [`PinyinLexicon::slots`].
    start: u32,
    /// Number of slots (distinct texts under this key).
    count: u32,
}

/// A hit before its display split is materialized. Borrowed from the table
/// while the bounded collector is filled, then sorted and materialized.
#[derive(Debug, Clone, Copy)]
struct Scored<'a> {
    row: &'a Row,
    packed: u128,
    exact: bool,
    syllable_gap: usize,
    coverage: usize,
    abbreviated: usize,
    full_last: bool,
    /// The matched key reads the code the way the table does (`ni hao` for
    /// `nihao`, whose fewest-syllable reading is 2). A code can also match
    /// under a finer reading (`ni ha o`); that one is kept behind.
    canonical_reading: bool,
    ordinal: usize,
}

/// The retrieval ranking (SPEC §8.2): the reading with exactly as many
/// syllables as were typed first, then full code equality, then more typed
/// letters covered, then fewer 简拼 positions, then a fully matched last
/// syllable, then the table's own reading of the code, then table frequency,
/// table order and finally text. `Reverse` inverts the fields where larger is
/// better.
fn rank_tuple<'a>(
    item: &'a Scored<'a>,
) -> (usize, bool, Reverse<usize>, usize, bool, bool, Reverse<i64>, usize, usize, &'a str, &'a str) {
    (
        item.syllable_gap,
        !item.exact,
        Reverse(item.coverage),
        item.abbreviated,
        !item.full_last,
        !item.canonical_reading,
        Reverse(item.row.weight),
        item.row.rank,
        item.row.text.len(),
        &item.row.text,
        &item.row.code,
    )
}

/// Natural tuple order, so the "greatest" entry is the worst ranked one and a
/// max-heap's root is the first to evict when a better row arrives.
impl Ord for Scored<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        rank_tuple(self).cmp(&rank_tuple(other))
    }
}

impl PartialOrd for Scored<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Scored<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Scored<'_> {}

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
        // Natural order: the best ranked entry is the smallest, so ascending
        // sort puts the best candidate first.
        list.sort();
        list
    }
}

/// Pinyin word table with a syllable index, in table order.
#[derive(Debug, Default)]
pub struct PinyinLexicon {
    rows: Vec<Row>,
    /// Distinct syllable keys, ascending by packed value.
    keys: Vec<KeySpan>,
    /// Row ids grouped by key; inside one key the best word leads.
    slots: Vec<u32>,
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
        let mut seen: HashSet<(String, String)> = HashSet::new();
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
            if !seen.insert((text.to_owned(), code.clone())) {
                continue;
            }
            rows.push(Row {
                text: text.to_owned(),
                syllables: syllables::min_syllables(&code).unwrap_or(0),
                code,
                weight,
                rank: index + 1,
            });
        }
        let entry_count = rows.len();

        // One (packed key, row id) pair per reading. Sorting by packed first
        // groups every key without a tree, and the tie-breakers put the most
        // frequent word first inside its key.
        let mut pairs: Vec<(u128, u32)> = Vec::with_capacity(rows.len() * 2);
        let mut reading_cache: HashMap<&str, Vec<u128>> = HashMap::with_capacity(rows.len() / 2);
        for (index, row) in rows.iter().enumerate() {
            let code_key: &str = &row.code;
            let forms = match reading_cache.get(code_key) {
                Some(cached) => cached.clone(),
                None => {
                    let forms = readings(code_key);
                    reading_cache.insert(code_key, forms.clone());
                    forms
                }
            };
            for packed in forms {
                pairs.push((packed, index as u32));
            }
        }
        pairs.sort_by(|left, right| {
            left.0.cmp(&right.0).then_with(|| {
                let a = &rows[left.1 as usize];
                let b = &rows[right.1 as usize];
                b.weight
                    .cmp(&a.weight)
                    .then_with(|| a.rank.cmp(&b.rank))
                    .then_with(|| a.text.cmp(&b.text))
            })
        });

        let mut keys: Vec<KeySpan> = Vec::new();
        let mut slots: Vec<u32> = Vec::with_capacity(pairs.len());
        let mut index = 0;
        while index < pairs.len() {
            let packed = pairs[index].0;
            let start = slots.len() as u32;
            let mut seen_text: HashSet<&str> = HashSet::new();
            while index < pairs.len() && pairs[index].0 == packed {
                let row = &rows[pairs[index].1 as usize];
                // A text may repeat under one key through several readings;
                // the best-ordered slot is the first one.
                if seen_text.insert(row.text.as_str()) {
                    slots.push(pairs[index].1);
                }
                index += 1;
            }
            keys.push(KeySpan { packed, start, count: slots.len() as u32 - start });
        }

        Self { rows, keys, slots, entry_count }
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

    /// Number of indexed syllable keys (each code contributes one per reading).
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    /// Whether the lexicon holds nothing usable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Word candidates for a typed pinyin string, best first, at most
    /// `limit`.
    ///
    /// The query is read as every syllable segmentation it admits: complete
    /// syllables, 简拼 initials and an unfinished tail. Whole-query matches
    /// come first, then words that cover a complete syllable prefix of the
    /// query (`开` while typing `kaifa`), which is what keeps long sentences
    /// committable word by word.
    #[must_use]
    pub fn lookup(&self, input: &str, limit: usize) -> Vec<PinyinMatch> {
        if input.is_empty()
            || limit == 0
            || input.len() > MAX_CODE_LEN
            || !input.bytes().all(|b| b.is_ascii_lowercase())
        {
            return Vec::new();
        }
        let whole_code = normalize_code(input);
        let capacity = limit.saturating_mul(4).max(limit);
        let mut top = TopK::new(capacity);
        let mut seen_forms: HashSet<Vec<u32>> = HashSet::new();

        for form in syllables::segmentations(input, MAX_QUERY_SEGMENTATIONS) {
            let signature = form_signature(&form);
            if !seen_forms.insert(signature) {
                continue;
            }
            let patterns: Vec<Vec<u32>> =
                form.iter().map(|pattern| pattern_ids(pattern)).collect();
            if patterns.iter().any(Vec::is_empty) {
                continue;
            }
            let letters = form.iter().map(|pattern| pattern.text.len()).sum::<usize>();
            let abbreviated = abbreviated_count(&form);
            let last_full = full_last(&form);
            self.collect(
                &patterns,
                0,
                0,
                0..self.keys.len(),
                false,
                &mut |packed, key_syllables, row, ordinal| {
                    top.offer(Scored {
                        row,
                        packed,
                        exact: row.code == whole_code,
                        syllable_gap: row.syllables.abs_diff(form.len()),
                        coverage: letters,
                        abbreviated,
                        full_last: last_full,
                        canonical_reading: key_syllables == row.syllables,
                        ordinal,
                    });
                },
            );

            // Words covering a complete-syllable prefix of the query. Only
            // the last few positions are worth enumerating: ranking puts
            // wider coverage first, so a one-syllable prefix of a long input
            // could never reach the visible page.
            let shortest = form.len().saturating_sub(PREFIX_LOOKBEHIND).max(1);
            for prefix_len in (shortest..form.len()).rev() {
                let prefix = &form[..prefix_len];
                let prefix_patterns = &patterns[..prefix_len];
                let prefix_letters: usize =
                    prefix.iter().map(|pattern| pattern.text.len()).sum();
                let prefix_abbreviated = abbreviated_count(prefix);
                let prefix_full_last = full_last(prefix);
                self.collect_exact(
                    prefix_patterns,
                    0,
                    0..self.keys.len(),
                    &mut |packed, key_syllables, row, ordinal| {
                        top.offer(Scored {
                            row,
                            packed,
                            exact: false,
                            syllable_gap: 0,
                            coverage: prefix_letters,
                            abbreviated: prefix_abbreviated,
                            full_last: prefix_full_last,
                            canonical_reading: key_syllables == row.syllables,
                            ordinal,
                        });
                    },
                );
            }
        }

        let mut scored = top.into_sorted();
        // One text can appear under several readings; keep the best.
        let mut seen_text: HashSet<&str> = HashSet::new();
        scored.retain(|item| seen_text.insert(item.row.text.as_str()));
        scored.truncate(limit);
        scored.into_iter().map(materialize).collect()
    }

    /// Every word whose reading has exactly `patterns.len()` syllables and
    /// matches every position, best (most frequent) first, at most `limit`.
    ///
    /// This is the word-graph edge source for the whole-sentence decoder
    /// ([`super::decoder`]): one call answers "which words fill the syllable
    /// span `patterns`".
    #[must_use]
    pub fn exact_matches(&self, patterns: &[Pattern], limit: usize) -> Vec<PinyinMatch> {
        if patterns.is_empty() || limit == 0 || patterns.len() > MAX_WORD_SYLLABLES {
            return Vec::new();
        }
        let ids: Vec<Vec<u32>> = patterns.iter().map(pattern_ids).collect();
        if ids.iter().any(Vec::is_empty) {
            return Vec::new();
        }
        let capacity = limit.min(MAX_SPAN_MATCHES);
        let mut top = TopK::new(capacity);
        // Within one span the table's own order (weight, then rank) is the
        // pre-selection key, so the ranking tuple is used with every
        // structural field tied.
        self.collect_exact(&ids, 0, 0..self.keys.len(), &mut |packed, key_syllables, row, ordinal| {
            top.offer(Scored {
                row,
                packed,
                exact: true,
                syllable_gap: 0,
                coverage: 0,
                abbreviated: 0,
                full_last: true,
                canonical_reading: key_syllables == patterns.len(),
                ordinal,
            });
        });
        let mut scored = top.into_sorted();
        let mut seen_text: HashSet<&str> = HashSet::new();
        scored.retain(|item| seen_text.insert(item.row.text.as_str()));
        scored.truncate(capacity);
        scored.into_iter().map(materialize).collect()
    }

    /// Visit every key in `range` that matches `patterns` from `depth` on.
    /// Longer keys match as prefixes of the query (the range bounds already
    /// exclude shorter ones).
    fn collect<'a>(
        &'a self,
        patterns: &[Vec<u32>],
        depth: usize,
        prefix: u128,
        range: std::ops::Range<usize>,
        exact_only: bool,
        visit: &mut dyn FnMut(u128, usize, &'a Row, usize),
    ) {
        if depth == patterns.len() {
            self.visit_range(range, exact_only, patterns.len(), visit);
            return;
        }
        let shift = syllables::shift_for(depth);
        for &id in &patterns[depth] {
            // The bound covers the positions matched so far, so a 简拼
            // position can match any syllable starting with its letters.
            let low = prefix | (u128::from(id) << shift);
            let high = low + (1u128 << shift);
            let start = self.lower_bound(low, range.clone());
            let end = self.upper_bound(high, start..range.end);
            if start < end {
                self.collect(patterns, depth + 1, low, start..end, exact_only, visit);
            }
        }
    }

    /// Same as [`Self::collect`] but only keys with exactly `patterns.len()`
    /// syllables.
    fn collect_exact<'a>(
        &'a self,
        patterns: &[Vec<u32>],
        depth: usize,
        range: std::ops::Range<usize>,
        visit: &mut dyn FnMut(u128, usize, &'a Row, usize),
    ) {
        self.collect(patterns, depth, 0, range, true, visit);
    }

    fn visit_range<'a>(
        &'a self,
        range: std::ops::Range<usize>,
        exact_only: bool,
        depth: usize,
        visit: &mut dyn FnMut(u128, usize, &'a Row, usize),
    ) {
        for key_index in range {
            let key = &self.keys[key_index];
            let key_syllables = key_depth(key.packed);
            if exact_only && key_syllables != depth {
                continue;
            }
            let start = key.start as usize;
            for (offset, slot) in self.slots[start..start + key.count as usize].iter().enumerate() {
                let row = &self.rows[*slot as usize];
                visit(key.packed, key_syllables, row, offset + 1);
            }
        }
    }

    fn lower_bound(&self, packed: u128, range: std::ops::Range<usize>) -> usize {
        let mut start = range.start;
        let mut end = range.end;
        while start < end {
            let mid = start + (end - start) / 2;
            if self.keys[mid].packed < packed {
                start = mid + 1;
            } else {
                end = mid;
            }
        }
        start
    }

    fn upper_bound(&self, packed: u128, range: std::ops::Range<usize>) -> usize {
        let mut start = range.start;
        let mut end = range.end;
        while start < end {
            let mid = start + (end - start) / 2;
            if self.keys[mid].packed < packed {
                start = mid + 1;
            } else {
                end = mid;
            }
        }
        start
    }
}

/// How many trailing syllable positions a prefix-word lookup enumerates.
const PREFIX_LOOKBEHIND: usize = 6;

/// Turn one collected hit into the public match (display split included).
fn materialize(item: Scored<'_>) -> PinyinMatch {
    PinyinMatch {
        text: item.row.text.clone(),
        key: split_of(item.packed),
        code: item.row.code.clone(),
        exact: item.exact,
        syllable_gap: item.syllable_gap,
        coverage: item.coverage,
        weight: item.row.weight,
        rank: item.row.rank,
        ordinal: item.ordinal,
    }
}

/// The syllable split a packed key stands for (`ni hao`).
#[must_use]
pub fn split_of(packed: u128) -> String {
    let mut parts: Vec<&'static str> = Vec::new();
    for depth in 0..MAX_WORD_SYLLABLES {
        match syllables::syllable_of(syllables::id_at(packed, depth)) {
            Some(text) => parts.push(text),
            None => break,
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    parts.join(" ")
}

/// Number of syllables a packed key holds.
fn key_depth(packed: u128) -> usize {
    (0..MAX_WORD_SYLLABLES)
        .take_while(|&depth| syllables::id_at(packed, depth) != 0)
        .count()
}

/// The syllable ids a query position accepts.
fn pattern_ids(pattern: &Pattern) -> Vec<u32> {
    if pattern.complete {
        return syllables::syllable_id(&pattern.canonical).into_iter().collect();
    }
    ids_with_prefix(&pattern.canonical)
}

/// Every syllable id whose spelling starts with `prefix`, ascending.
fn ids_with_prefix(prefix: &str) -> Vec<u32> {
    static PREFIXES: OnceLock<HashMap<&'static str, Vec<u32>>> = OnceLock::new();
    let map = PREFIXES.get_or_init(|| {
        let mut map: HashMap<&'static str, Vec<u32>> = HashMap::new();
        for (index, syllable) in syllables::ALL.iter().enumerate() {
            for end in 1..=syllable.len() {
                let prefix = &syllable[..end];
                if end == syllable.len() {
                    // A complete spelling is not a prefix match; the complete
                    // branch handles it (and would otherwise match `fan` by
                    // `fa` as well as by `fan`).
                    continue;
                }
                map.entry(prefix).or_default().push(index as u32 + 1);
            }
        }
        map
    });
    map.get(prefix).cloned().unwrap_or_default()
}

/// A stable identity for one segmentation (its text pieces and completeness),
/// so two spellings that read the same are looked up once.
fn form_signature(form: &[Pattern]) -> Vec<u32> {
    let mut signature = Vec::with_capacity(form.len() * 2);
    for pattern in form {
        signature.push(syllables::syllable_id(&pattern.text).unwrap_or(u32::MAX));
        signature.push(u32::from(pattern.complete));
    }
    signature
}

/// Non-final positions spelled as 简拼 or an unfinished syllable: the fewer,
/// the closer the reading is to what was typed.
fn abbreviated_count(form: &[Pattern]) -> usize {
    if form.len() <= 1 {
        return 0;
    }
    form[..form.len() - 1].iter().filter(|pattern| !pattern.complete).count()
}

/// Whether the last position is a complete syllable (so a word matching it
/// spells the tail in full).
fn full_last(form: &[Pattern]) -> bool {
    form.last().is_some_and(|pattern| pattern.complete)
}

/// Every complete syllable reading of a code, packed, longest syllables
/// first (`xian` → `xian`, `xi an`).
fn readings(code: &str) -> Vec<u128> {
    syllables::syllabifications(code, MAX_READINGS)
        .into_iter()
        .filter(|form| form.len() <= MAX_WORD_SYLLABLES)
        .map(|form| {
            let parts: Vec<&str> = form.iter().map(String::as_str).collect();
            syllables::pack_ids(&parts)
        })
        .filter(|packed| *packed != 0)
        .collect()
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
    fn initials_reach_words_they_abbreviate() {
        let lex = PinyinLexicon::parse(
            "开发\tkaifa\t100\n\
             开放\tkaifang\t90\n\
             开\tkai\t80\n\
             女孩\tnvhai\t100\n\
             你好\tnihao\t200\n",
        );
        let list = lex.lookup("kf", 5);
        assert_eq!(texts(&list), vec!["开发", "开放", "开"], "{list:?}");
        assert_eq!(list[0].key, "kai fa");

        // `nh` abbreviates both `ni hao` and `nv hai`; the more frequent one
        // leads.
        let list = lex.lookup("nh", 5);
        assert_eq!(texts(&list), vec!["你好", "女孩"], "{list:?}");
        assert!(list.iter().all(|hit| !hit.exact), "简拼 never equals the code");

        // A 简拼 position mixed with a full syllable. `nhao` also admits
        // reading `n h` as an abbreviated prefix of `nv hai`, so 女孩 stays
        // behind the word that covers the whole query.
        let list = lex.lookup("nhao", 3);
        assert_eq!(texts(&list)[0], "你好", "{list:?}");
        assert_eq!(list[0].key, "ni hao");
        assert_eq!(list[0].coverage, 4, "the whole query");
    }

    #[test]
    fn a_word_whose_code_is_missing_still_reads_syllable_by_syllable() {
        // The table has no `huangzhao`; the index reaches 兆 / 照 under `zhao`
        // only when the query names them, which is what the sentence decoder
        // does with the same key set.
        let lex = PinyinLexicon::parse("黄\thuang\t100\n兆\tzhao\t50\n照\tzhao\t40\n");
        let patterns = [
            Pattern { text: "huang".into(), canonical: "huang".into(), complete: true },
            Pattern { text: "zhao".into(), canonical: "zhao".into(), complete: true },
        ];
        let spans = lex.exact_matches(&patterns, 8);
        assert!(spans.is_empty(), "the span has two syllables, no word fills it");
        let single = lex.exact_matches(&patterns[1..], 8);
        assert_eq!(texts(&single), vec!["兆", "照"]);
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
        // 200 two-syllable words under distinct `ni …` keys with increasing
        // weight; only the last few are worth returning. The heap capacity
        // is 4 * limit, so eviction has to keep the best, not the newest.
        let mut rows = String::new();
        let mut used = HashSet::new();
        let mut count = 0u32;
        for syllable in syllables::ALL {
            let code = format!("ni{syllable}");
            let reads_as_two = syllables::syllabifications(&code, 1)
                .first()
                .is_some_and(|form| form.len() == 2 && form[0] == "ni");
            if !reads_as_two || !used.insert(code.clone()) {
                continue;
            }
            rows.push_str(&format!("词{count}\t{code}\t{count}\n"));
            count += 1;
            if count == 200 {
                break;
            }
        }
        assert_eq!(count, 200, "the syllable table yields 200 distinct keys");
        let lex = PinyinLexicon::parse(&rows);
        let list = lex.lookup("ni", 2);
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
        // 简拼 over the real table: the initials reach the words.
        assert!(texts(&lex.lookup("nh", 10)).contains(&"你好"), "nh reaches 你好");
        assert!(texts(&lex.lookup("kf", 10)).contains(&"开发"), "kf reaches 开发");
    }
}
