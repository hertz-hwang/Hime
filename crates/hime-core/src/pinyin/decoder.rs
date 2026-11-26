//! Whole-sentence pinyin decoding: a word graph over syllable positions.
//!
//! A typed string is split into syllable positions (`huang zhao`), which is
//! the lattice's coordinate system. Every table word covering a run of
//! positions is an edge; a beam search walks the positions left to right and
//! keeps the best paths per "last two characters" context, scored by the same
//! character n-gram the 形码整句 decoder uses.
//!
//! This is what reaches words the table does not contain: `huangzhao` has no
//! row, but `黄` and `照` are both single-character edges, so the chain
//! 黄 + 照 competes with every other reading the model can score.
//!
//! The state is the last two characters, not the previous word, because the
//! scoring transition is a character-level trigram: a word's transfer score is
//! "append these characters to the two characters before it". Two paths ending
//! at the same position with the same last two characters are merged, which is
//! real Viterbi; the beam width is spent only on distinct contexts.
//!
//! Deferred relative to the reference implementation (same deferrals as the
//! shape-code decoder): fuzzy spellings, typo correction, the learning
//! channels and the span cache. Each is an additive score term or a latency
//! optimization; the deterministic ranking below is the reference core.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::ngram::{NgramModel, CP_BOS, CP_EOS};

use super::lexicon::PinyinLexicon;
use super::syllables::{Pattern, MAX_WORD_SYLLABLES};

/// Partial paths kept at each syllable position.
const BEAM_WIDTH: usize = 32;

/// Words kept per span before the model scores them. Homophones are common,
/// so the full set would only widen the beam.
const SPAN_CANDIDATES: usize = 8;

/// Words kept per span that contains a 简拼 position: one initial covers
/// dozens of syllables (`h` → 和 / 好 / 会 / 还 / 很 …), and keeping a few
/// would push out the one the sentence needs.
const ABBREVIATED_SPAN_CANDIDATES: usize = 24;

/// Table words fetched per span before the prior ranks them.
const SPAN_FETCH: usize = 64;

/// Light penalty for a non-first table rank, as in 形码整句.
const RANK_PENALTY: f64 = 0.03;

/// Score for a syllable the table cannot cover at all: the path stays alive
/// with the typed syllable as its text, far behind any real word.
const UNKNOWN_LOG_PROB: f64 = -30.0;

/// One whole-sentence candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct PinyinCandidate {
    /// The sentence text to commit.
    pub text: String,
    /// The syllable split behind `text`, e.g. `huang zhao`.
    pub segmented: String,
    /// Final score including the EOS transition; drives candidate order.
    pub score: f64,
    /// Number of word edges on the path.
    pub word_count: usize,
    /// Number of syllable positions the path covers.
    pub syllable_count: usize,
}

/// A path prefix covering the first `start` positions. Paths form a parent
/// chain through `start` / `back` indices.
#[derive(Debug, Clone)]
struct Node {
    /// Second-to-last character, the model history's older half.
    prev2: u32,
    /// Last character.
    prev1: u32,
    score: f64,
    /// Position this edge started at.
    start: usize,
    /// Index of the predecessor inside `nodes[start]`.
    back: usize,
    /// The word this edge emits; `None` for the root.
    word: Option<Word>,
}

/// One word on a path.
#[derive(Debug, Clone)]
struct Word {
    text: String,
    /// Syllable split of this word (`ni hao`).
    split: String,
}

/// A word available inside one span, with its context-free score.
#[derive(Debug, Clone)]
struct SpanWord {
    text: String,
    split: String,
    scalars: Vec<u32>,
    /// Score of this word's third character onward: independent of context.
    tail: f64,
    /// Context-free prior: the unigram chain from BOS, minus the rank penalty.
    prior: f64,
    /// 1-based rank inside its syllable key.
    ordinal: u32,
}

impl SpanWord {
    fn into_word(self) -> Word {
        Word { text: self.text, split: self.split }
    }
}

/// Whole-sentence candidates for one syllable reading of `code`, best first.
///
/// `patterns` is a complete segmentation of the query; every position is a
/// complete syllable, a 简拼 initial, or (last position only) an unfinished
/// syllable. `context` is the committed text left of the cursor, used only as
/// model history.
#[must_use]
pub fn decode(
    model: &NgramModel,
    lexicon: &PinyinLexicon,
    patterns: &[Pattern],
    context: &str,
    limit: usize,
) -> Vec<PinyinCandidate> {
    let length = patterns.len();
    if length == 0 || limit == 0 {
        return Vec::new();
    }

    let (prev2, prev1) = context_tail(context);
    let mut nodes: Vec<Vec<Node>> = (0..=length).map(|_| Vec::new()).collect();
    nodes[0].push(Node { prev2, prev1, score: 0.0, start: 0, back: 0, word: None });

    for start in 0..length {
        prune(&mut nodes[start]);
        if nodes[start].is_empty() {
            continue;
        }
        let furthest = length.min(start + MAX_WORD_SYLLABLES);
        let mut covered = false;
        for end in start + 1..=furthest {
            let hits = span_words(model, lexicon, &patterns[start..end]);
            if hits.is_empty() {
                continue;
            }
            covered = true;
            for hit in &hits {
                // One node per word, hung off the best predecessor. Expanding
                // every predecessor would multiply the beam by the word count
                // for the same contexts `prune` merges anyway.
                let mut best = f64::NEG_INFINITY;
                let mut best_index = 0;
                for (index, previous) in nodes[start].iter().enumerate() {
                    let total =
                        previous.score + transition(model, previous.prev2, previous.prev1, hit);
                    if total > best {
                        best = total;
                        best_index = index;
                    }
                }
                let fallback = nodes[start][best_index].prev1;
                let (prev2, prev1) = tail_context(&hit.scalars, fallback);
                let score = best - f64::from(hit.ordinal.saturating_sub(1)) * RANK_PENALTY;
                nodes[end].push(Node {
                    prev2,
                    prev1,
                    score,
                    start,
                    back: best_index,
                    word: Some(hit.clone().into_word()),
                });
            }
        }

        if !covered {
            // Nothing in the table reads this position: keep the typed
            // syllable as a placeholder so the rest of the sentence decodes.
            let text = patterns[start].text.clone();
            let mut best = f64::NEG_INFINITY;
            let mut best_index = 0;
            for (index, previous) in nodes[start].iter().enumerate() {
                if previous.score > best {
                    best = previous.score;
                    best_index = index;
                }
            }
            let scalars: Vec<u32> = text.chars().map(|c| c as u32).collect();
            let (prev2, prev1) = tail_context(&scalars, nodes[start][best_index].prev1);
            nodes[start + 1].push(Node {
                prev2,
                prev1,
                score: best + UNKNOWN_LOG_PROB,
                start,
                back: best_index,
                word: Some(Word { text, split: patterns[start].text.clone() }),
            });
        }
    }

    prune(&mut nodes[length]);
    let mut paths: Vec<PinyinCandidate> = Vec::with_capacity(nodes[length].len());
    for index in 0..nodes[length].len() {
        let node = &nodes[length][index];
        let words = backtrack(&nodes, length, index);
        let text: String = words.iter().map(|word| word.text.as_str()).collect();
        if text.is_empty() {
            continue;
        }
        paths.push(PinyinCandidate {
            text,
            segmented: words.iter().map(|word| word.split.as_str()).collect::<Vec<_>>().join(" "),
            // The EOS transition is what makes "the sentence ends here" part
            // of the score, the same convention as 形码整句.
            score: node.score + model.logp(node.prev2, node.prev1, CP_EOS),
            word_count: words.len(),
            syllable_count: length,
        });
    }
    paths.sort_by(|left, right| {
        right.score.partial_cmp(&left.score).unwrap_or(Ordering::Equal).then_with(|| {
            right.word_count.cmp(&left.word_count)
        })
    });
    let mut seen: HashSet<String> = HashSet::new();
    paths.retain(|path| seen.insert(path.text.clone()));
    paths.truncate(limit);
    paths
}

/// The word candidates inside one span, best prior first.
fn span_words(
    model: &NgramModel,
    lexicon: &PinyinLexicon,
    span: &[Pattern],
) -> Vec<SpanWord> {
    let abbreviated = span.iter().any(|pattern| !pattern.complete);
    let matches = lexicon.exact_matches(span, SPAN_FETCH);
    let mut scored: Vec<SpanWord> = Vec::with_capacity(matches.len());
    for hit in matches {
        let scalars: Vec<u32> = hit.text.chars().map(|c| c as u32).collect();
        let tail = tail_score(model, &scalars);
        let head = head_score(model, &scalars);
        scored.push(SpanWord {
            prior: head + tail
                - f64::from(hit.ordinal.saturating_sub(1) as u32) * RANK_PENALTY,
            text: hit.text,
            split: hit.key,
            scalars,
            tail,
            ordinal: hit.ordinal as u32,
        });
    }
    scored.sort_by(|left, right| {
        right.prior.partial_cmp(&left.prior).unwrap_or(Ordering::Equal)
    });
    // One text can be reached through several readings; the higher prior wins.
    let mut seen: HashSet<String> = HashSet::new();
    scored.retain(|word| seen.insert(word.text.clone()));
    scored.truncate(if abbreviated { ABBREVIATED_SPAN_CANDIDATES } else { SPAN_CANDIDATES });
    scored
}

/// Merge paths with the same last two characters, then cut to the beam width.
fn prune(nodes: &mut Vec<Node>) {
    let mut best: HashMap<u64, usize> = HashMap::with_capacity(nodes.len());
    let mut kept: Vec<Node> = Vec::with_capacity(nodes.len());
    for node in nodes.drain(..) {
        let context = (u64::from(node.prev2) << 32) | u64::from(node.prev1);
        match best.get(&context) {
            Some(&slot) => {
                if node.score > kept[slot].score {
                    kept[slot] = node;
                }
            }
            None => {
                best.insert(context, kept.len());
                kept.push(node);
            }
        }
    }
    kept.sort_by(|left, right| {
        right.score.partial_cmp(&left.score).unwrap_or(Ordering::Equal)
    });
    kept.truncate(BEAM_WIDTH);
    *nodes = kept;
}

/// Walk the parent chain back to the root.
fn backtrack(nodes: &[Vec<Node>], position: usize, index: usize) -> Vec<Word> {
    let mut words = Vec::new();
    let mut position = position;
    let mut index = index;
    while position > 0 {
        let node = &nodes[position][index];
        if let Some(word) = &node.word {
            words.push(word.clone());
        }
        position = node.start;
        index = node.back;
    }
    words.reverse();
    words
}

/// Score of a word's third character onward: only the first two characters
/// consume the path context.
fn tail_score(model: &NgramModel, scalars: &[u32]) -> f64 {
    scalars
        .windows(3)
        .map(|window| model.logp(window[0], window[1], window[2]))
        .sum()
}

/// Score of appending `word` after `(prev2, prev1)`: its first two characters
/// consume the path context, the rest is context-free.
fn transition(model: &NgramModel, prev2: u32, prev1: u32, word: &SpanWord) -> f64 {
    head_score_with(model, prev2, prev1, &word.scalars) + word.tail
}

/// The unigram chain of a word from `(prev2, prev1)`, first two characters
/// only. With `(CP_BOS, CP_BOS)` this is the word's context-free prior.
fn head_score(model: &NgramModel, scalars: &[u32]) -> f64 {
    head_score_with(model, CP_BOS, CP_BOS, scalars)
}

fn head_score_with(model: &NgramModel, prev2: u32, prev1: u32, scalars: &[u32]) -> f64 {
    let Some(&first) = scalars.first() else { return 0.0 };
    let mut total = model.logp(prev2, prev1, first);
    if let Some(&second) = scalars.get(1) {
        total += model.logp(prev1, first, second);
    }
    total
}

/// The last two characters a path ends with after appending `scalars`. A
/// one-character word shifts the old last character back: `(A, B)` → `(B, C)`.
fn tail_context(scalars: &[u32], fallback: u32) -> (u32, u32) {
    match scalars.len() {
        0 => (fallback, fallback),
        1 => (fallback, scalars[0]),
        _ => (scalars[scalars.len() - 2], scalars[scalars.len() - 1]),
    }
}

/// The `(prev2, prev1)` history a left context seeds the model with.
fn context_tail(context: &str) -> (u32, u32) {
    let scalars: Vec<u32> = context.chars().map(|c| c as u32).collect();
    let mut prev2 = CP_BOS;
    let mut prev1 = CP_BOS;
    for &scalar in scalars.iter().rev().take(2).rev() {
        prev2 = prev1;
        prev1 = scalar;
    }
    (prev2, prev1)
}
