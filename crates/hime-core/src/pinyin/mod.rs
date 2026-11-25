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
//! The lexicon is also the reverse-lookup source: a 形码 or 形码整句 session
//! prefixes the query with `` ` `` and the candidates carry the current
//! scheme's complete code in their comment (see
//! [`Session`](crate::session::Session)).
//!
//! Scope: word lookup only. The reference implementation's fuzzy spellings,
//! typo correction and pinyin whole-sentence decoder are not ported; the
//! table's words and syllable-prefix matching are.

pub mod lexicon;
pub mod syllables;
pub mod translator;

pub use lexicon::{PinyinLexicon, PinyinMatch, SharedPinyin};
pub use syllables::MAX_CODE_LEN;
pub use translator::PinyinTranslator;
