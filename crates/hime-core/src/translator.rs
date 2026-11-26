//! Encoding to candidate retrieval (SPEC §4).
//!
//! A [`Translator`] turns the encoding typed so far into candidates. This is
//! the main extension point of the engine: it ships [`TableTranslator`],
//! which queries the TSV lexicon; a later `PinyinTranslator` will walk a
//! full/pinyin-syllable graph instead. Translators report raw matches in
//! retrieval-priority order and drop nothing — dropping, reordering and
//! injecting entries belongs to [`Filter`](crate::filter::Filter).

use std::sync::Arc;

use crate::dictionary::Dictionary;
use crate::types::Candidate;

/// One candidate source.
///
/// The FFI layer hands sessions to whichever thread delivers a key, so a
/// source must be safe to share and use across threads.
pub trait Translator: Send + Sync {
    /// Candidates for `input`, best first, at most `limit` of them.
    ///
    /// `limit` is a retrieval budget, not the number a shell shows: it is
    /// [`crate::FETCH_CANDIDATES`], which is larger than
    /// [`crate::MAX_CANDIDATES`] because filters drop and add entries
    /// afterwards and the page is cut at the very end.
    fn translate(&self, input: &str, limit: usize) -> Vec<Candidate>;

    /// Candidates for `input` with the text left of the cursor as model
    /// history. Sources that never read history keep the default forwarding
    /// to [`Self::translate`]; the 形码整句 / 全拼 sources use it so the
    /// n-gram (and the learned user n-gram blended with it) scores the chain
    /// in context.
    fn translate_with_context(
        &self,
        input: &str,
        context: &str,
        limit: usize,
    ) -> Vec<Candidate> {
        let _ = context;
        self.translate(input, limit)
    }
}

/// Looks the encoding up in a [`Dictionary`] table (形码查表).
#[derive(Debug, Clone)]
pub struct TableTranslator {
    dict: Arc<Dictionary>,
}

impl TableTranslator {
    pub fn new(dict: Arc<Dictionary>) -> Self {
        Self { dict }
    }
}

impl Translator for TableTranslator {
    fn translate(&self, input: &str, limit: usize) -> Vec<Candidate> {
        self.dict.lookup(input, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::Dictionary;

    fn translator(rows: &str) -> TableTranslator {
        TableTranslator::new(Arc::new(Dictionary::parse_str(rows).expect("rows parse")))
    }

    fn cand(text: &str, comment: &str) -> Candidate {
        Candidate::new(text, comment)
    }

    /// The translator is the pure table query: exact code first, then longer
    /// codes with the remaining coding in `comment`, best weight first.
    #[test]
    fn reports_table_order_with_remaining_coding_as_comment() {
        let t = translator("a\t工\t100\na\t戈\t90\nab\t一\t80\n");
        assert_eq!(t.translate("a", 8), vec![cand("工", ""), cand("戈", ""), cand("一", "~b")]);
    }

    /// Repeated text under several codes is a retrieval fact, not something
    /// this stage hides: DedupFilter is the stage that decides what a page
    /// shows.
    #[test]
    fn keeps_repeated_texts_for_the_filter_stage_to_handle() {
        let t = translator("a\t工\t100\nab\t工\t90\n");
        assert_eq!(t.translate("a", 8), vec![cand("工", ""), cand("工", "~b")]);
    }

    #[test]
    fn empty_encoding_or_zero_budget_returns_nothing() {
        let t = translator("a\t工\t100\n");
        assert!(t.translate("", 8).is_empty());
        assert!(t.translate("a", 0).is_empty());
        assert!(t.translate("z", 8).is_empty());
    }

    #[test]
    fn respects_the_retrieval_budget() {
        let t = translator("a\t一\t1\nab\t二\t2\nac\t三\t3\n");
        assert_eq!(t.translate("a", 2).len(), 2);
    }
}
