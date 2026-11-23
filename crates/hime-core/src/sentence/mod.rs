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
//! [`auto_commit`] implements Fire's probabilistic early-commit and empty-code
//! state machine; it is gated on `Config::auto_commit` and lives in the session.
//!
//! Deferred relative to Fire (documented where each belongs): the learning
//! channels (session cache / user n-gram / correction), the Aho-Corasick
//! weighted-word supplement, the per-session incremental lattice (a latency
//! optimization), and pinyin decoding. None of them affect the deterministic
//! whole-sentence ranking that is the core of 形码整句; they are additive score
//! terms gated off when there is no learning or user-word data, exactly as in
//! Fire.

pub(crate) mod auto_commit;
pub mod decoder;
pub mod engine;
pub mod lexicon;

pub use decoder::{SentenceCandidate, SentenceDecoder};
pub use engine::{SentenceEngine, SentenceModelError, SentenceTranslator};
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
    /// Cap on the raw key stream with auto-commit on. Lower than the manual
    /// cap because the near-cap fallback commits a mature prefix before the
    /// buffer can grow into the range where decoding slows (Fire keeps it at
    /// 64 ≈ sixteen multi-key 形码 characters).
    pub const AUTO_COMMIT_MAX_RAW_LENGTH: usize = 64;
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

    // ---- Probabilistic early commit (ported from Fire `SentenceConfig`) ----
    /// Posterior share a prefix must reach to qualify as commit evidence.
    pub const EARLY_COMMIT_MINIMUM_SHARE: f64 = 0.995;
    /// Strong-confirmation share; reaching it twice commits without waiting.
    pub const EARLY_COMMIT_STRONG_SHARE: f64 = 0.99999;
    /// Boundary share at which a raw position counts as a closed split.
    pub const EARLY_COMMIT_CLOSED_BOUNDARY_SHARE: f64 = 0.99999;
    /// Consecutive generations a prefix must hold before it matures.
    pub const EARLY_COMMIT_REQUIRED_EVIDENCE: usize = 3;
    /// Consecutive strong confirmations that let a prefix mature early.
    pub const EARLY_COMMIT_REQUIRED_STRONG: usize = 2;
    /// Generations a tracker may survive a neutral (low-confidence) gap.
    pub const EARLY_COMMIT_MAXIMUM_NEUTRAL_GAP: usize = 3;
    /// Keys the composition keeps after an auto-commit, so typing continues.
    pub const EARLY_COMMIT_RETAINED_RAW_LENGTH: usize = 3;
    /// Keys at the start of a composition that never count as evidence.
    pub const FIRST_KEYS_IMMUNITY: usize = 4;
    /// How close to the cap (in keys) the last-resort fallback starts.
    pub const NEAR_CAP_COMMIT_GENERATIONS: usize = 5;
    /// Prefix share the near-cap fallback accepts (vs the normal minimum).
    pub const NEAR_CAP_MINIMUM_SHARE: f64 = 0.9;

    /// Beam width to use at a given raw length.
    #[must_use]
    pub const fn beam_limit_at(raw_length: usize) -> usize {
        if raw_length > LONG_INPUT_FULL_BEAM_LENGTH {
            LONG_INPUT_BEAM_WIDTH
        } else {
            BEAM_WIDTH
        }
    }

    /// Cap on the raw key stream, which depends on whether auto-commit is on:
    /// the tighter [`AUTO_COMMIT_MAX_RAW_LENGTH`] so the near-cap fallback can
    /// still commit before the cap, or the manual [`MAX_RAW_LENGTH`] when
    /// auto-commit is off (a long sentence typed by hand).
    #[must_use]
    pub const fn max_raw_length(auto_commit: bool) -> usize {
        if auto_commit {
            AUTO_COMMIT_MAX_RAW_LENGTH
        } else {
            MAX_RAW_LENGTH
        }
    }
}
