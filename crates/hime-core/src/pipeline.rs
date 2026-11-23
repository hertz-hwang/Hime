//! The retrieval pipeline: translators, then filters (SPEC_MVP §4).
//!
//! [`Pipeline`] is the whole "encoding in, page of candidates out" step. It
//! keeps no typing state — [`Session`](crate::session::Session) owns the
//! encoding and asks the pipeline for candidates whenever it changes.

use std::sync::Arc;

use crate::dictionary::Dictionary;
use crate::filter::{DedupFilter, Filter, LimitFilter};
use crate::translator::{TableTranslator, Translator};
use crate::types::Candidate;
use crate::{FETCH_CANDIDATES, MAX_CANDIDATES};

/// The configured translator and filter chain.
pub struct Pipeline {
    translators: Vec<Box<dyn Translator>>,
    filters: Vec<Box<dyn Filter>>,
}

/// The stages are trait objects without their own `Debug`, so a session can
/// still be inspected by showing how it is wired.
impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("translators", &self.translators.len())
            .field("filters", &self.filters.len())
            .finish()
    }
}

impl Pipeline {
    /// Builds a chain from parts.
    ///
    /// Translators are asked in order and their reports are concatenated in
    /// that order, so a later `PinyinTranslator` sits beside the table source
    /// and a filter can interleave the two. The last filter is responsible for
    /// cutting the list to one page; [`Pipeline::table`] does that with
    /// [`LimitFilter`].
    pub fn new(translators: Vec<Box<dyn Translator>>, filters: Vec<Box<dyn Filter>>) -> Self {
        Self { translators, filters }
    }

    /// The MVP chain: one 形码 table source, then de-duplicate, then cut the
    /// page.
    pub fn table(dict: Arc<Dictionary>) -> Self {
        Self::new(
            vec![Box::new(TableTranslator::new(dict))],
            vec![Box::new(DedupFilter), Box::new(LimitFilter::new(MAX_CANDIDATES))],
        )
    }

    /// Candidates for `code`, ready to render.
    pub fn retrieve(&self, code: &str) -> Vec<Candidate> {
        let merged: Vec<Candidate> = self
            .translators
            .iter()
            .flat_map(|translator| translator.translate(code, FETCH_CANDIDATES))
            .collect();
        self.filters
            .iter()
            .fold(merged, |candidates, filter| filter.filter(candidates))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::Filter;
    use crate::translator::Translator;
    use crate::MAX_CANDIDATES;

    fn dict(rows: &str) -> Arc<Dictionary> {
        Arc::new(Dictionary::parse_str(rows).expect("rows parse"))
    }

    fn texts(list: &[Candidate]) -> Vec<&str> {
        list.iter().map(|c| c.text.as_str()).collect()
    }

    /// Mirrors a future source that invents candidates instead of reading them
    /// from a table.
    struct FixedTranslator { texts: Vec<&'static str> }

    impl Translator for FixedTranslator {
        fn translate(&self, _input: &str, limit: usize) -> Vec<Candidate> {
            self.texts
                .iter()
                .take(limit)
                .map(|text| Candidate { text: (*text).into(), comment: String::new() })
                .collect()
        }
    }

    struct PrependFilter;

    impl Filter for PrependFilter {
        fn filter(&self, mut candidates: Vec<Candidate>) -> Vec<Candidate> {
            candidates.insert(0, Candidate { text: "★".into(), comment: String::new() });
            candidates
        }
    }

    /// The shipped sample maps `工` to `a`, `aaa` and `aaaa`, so the raw window
    /// holds it three times. A page must still be full and free of repeats.
    #[test]
    fn table_chain_returns_a_full_page_without_repeats() {
        let pipeline = Pipeline::table(dict(concat!(
            "a\t工\t100\n",
            "a\t戈\t90\n",
            "aa\t式\t100\n",
            "aaa\t工\t100\n",
            "aaaa\t工\t100\n",
            "aaaa\t㠭\t80\n",
            "ad\t匿\t100\n",
        )));
        let page = pipeline.retrieve("a");
        assert_eq!(texts(&page), vec!["工", "戈", "式", "匿", "㠭"]);
        assert!(page.len() <= MAX_CANDIDATES);
    }

    #[test]
    fn filters_see_the_sources_merged_in_registration_order() {
        let pipeline = Pipeline::new(
            vec![
                Box::new(FixedTranslator { texts: vec!["甲", "乙"] }),
                Box::new(FixedTranslator { texts: vec!["丙"] }),
            ],
            vec![Box::new(PrependFilter), Box::new(DedupFilter)],
        );
        assert_eq!(texts(&pipeline.retrieve("x")), vec!["★", "甲", "乙", "丙"]);
    }

    #[test]
    fn empty_encoding_retrieves_nothing() {
        assert!(Pipeline::table(dict("a\t工\t100\n")).retrieve("").is_empty());
    }
}
