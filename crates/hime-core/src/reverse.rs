//! 反查: pinyin lookup that reports the current scheme's code (SPEC §8.3).
//!
//! With a 形码 or 形码整句 scheme active, `` ` `` opens a lookup: the letters
//! that follow are full pinyin, the candidates are the pinyin words, and each
//! candidate carries the complete code that word has in the active scheme.
//! `` ` `` followed by Space commits the literal backtick.
//!
//! The lookup is deliberately one object: the pinyin source plus a way to
//! ask it for the active scheme's codes. The session owns the mode flag and
//! the query; nothing here keeps state, so one lookup is shared by every
//! session of the same scheme.

use std::fmt;
use std::sync::Arc;

use crate::dictionary::Dictionary;
use crate::pinyin::lexicon::SharedPinyin;
use crate::pinyin::PinyinLexicon;
use crate::sentence::SentenceEngine;
use crate::types::Candidate;

/// Which scheme table answers "what is this word's complete code?".
enum CodeSource {
    /// A 形码 word table.
    Table(Arc<Dictionary>),
    /// A 形码整句 table (its word graph knows every code of every entry).
    Sentence(Arc<SentenceEngine>),
}

impl CodeSource {
    fn code_for(&self, text: &str) -> Option<&str> {
        match self {
            Self::Table(dict) => dict.code_for(text),
            Self::Sentence(engine) => engine.code_for(text),
        }
    }
}

/// Longest word whose missing code is answered by per-character codes. A
/// 形码整句 table holds single characters, so a word is spelled out from its
/// characters; past this length the annotation would be wider than the
/// candidate list is worth.
const MAX_CHARACTER_CODE_WORD: usize = 4;

/// The pinyin source plus the active scheme's code table. The handle is the
/// same one 全拼 sessions read, and the shell is expected to have warmed it
/// before the first `` ` ``.
pub struct ReverseLookup {
    pinyin: Arc<SharedPinyin>,
    codes: CodeSource,
}

impl ReverseLookup {
    /// Lookup for a 形码 word table.
    #[must_use]
    pub fn for_table(pinyin: Arc<SharedPinyin>, dict: Arc<Dictionary>) -> Self {
        Self { pinyin, codes: CodeSource::Table(dict) }
    }

    /// Lookup for a 形码整句 scheme.
    #[must_use]
    pub fn for_sentence(pinyin: Arc<SharedPinyin>, engine: Arc<SentenceEngine>) -> Self {
        Self { pinyin, codes: CodeSource::Sentence(engine) }
    }

    /// Lookup over an already-loaded table (tests, and a shell that loads the
    /// table itself).
    #[must_use]
    pub fn from_lexicon(lexicon: Arc<PinyinLexicon>, dict: Arc<Dictionary>) -> Self {
        Self {
            pinyin: Arc::new(SharedPinyin::from_loaded(lexicon)),
            codes: CodeSource::Table(dict),
        }
    }

    /// Pinyin candidates for `query`, each with the active scheme's complete
    /// code in `comment`. A word the table does not list falls back to its
    /// characters' codes joined by spaces (`你好` → `flll olgu`) when it is
    /// short enough; otherwise the comment stays empty.
    ///
    /// This reads the table only when it is already in memory: a lookup answers
    /// from a partial query on every keystroke, so it must never wait for an
    /// 11 MB file. A shell warms the handle with
    /// [`SharedPinyin::preheat`](SharedPinyin::preheat) at start-up; while that
    /// load is still running this returns no candidates and the next keystroke,
    /// once the table is there, returns them.
    #[must_use]
    pub fn lookup(&self, query: &str, limit: usize) -> Vec<Candidate> {
        let Some(pinyin) = self.pinyin.get_ready() else {
            return Vec::new();
        };
        pinyin
            .lookup(query, limit)
            .into_iter()
            .map(|hit| Candidate {
                comment: self.code_text(&hit.text),
                text: hit.text,
                code: hit.key,
                spans: Vec::new(),
            })
            .collect()
    }

    /// The annotation drawn after one candidate.
    fn code_text(&self, text: &str) -> String {
        if let Some(code) = self.codes.code_for(text) {
            return code.to_owned();
        }
        let characters = text.chars().count();
        if !(2..=MAX_CHARACTER_CODE_WORD).contains(&characters) {
            return String::new();
        }
        let mut parts: Vec<&str> = Vec::with_capacity(characters);
        for character in text.chars() {
            let mut buffer = [0u8; 4];
            match self.codes.code_for(character.encode_utf8(&mut buffer)) {
                Some(code) => parts.push(code),
                // One character the table does not know makes the whole
                // spelling guesswork; show nothing instead of half of it.
                None => return String::new(),
            }
        }
        parts.join(" ")
    }
}

impl fmt::Debug for ReverseLookup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = match self.codes {
            CodeSource::Table(_) => "table",
            CodeSource::Sentence(_) => "sentence",
        };
        f.debug_struct("ReverseLookup")
            .field("codes", &source)
            .field("pinyin", &self.pinyin)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup() -> ReverseLookup {
        let pinyin = Arc::new(PinyinLexicon::parse(
            "你好\tnihao\t200\n你\tni\t100\n好\thao\t90\n",
        ));
        let dict = Arc::new(
            Dictionary::parse_str("你\tfl\n你\tflll\n好\tolgu\n你好\tflol\n").expect("table"),
        );
        ReverseLookup::from_lexicon(pinyin, dict)
    }

    #[test]
    fn candidates_carry_the_scheme_code() {
        let list = lookup().lookup("nihao", 5);
        assert_eq!(list[0].text, "你好");
        assert_eq!(list[0].comment, "flol");
        assert_eq!(list[0].code, "ni hao");

        let list = lookup().lookup("ni", 5);
        assert_eq!(list[0].text, "你");
        assert_eq!(list[0].comment, "flll", "the longest code is the full code");
    }

    /// 反查 reads the table from memory. A handle that is still loading — or
    /// that was never pointed at a readable file — answers the keystroke with no
    /// candidates and no wait; the same lookup answers once the load is done,
    /// which is the degradation SPEC §8.4 asks for.
    #[test]
    fn a_table_that_is_not_ready_answers_nothing() {
        let dict = Arc::new(Dictionary::parse_str("你好\tflol\n").expect("table"));
        let pinyin = Arc::new(SharedPinyin::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/schemes/pinyin/py.hdict"
        )));
        let lookup = ReverseLookup::for_table(pinyin, dict);

        assert!(
            lookup.lookup("nihao", 5).is_empty(),
            "no load has run, so there is nothing to wait for"
        );

        Arc::clone(&lookup.pinyin).preheat();
        for _ in 0..2_000 {
            if !lookup.lookup("nihao", 5).is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let list = lookup.lookup("nihao", 5);
        assert_eq!(list[0].text, "你好", "the next keystroke after the load has the candidates");
        assert_eq!(list[0].comment, "flol");
    }

    /// A word the table does not list falls back to its characters' codes,
    /// which is what makes 反查 usable with a single-character sentence table.
    #[test]
    fn a_missing_word_spells_out_its_characters() {
        let pinyin = Arc::new(PinyinLexicon::parse("你好\tnihao\t5\n生僻\tshengpi\t1\n"));
        let dict =
            Arc::new(Dictionary::parse_str("你\tflll\n好\tolgu\n生\tog\n").expect("table"));
        let lookup = ReverseLookup::from_lexicon(pinyin, dict);

        let list = lookup.lookup("nihao", 5);
        assert_eq!(list[0].comment, "flll olgu");

        // 僻 is unknown, so the spelling would be a guess: nothing is shown.
        let list = lookup.lookup("shengpi", 5);
        assert_eq!(list[0].text, "生僻");
        assert!(list[0].comment.is_empty());

        // Longer than the fallback's own limit.
        let pinyin = Arc::new(PinyinLexicon::parse("一二三四五\tyiersansiwu\t1\n"));
        let dict = Arc::new(
            Dictionary::parse_str("一\tg\n二\tf\n三\td\n四\ts\n五\ta\n").expect("table"),
        );
        let lookup = ReverseLookup::from_lexicon(pinyin, dict);
        assert!(
            lookup.lookup("yiersansiwu", 5)[0].comment.is_empty(),
            "a five-character word is too long to spell out"
        );
    }
}
