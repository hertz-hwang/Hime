//! Whole-sentence (形码整句) decoding.
//!
//! Port of Fire's `Fire/Sentence/` engine for shape codes. The candidate for a
//! long encoding is a whole sentence, not a single word: the encoder splits the
//! key stream into words (a word graph), and a beam search over that graph
//! picks the chain the n-gram model scores highest.
//!
//! [`lexicon`] builds the word graph (code → ranked edges) from a shape-code
//! table; [`crate::ngram`] scores a character sequence; the beam-search
//! decoder consumes both and is added beside these. A thin engine ties the
//! three together and is the single object the engine and shells talk to.
//!
//! Deferred relative to Fire (documented where each belongs): the learning
//! channels (session cache / user n-gram / correction), the Aho-Corasick
//! weighted-word supplement, the per-session incremental lattice (a latency
//! optimization), pinyin decoding, and the probabilistic early-commit overlay.
//! None of them affect the deterministic whole-sentence ranking that is the
//! core of 形码整句; they are additive score terms gated off when there is no
//! learning or user-word data, exactly as in Fire.

pub mod lexicon;

pub use lexicon::{SentenceEdge, SentenceLexicon};

/// Beam-search and scoring constants, carried over from Fire's `SentenceConfig`
/// for the shape-code (虎整句) path. The decoder consumes them once it lands.
pub(crate) mod config {
    /// Shape-code maximum length per edge (longer codes are table errors).
    pub const MAX_CODE_LENGTH: usize = 8;
}
