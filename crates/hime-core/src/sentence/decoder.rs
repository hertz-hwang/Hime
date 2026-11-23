//! Whole-sentence beam search (port of Fire's `SentenceDecoder`).
//!
//! The key stream is a chain of shape-code words. A *word graph* (`lexicon`)
//! says which words start at each position; this decoder walks the stream left
//! to right, keeping at each position a *beam* of candidate prefixes scored by
//! the [`crate::ngram`] model, and returns the highest-scoring complete
//! sentences.
//!
//! Faithful to Fire's deterministic core, which is what 形码整句 is:
//!
//! - same-text states merge their probability mass (`massScore`) by
//!   log-sum-exp, while the kept representative is chosen by max-rank, then
//!   score, then fewer edges;
//! - each position is de-duplicated and truncated to the beam width before it
//!   is expanded;
//! - a whole-input single edge (the key stream is exactly one lexicon entry)
//!   keeps lexicon order; only multi-word segmentations are re-ranked by score;
//! - candidates are scored to the end with an EOS transition.
//!
//! Deferred, exactly as gated off in Fire when there is no user data (each is
//! an additive score term that reduces to these code paths when inactive): the
//! learning channels (session cache / user n-gram / correction), the
//! Aho-Corasick supplement, the per-session incremental lattice (latency only
//! — every call rebuilds from scratch here), pinyin, and the probabilistic
//! early-commit overlay. The score-`massScore` split is kept: `massScore`
//! drops only the whole-code single-character reward and drives the
//! (deferred) auto-commit confidence, so it stays a calibrated probability.

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use super::config::*;
use super::lexicon::{SentenceEdge, SentenceLexicon};
use crate::ngram::{NgramModel, CP_BOS, CP_EOS};

/// One whole-sentence candidate. `segmented` is the ␣-joined code split of the
/// winning path (used for display and, later, learning); `confidence` is the
/// `massScore`-based score the early-commit overlay would consume.
#[derive(Debug, Clone, PartialEq)]
pub struct SentenceCandidate {
    /// The sentence text to commit.
    pub text: String,
    /// The code split behind `text`, e.g. `jin tian tian qi`.
    pub segmented: String,
    /// Final score including the EOS transition (drives candidate ordering).
    pub score: f64,
    /// Probability mass, score minus the whole-code single-character reward.
    pub confidence: f64,
    /// Highest edge rank on the path (1 = every word was the table's first).
    pub max_rank: u32,
    /// Number of word edges on the path.
    pub edge_count: u32,
}

/// A beam-search state: one sentence prefix covering the first `raw_length`
/// keys. States form a parent chain (`previous`); `mass` is the only field
/// that mutates after creation (it is merged during aggregation).
struct SentenceState {
    score: f64,
    mass: Cell<f64>,
    text: String,
    prev2: u32,
    prev1: u32,
    max_rank: u32,
    previous: Option<Rc<SentenceState>>,
    raw_length: usize,
    edge_count: u32,
}

/// A beam bucket: candidate states that end at one code position. States are
/// kept loose until the bucket grows, then frozen one-per-text; `truncated`
/// records that a de-dup dropped states (it invalidates early-commit).
#[derive(Default)]
struct Bucket {
    items: Vec<Rc<SentenceState>>,
    aggregated: bool,
    truncated: bool,
}

/// `log(exp(a) + exp(b))`, computed around the larger term to stay finite.
fn logsumexp(left: f64, right: f64) -> f64 {
    let maximum = left.max(right);
    if maximum == f64::NEG_INFINITY {
        return f64::NEG_INFINITY;
    }
    maximum + ((left - maximum).exp() + (right - maximum).exp()).ln()
}

/// Kept-representative rule for two same-text states: lower max-rank wins, then
/// higher score, then fewer edges (Fire `duplicateBetter`).
fn duplicate_better(item: &SentenceState, previous: &SentenceState) -> bool {
    if item.max_rank != previous.max_rank {
        return item.max_rank < previous.max_rank;
    }
    if item.score != previous.score {
        return item.score > previous.score;
    }
    item.edge_count < previous.edge_count
}

/// Score-first ordering (segmented paths): score, then max-rank, then text.
/// (Fire's state-level rank-first comparator is unused even there; emit ranks
/// whole-input single edges via the completed candidate comparator instead.)
fn state_better_score_first(a: &Rc<SentenceState>, b: &Rc<SentenceState>) -> bool {
    if a.score == b.score {
        if a.max_rank != b.max_rank {
            return a.max_rank < b.max_rank;
        }
        return a.text < b.text;
    }
    a.score > b.score
}

/// `better` as an `Ordering` for `sort_by` (descending preference, stable on ties).
fn cmp_by<T>(better: &impl Fn(&T, &T) -> bool, a: &T, b: &T) -> Ordering {
    if better(a, b) {
        Ordering::Less
    } else if better(b, a) {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

fn sort_by_better<T>(values: &mut [T], better: &impl Fn(&T, &T) -> bool) {
    values.sort_by(|a, b| cmp_by(better, a, b));
}

/// Keep the `limit` best of `values` (a min-heap keyed on `better`, so the root
/// is the worst item kept), returned best-first. Port of `selectExactTop`.
fn select_exact_top<T>(
    values: Vec<T>,
    limit: usize,
    better: &impl Fn(&T, &T) -> bool,
) -> Vec<T> {
    if limit == 0 {
        return Vec::new();
    }
    let mut heap: Vec<T> = Vec::with_capacity(limit.min(values.len()));
    for item in values {
        if heap.len() < limit {
            heap.push(item);
            let last = heap.len() - 1;
            heap_sift_up(&mut heap, last, better);
        } else if better(&item, &heap[0]) {
            heap[0] = item;
            heap_sift_down(&mut heap, 0, better);
        }
    }
    sort_by_better(&mut heap, better);
    heap
}

fn heap_sift_up<T>(heap: &mut [T], start: usize, better: &impl Fn(&T, &T) -> bool) {
    let mut index = start;
    while index > 0 {
        let parent = (index - 1) / 2;
        if better(&heap[parent], &heap[index]) {
            heap.swap(parent, index);
            index = parent;
        } else {
            break;
        }
    }
}

fn heap_sift_down<T>(heap: &mut [T], start: usize, better: &impl Fn(&T, &T) -> bool) {
    let mut index = start;
    loop {
        let left = index * 2 + 1;
        if left >= heap.len() {
            return;
        }
        let mut worse = left;
        let right = left + 1;
        if right < heap.len() && better(&heap[left], &heap[right]) {
            worse = right;
        }
        if better(&heap[index], &heap[worse]) {
            heap.swap(index, worse);
            index = worse;
        } else {
            return;
        }
    }
}

/// A complete sentence scored to the end (with EOS), before it becomes a
/// [`SentenceCandidate`].
struct Completed {
    score: f64,
    confidence: f64,
    text: String,
    max_rank: u32,
    edge_count: u32,
    segmented: String,
    /// Whether the path has more than one word (a real segmentation).
    segmented_path: bool,
}

fn completed_better_score_first(a: &Completed, b: &Completed) -> bool {
    if a.score == b.score {
        if a.max_rank != b.max_rank {
            return a.max_rank < b.max_rank;
        }
        return a.text < b.text;
    }
    a.score > b.score
}

fn completed_better_rank_first(a: &Completed, b: &Completed) -> bool {
    if a.max_rank != b.max_rank {
        return a.max_rank < b.max_rank;
    }
    if a.score == b.score {
        return a.text < b.text;
    }
    a.score > b.score
}

impl Bucket {
    fn append(&mut self, item: Rc<SentenceState>) {
        if self.aggregated {
            self.add_aggregated(&item);
            return;
        }
        self.items.push(item);
        if self.items.len() >= AGGREGATE_DURING_EXPANSION_THRESHOLD {
            self.aggregate();
        }
    }

    /// Merge an item into an already-aggregated bucket (one state per text).
    fn add_aggregated(&mut self, item: &Rc<SentenceState>) {
        let existing = self.items.iter().position(|s| s.text == item.text);
        if let Some(index) = existing {
            let combined = logsumexp(self.items[index].mass.get(), item.mass.get());
            if duplicate_better(item, &self.items[index]) {
                item.mass.set(combined);
                self.items[index] = Rc::clone(item);
            } else {
                self.items[index].mass.set(combined);
            }
        } else {
            self.items.push(Rc::clone(item));
        }
    }

    /// Collapse loose states to one per text: mass is combined by log-sum-exp,
    /// the representative is the best per [`duplicate_better`], in first-seen
    /// text order.
    fn aggregate(&mut self) {
        if self.aggregated {
            return;
        }
        let mut order: Vec<String> = Vec::new();
        let mut best: HashMap<&str, Rc<SentenceState>> = HashMap::new();
        let mut mass: HashMap<&str, f64> = HashMap::new();
        for item in &self.items {
            let text = item.text.as_str();
            if let Some(m) = mass.get_mut(text) {
                *m = logsumexp(*m, item.mass.get());
                if duplicate_better(item, best[text].as_ref()) {
                    best.insert(text, Rc::clone(item));
                }
            } else {
                mass.insert(text, item.mass.get());
                order.push(item.text.clone());
                best.insert(text, Rc::clone(item));
            }
        }
        let mut merged: Vec<Rc<SentenceState>> = Vec::with_capacity(order.len());
        for text in &order {
            let item = &best[text.as_str()];
            item.mass.set(mass[text.as_str()]);
            merged.push(Rc::clone(item));
        }
        self.items = merged;
        self.aggregated = true;
    }

    /// Aggregate-if-needed and truncate to `limit`, returning a new frozen
    /// bucket sorted best-first (port of `dedup`).
    fn dedup(&self, limit: usize, better: &impl Fn(&Rc<SentenceState>, &Rc<SentenceState>) -> bool) -> Bucket {
        let mut truncated = self.truncated;
        let mut result = self.items.clone();
        if result.len() > limit {
            truncated = true;
            result = select_exact_top(result, limit, better);
        }
        let mut output = Bucket { items: result, aggregated: false, truncated };
        if self.aggregated {
            output.aggregated = true;
        } else {
            output.aggregate();
        }
        sort_by_better(&mut output.items, better);
        output
    }
}

/// Parse the rank-selection marker after a code edge. `;`=2nd, `'`=3rd,
/// digits=that rank (`0`=10, digits may run, e.g. `12` = 12th). Consumes the
/// marker bytes, returning `(rank, end)`. `rank == 0` means none.
fn parse_selector(raw: &[u8], code_end: usize) -> (u32, usize) {
    if code_end >= raw.len() {
        return (0, code_end);
    }
    let mark = raw[code_end];
    if mark == b';' {
        return (2, code_end + 1);
    }
    if mark == b'\'' {
        return (3, code_end + 1);
    }
    if mark.is_ascii_digit() {
        let mut end = code_end;
        while end < raw.len() && raw[end].is_ascii_digit() {
            end += 1;
        }
        let token = &raw[code_end..end];
        if token == b"0" {
            return (10, end);
        }
        let rank = std::str::from_utf8(token).ok().and_then(|s| s.parse::<u32>().ok());
        return (rank.unwrap_or(0), end);
    }
    (0, code_end)
}

/// Which edges under a code may be used at this point (port of `eligibleEdges`).
/// An explicit rank takes that edge only; otherwise a whole-input edge may use
/// every rank, and a mid-stream edge uses rank 1 (plus single-character
/// homophones when duplicate-single is on).
fn eligible_edges(
    edges: &[SentenceEdge],
    selected_rank: u32,
    whole_input_edge: bool,
    allow_duplicate_single: bool,
) -> Vec<&SentenceEdge> {
    if selected_rank > 0 {
        return edges.iter().filter(|e| u32::from(e.rank) == selected_rank).collect();
    }
    if whole_input_edge {
        return edges.iter().collect();
    }
    if allow_duplicate_single {
        return edges.iter().filter(|e| e.rank == 1 || e.text.chars().count() == 1).collect();
    }
    edges.iter().filter(|e| e.rank == 1).collect()
}

/// A decode needs a model and a graph; this bundles the two borrows so the
/// recursive-free helpers do not thread them separately.
struct Decoder<'a> {
    model: &'a NgramModel,
    lexicon: &'a SentenceLexicon,
}

impl<'a> Decoder<'a> {
    fn expand(
        &self,
        raw: &[u8],
        buckets: &mut [Option<Bucket>],
        from: usize,
        length: usize,
        allow_duplicate_single: bool,
    ) {
        for position in from..length {
            let current = match buckets[position].take() {
                Some(bucket) => bucket,
                None => continue,
            };
            let current = current.dedup(beam_limit_at(position), &state_better_score_first);
            let states = current.items.clone();
            buckets[position] = Some(current);
            if states.is_empty() {
                continue;
            }

            for code_length in self.lexicon.lengths() {
                let end = position + code_length;
                if end > length {
                    continue;
                }
                let (rank, consumed_end) = parse_selector(raw, end);
                if consumed_end > length {
                    continue;
                }
                // Short-edge gate: for any multi-key input, an edge must consume
                // at least two keys (the selector counts toward the span).
                if length > 1 && consumed_end - position < 2 {
                    continue;
                }
                let Ok(code) = std::str::from_utf8(&raw[position..end]) else {
                    continue;
                };
                let Some(edges) = self.lexicon.edges(code) else {
                    continue;
                };
                let whole_input_edge = position == 0 && consumed_end == length;
                let selected =
                    eligible_edges(edges, rank, whole_input_edge, allow_duplicate_single);
                if selected.is_empty() {
                    continue;
                }

                for item in &states {
                    for edge in &selected {
                        let state = self.follow(item, edge, rank, whole_input_edge, consumed_end);
                        buckets[consumed_end]
                            .get_or_insert_with(Bucket::default)
                            .append(state);
                    }
                }
            }
        }
    }

    /// Extend `item` by one edge: walk its characters through the model, add the
    /// emitted-character reward and (when no rank was forced) the light rank
    /// penalty, apply the whole-code optimal-single reward, and carry the
    /// probability mass separately from the score.
    fn follow(
        &self,
        item: &Rc<SentenceState>,
        edge: &SentenceEdge,
        selected_rank: u32,
        whole_input_edge: bool,
        consumed_end: usize,
    ) -> Rc<SentenceState> {
        let mut score = item.score;
        let mut prev2 = item.prev2;
        let mut prev1 = item.prev1;
        let mut char_count = 0usize;
        for ch in edge.text.chars() {
            let target = ch as u32;
            score += self.model.logp(prev2, prev1, target);
            score += EMITTED_CHARACTER_REWARD;
            prev2 = prev1;
            prev1 = target;
            char_count += 1;
        }
        if selected_rank == 0 {
            // A non-first rank is mildly discouraged, but only when the user did
            // not explicitly pick it.
            score -= RANK_PENALTY * f64::from(u32::from(edge.rank)).ln();
        }
        // `mass` tracks probability: it keeps the rank penalty but drops the
        // whole-code single-character reward, so it stays a calibrated score.
        let mut mass_delta = score - item.score;
        if whole_input_edge && selected_rank == 0 && edge.optimal_single && char_count == 1 {
            score += WHOLE_INPUT_SINGLE_CHARACTER_REWARD;
            mass_delta = score - WHOLE_INPUT_SINGLE_CHARACTER_REWARD - item.score;
        }
        Rc::new(SentenceState {
            score,
            mass: Cell::new(item.mass.get() + mass_delta),
            text: format!("{}{}", item.text, edge.text),
            prev2,
            prev1,
            max_rank: item.max_rank.max(u32::from(edge.rank)),
            previous: Some(Rc::clone(item)),
            raw_length: consumed_end,
            edge_count: item.edge_count + 1,
        })
    }

    fn build_buckets(
        &self,
        raw: &[u8],
        allow_duplicate_single: bool,
        context: &str,
    ) -> Vec<Option<Bucket>> {
        let length = raw.len();
        // Left context only seeds the model history (the last ≤2 chars as
        // prev2/prev1); it is never scored or shown.
        let scalars: Vec<u32> = context.chars().map(|c| c as u32).collect();
        let mut prev2 = CP_BOS;
        let mut prev1 = CP_BOS;
        for &scalar in scalars.iter().rev().take(2).rev() {
            prev2 = prev1;
            prev1 = scalar;
        }

        let mut buckets: Vec<Option<Bucket>> = (0..=length).map(|_| None).collect();
        let mut root = Bucket::default();
        root.append(Rc::new(SentenceState {
            score: 0.0,
            mass: Cell::new(0.0),
            text: String::new(),
            prev2,
            prev1,
            max_rank: 1,
            previous: None,
            raw_length: 0,
            edge_count: 0,
        }));
        buckets[0] = Some(root);
        for slot in buckets.iter_mut().skip(1) {
            *slot = Some(Bucket::default());
        }
        self.expand(raw, &mut buckets, 0, length, allow_duplicate_single);
        buckets
    }

    /// Score the end bucket, choose lexicon-order vs score-order, and return the
    /// top candidates.
    fn emit(
        &self,
        raw: &[u8],
        buckets: &mut [Option<Bucket>],
        length: usize,
        allow_duplicate_single: bool,
    ) -> Vec<SentenceCandidate> {
        let base = buckets[length].take().unwrap_or_default();
        let completed = base.dedup(beam_limit_at(length), &state_better_score_first);

        let mut all: Vec<Completed> = Vec::with_capacity(completed.items.len());
        for state in &completed.items {
            let ending = self.model.logp(state.prev2, state.prev1, CP_EOS);
            let segmented_path =
                state.previous.as_ref().is_some_and(|p| p.raw_length > 0);
            all.push(Completed {
                score: state.score + ending,
                confidence: state.mass.get() + ending,
                text: state.text.clone(),
                max_rank: state.max_rank.max(1),
                edge_count: state.edge_count,
                segmented: segmented_from_path(state, raw),
                segmented_path,
            });
        }

        // A whole-input single edge is ordered by the table (the lexicon owns
        // "code → first choice"); only when a single-character-homophone path
        // could also read the keys as a multi-word sentence do the candidates
        // compete by score.
        let has_segmented_path = all.iter().any(|c| c.segmented_path);
        let use_score = allow_duplicate_single && has_segmented_path;
        let mut result = all;
        if use_score {
            if result.len() > CANDIDATE_LIMIT {
                result = select_exact_top(result, CANDIDATE_LIMIT, &completed_better_score_first);
            } else {
                sort_by_better(&mut result, &completed_better_score_first);
            }
        } else if result.len() > CANDIDATE_LIMIT {
            result = select_exact_top(result, CANDIDATE_LIMIT, &completed_better_rank_first);
        } else {
            sort_by_better(&mut result, &completed_better_rank_first);
        }

        result
            .into_iter()
            .map(|c| SentenceCandidate {
                text: c.text,
                segmented: c.segmented,
                score: c.score,
                confidence: c.confidence,
                max_rank: c.max_rank,
                edge_count: c.edge_count,
            })
            .collect()
    }
}

/// ␣-joined code split behind a path (port of `segmentedFromPath`).
fn segmented_from_path(path: &Rc<SentenceState>, raw: &[u8]) -> String {
    let mut ends: Vec<usize> = Vec::new();
    let mut node = Some(Rc::clone(path));
    while let Some(state) = node {
        if state.raw_length == 0 {
            break;
        }
        ends.push(state.raw_length);
        node = state.previous.clone();
    }
    let mut pieces: Vec<&str> = Vec::with_capacity(ends.len());
    let mut start = 0;
    for finish in ends.iter().rev() {
        pieces.push(std::str::from_utf8(&raw[start..*finish]).unwrap_or(""));
        start = *finish;
    }
    pieces.join(" ")
}

/// Keep only the bytes a sentence code may contain: ASCII letters (case kept),
/// `;`, `'`, and digits; spaces and tabs are dropped; any other character makes
/// the whole input non-sentence (`None`).
fn normalize(raw: &str) -> Option<Vec<u8>> {
    let mut bytes: Vec<u8> = Vec::with_capacity(raw.len());
    for ch in raw.chars() {
        let value = ch as u32;
        if value == 0x20 || value == 0x09 {
            continue;
        }
        let allowed = (0x41..=0x5A).contains(&value) // A-Z
            || (0x61..=0x7A).contains(&value) // a-z
            || value == 0x3B
            || value == 0x27 // ; '
            || (0x30..=0x39).contains(&value); // 0-9
        if !allowed {
            return None;
        }
        bytes.push(value as u8);
    }
    Some(bytes)
}

/// Whether the stream holds at least one letter (a bare `123;` is not a code).
fn has_letter(raw: &[u8]) -> bool {
    raw.iter().any(|b| b.is_ascii_alphabetic())
}

/// Stateless handle over the beam search. It holds no data; the model and graph
/// are passed per call so a single value is safe to share across threads
/// (AGENTS.md §1).
#[derive(Debug, Default, Clone, Copy)]
pub struct SentenceDecoder;

impl SentenceDecoder {
    /// Decode `raw_code` into whole-sentence candidates, best first. `context`
    /// is text left of the cursor (used only as model history);
    /// `allow_duplicate_single` mirrors Fire's "单字重码组句" (on by default).
    ///
    /// Returns empty for an empty / letter-less / over-long input or one holding
    /// a character a sentence code cannot contain.
    #[must_use]
    pub fn decode(
        model: &NgramModel,
        lexicon: &SentenceLexicon,
        raw_code: &str,
        context: &str,
        allow_duplicate_single: bool,
    ) -> Vec<SentenceCandidate> {
        let Some(raw) = normalize(raw_code) else {
            return Vec::new();
        };
        if raw.is_empty() || !has_letter(&raw) || raw.len() > MAX_RAW_LENGTH {
            return Vec::new();
        }
        let length = raw.len();
        let decoder = Decoder { model, lexicon };
        let mut buckets = decoder.build_buckets(&raw, allow_duplicate_single, context);
        decoder.emit(&raw, &mut buckets, length, allow_duplicate_single)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(text: &str, score: f64, max_rank: u32, edge_count: u32, raw_length: usize) -> Rc<SentenceState> {
        Rc::new(SentenceState {
            score,
            mass: Cell::new(score),
            text: text.into(),
            prev2: CP_BOS,
            prev1: CP_BOS,
            max_rank,
            previous: None,
            raw_length,
            edge_count,
        })
    }

    fn edge(text: &str, rank: u16) -> SentenceEdge {
        SentenceEdge { text: text.into(), rank, optimal_single: false }
    }

    #[test]
    fn logsumexp_matches_the_sum_of_exponentials() {
        let got = logsumexp(0.0, 0.0);
        assert!((got - 2f64.ln()).abs() < 1e-12);
        // Dominated by the larger term when they are far apart.
        let got = logsumexp(100.0, 0.0);
        assert!((got - 100.0).abs() < 1e-6);
    }

    #[test]
    fn selectors_parse_markers_and_digit_runs() {
        let raw = b"ab;12c'9";
        assert_eq!(parse_selector(raw, 2), (2, 3)); // ';' → rank 2
        assert_eq!(parse_selector(raw, 3), (12, 5)); // "12" → rank 12
        assert_eq!(parse_selector(raw, 5), (0, 5)); // 'c' is not a marker
        assert_eq!(parse_selector(raw, 6), (3, 7)); // '\'' → rank 3
        assert_eq!(parse_selector(raw, 7), (9, 8)); // '9' → rank 9
        // "0" means the 10th.
        assert_eq!(parse_selector(b"x0", 1), (10, 2));
    }

    #[test]
    fn normalize_keeps_letters_and_selectors_drops_blanks() {
        assert_eq!(normalize("Ab;c 12\t9").unwrap(), b"Ab;c129");
        // Any other character disqualifies the whole input.
        assert!(normalize("好abc").is_none());
        assert!(normalize("a,b").is_none());
    }

    #[test]
    fn eligible_edges_apply_rank_and_single_rules() {
        let edges = [edge("第一", 1), edge("弟", 2), edge("敌", 3)];
        // Explicit rank selects that edge only.
        assert_eq!(eligible_edges(&edges, 2, false, true).len(), 1);
        // Whole-input uses every rank.
        assert_eq!(eligible_edges(&edges, 0, true, false).len(), 3);
        // Mid-stream with duplicate-single off: rank 1 only.
        assert_eq!(eligible_edges(&edges, 0, false, false).len(), 1);
        // With it on: rank 1 plus single-character edges (弟/敌 are single).
        assert_eq!(eligible_edges(&edges, 0, false, true).len(), 3);
    }

    #[test]
    fn select_exact_top_keeps_best_first() {
        let values = vec![3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0];
        let top = select_exact_top(values, 3, &|a, b| a > b);
        assert_eq!(top, vec![9.0, 6.0, 5.0]);
    }

    #[test]
    fn bucket_aggregates_same_text_and_merges_mass() {
        let mut bucket = Bucket::default();
        bucket.append(state("北京", 1.0, 1, 1, 2));
        bucket.append(state("北京", 0.5, 1, 2, 2)); // same text, lower score
        bucket.append(state("背景", 2.0, 1, 1, 2));
        bucket.aggregate();
        assert_eq!(bucket.items.len(), 2, "one state per distinct text");
        let beijing = bucket.items.iter().find(|s| s.text == "北京").unwrap();
        // Kept the higher-score representative.
        assert_eq!(beijing.edge_count, 1);
        // Mass merged by log-sum-exp: e^1 + e^0.5.
        let expected = logsumexp(1.0, 0.5);
        assert!((beijing.mass.get() - expected).abs() < 1e-9);
    }

    #[test]
    fn bucket_dedup_truncates_to_beam_and_marks_truncated() {
        let mut bucket = Bucket::default();
        for i in 0..5 {
            bucket.append(state(&format!("字{i}"), i as f64, 1, 1, 2));
        }
        let out = bucket.dedup(2, &state_better_score_first);
        assert!(out.truncated);
        assert_eq!(out.items.len(), 2);
        // Best-first: the two highest scores.
        assert_eq!(out.items[0].score, 4.0);
        assert_eq!(out.items[1].score, 3.0);
    }

    #[test]
    fn segmented_from_path_joins_code_spans() {
        // path: root ← ab(0..2) ← cd(2..4)
        let root = state("", 0.0, 1, 0, 0);
        let mid = SentenceState {
            text: "中".into(),
            raw_length: 2,
            previous: Some(Rc::clone(&root)),
            ..score_state(0.0)
        };
        let tail = SentenceState {
            text: "华".into(),
            raw_length: 4,
            previous: Some(Rc::new(mid)),
            ..score_state(0.0)
        };
        assert_eq!(segmented_from_path(&Rc::new(tail), b"abcd"), "ab cd");
    }

    fn score_state(score: f64) -> SentenceState {
        SentenceState {
            score,
            mass: Cell::new(score),
            text: String::new(),
            prev2: CP_BOS,
            prev1: CP_BOS,
            max_rank: 1,
            previous: None,
            raw_length: 0,
            edge_count: 0,
        }
    }

    #[test]
    fn state_comparator_orders_by_score_then_rank() {
        let low = state("a", 1.0, 1, 1, 1);
        let high = state("b", 5.0, 3, 1, 1);
        assert!(state_better_score_first(&high, &low));
        // Tied score falls back to the lower max-rank.
        let low_rank = state("a", 5.0, 1, 1, 1);
        assert!(state_better_score_first(&low_rank, &high));
    }

    #[test]
    fn completed_comparators_switch_between_score_and_rank() {
        let make = |score: f64, max_rank: u32, text: &str| Completed {
            score,
            confidence: score,
            text: text.into(),
            max_rank,
            edge_count: 1,
            segmented: String::new(),
            segmented_path: true,
        };
        let first_choice = make(1.0, 1, "甲");
        let better_score_worse_rank = make(5.0, 3, "乙");
        // Score-first ranks the higher score ahead.
        assert!(completed_better_score_first(&better_score_worse_rank, &first_choice));
        // Rank-first keeps the lexicon's first choice ahead regardless of score.
        assert!(completed_better_rank_first(&first_choice, &better_score_worse_rank));
    }
}
