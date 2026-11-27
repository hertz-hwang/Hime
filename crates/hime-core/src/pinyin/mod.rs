//! Full pinyin (全拼) candidate source and its syllable index (SPEC §8).
//!
//! The shipped scheme `data/schemes/pinyin` pairs a `metadata.toml` of
//! `type = 0` with `py.hdict`, a `.hdict` container whose payload is
//! `候选\t连写全拼\t词频`. This module reads that table, indexes it by
//! syllable sequence, and answers a typed full-pinyin query. The engine's
//! state machine is untouched: pinyin candidates enter through the same
//! [`Translator`](crate::translator::Translator) pipeline as table words, so
//! Space, digits, Backspace, paging and punctuation mean exactly what they
//! mean for 形码.
//!
//! Two candidate sources make up a 全拼 answer:
//!
//! - [`lexicon`] ranks table words, including 简拼 (`kf` → 开发) and words
//!   covering a syllable prefix of the input (`开` while typing `kaifa`);
//! - [`decoder`] walks a word graph over the syllable positions and scores
//!   whole sentences with the character n-gram, which reaches combinations
//!   the table has no row for (`huangzhao` → 黄照).
//!
//! The index the lexicon queries is compiled offline into a `.hidx` container
//! (SPEC §8.1): reading a table is then a file mapping, and parsing and sorting
//! the rows in this process is only the fallback for a table nobody compiled.
//!
//! The lexicon is also the reverse-lookup source: a 形码 or 形码整句 session
//! prefixes the query with `` ` `` and the candidates carry the current
//! scheme's complete code in their comment (see
//! [`Session`](crate::session::Session)).
//!
//! Scope: word and sentence retrieval from the table plus the shared n-gram
//! model. The reference implementation's fuzzy spellings, typo correction and
//! learning channels stay deferred.

mod index;
pub mod decoder;
pub mod lexicon;
pub mod syllables;
pub mod translator;

pub use decoder::PinyinCandidate;
pub use index::index_path;
pub use lexicon::{PinyinLexicon, PinyinMatch, SharedPinyin};
pub use syllables::MAX_CODE_LEN;
pub use translator::PinyinTranslator;
