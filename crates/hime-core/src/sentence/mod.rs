//! Whole-sentence (形码整句) decoding.
//!
//! Port of Fire's `Fire/Sentence/` engine for shape codes. The candidate for a
//! long encoding is a whole sentence, not a single word: the encoder splits the
//! key stream into words (a word graph), and a beam search over that graph
//! picks the chain the n-gram model scores highest.
//!
//! [`lexicon`] builds the word graph (code → ranked edges) from a shape-code
//! table; [`crate::ngram`] scores a character sequence; [`decoder`] runs the
//! beam search over the two. A thin engine ties the three together and is the
//! single object the engine and shells talk to.
//!
//! Deferred relative to Fire (documented where each belongs): the learning
//! channels (session cache / user n-gram / correction), the Aho-Corasick
//! weighted-word supplement, the per-session incremental lattice (a latency
//! optimization), pinyin decoding, and the probabilistic early-commit overlay.
//! None of them affect the deterministic whole-sentence ranking that is the
//! core of 形码整句; they are additive score terms gated off when there is no
//! learning or user-word data, exactly as in Fire.

pub mod decoder;
pub mod lexicon;

pub use decoder::{SentenceCandidate, SentenceDecoder};
pub use lexicon::{SentenceEdge, SentenceLexicon};

/// Beam-search and scoring constants, carried over from Fire's `SentenceConfig`
/// for the shape-code (虎整句) path. The pinyin-only knobs are omitted because
/// only shape codes are migrated.
pub(crate) mod config {
    /// Beam width at each code position.
    pub const BEAM_WIDTH: usize = 200;
    /// Past this length the beam narrows, which is what keeps long inputs
    /// responsive (decoding cost grows with length).
    pub const LONG_INPUT_FULL_BEAM_LENGTH: usize = 12;
    /// Beam width once past [`LONG_INPUT_FULL_BEAM_LENGTH`].
    pub const LONG_INPUT_BEAM_WIDTH: usize = 48;
    /// Candidate window the engine hands back (wider than a page).
    pub const CANDIDATE_LIMIT: usize = 20;
    /// Hard cap on the raw key stream with auto-commit off; keys past it are
    /// handed back to the host.
    pub const MAX_RAW_LENGTH: usize = 256;
    /// Light penalty for a non-first rank, scaled by ln(rank).
    pub const RANK_PENALTY: f64 = 0.03;
    /// Reward per character emitted, so full coverage outranks partial.
    pub const EMITTED_CHARACTER_REWARD: f64 = 2.0;
    /// Extra reward when the whole input is a single optimal single-character
    /// edge (the code is the character's shortest code).
    pub const WHOLE_INPUT_SINGLE_CHARACTER_REWARD: f64 = 5.0;
    /// Shape-code maximum length per edge (longer codes are table errors).
    pub const MAX_CODE_LENGTH: usize = 8;
    /// Aggregate a bucket's loose states into one-per-text once it grows past
    /// this many, so expansion stays bounded before an explicit de-dup.
    pub const AGGREGATE_DURING_EXPANSION_THRESHOLD: usize = 128;

    /// Beam width to use at a given raw length.
    #[must_use]
    pub const fn beam_limit_at(raw_length: usize) -> usize {
        if raw_length > LONG_INPUT_FULL_BEAM_LENGTH {
            LONG_INPUT_BEAM_WIDTH
        } else {
            BEAM_WIDTH
        }
    }
}
