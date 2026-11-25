//! [`PinyinTranslator`]: the 全拼 scheme's candidate source (SPEC §8.2).
//!
//! The translator is a thin adapter over [`PinyinLexicon`]: it reports the
//! word text and puts the matched syllable split into `Candidate::code`, so
//! the session's display line can show `ni hao` for a complete query without
//! any shell logic. A candidate carries no remaining-code comment; the split
//! is what the window displays.

use std::sync::Arc;

use crate::translator::Translator;
use crate::types::Candidate;

use super::lexicon::PinyinLexicon;

/// Full-pinyin word source.
#[derive(Debug, Clone)]
pub struct PinyinTranslator {
    lexicon: Arc<PinyinLexicon>,
}

impl PinyinTranslator {
    pub fn new(lexicon: Arc<PinyinLexicon>) -> Self {
        Self { lexicon }
    }
}

impl Translator for PinyinTranslator {
    fn translate(&self, input: &str, limit: usize) -> Vec<Candidate> {
        self.lexicon
            .lookup(input, limit)
            .into_iter()
            .map(|hit| Candidate {
                text: hit.text,
                comment: String::new(),
                code: hit.key,
                spans: Vec::new(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translator() -> PinyinTranslator {
        PinyinTranslator::new(Arc::new(PinyinLexicon::parse(
            "你好\tnihao\t200\n你\tni\t100\n开发\tkaifa\t100\n",
        )))
    }

    #[test]
    fn reports_words_with_the_syllable_split() {
        let list = translator().translate("nihao", 5);
        assert_eq!(list[0].text, "你好");
        assert_eq!(list[0].code, "ni hao");
        assert!(list[0].comment.is_empty());
    }

    #[test]
    fn honors_the_retrieval_budget() {
        assert_eq!(translator().translate("ni", 1).len(), 1);
        assert!(translator().translate("ni", 0).is_empty());
    }
}
