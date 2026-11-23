//! The retrieval pipeline: translators, then filters (SPEC_MVP §4).
//!
//! [`Pipeline`] is the whole "encoding in, page of candidates out" step. It
//! keeps no typing state — [`Session`](crate::session::Session) owns the
//! encoding and asks the pipeline for candidates whenever it changes.

use std::sync::Arc;

use crate::dictionary::Dictionary;
use crate::filter::{DedupFilter, Filter};
use crate::translator::{TableTranslator, Translator};
use crate::types::Candidate;
use crate::FETCH_CANDIDATES;

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
    /// and a filter can interleave the two. The chain returns the whole
    /// retrieved window; cutting it into pages and paging through them is the
    /// caller's job ([`Session`](crate::session::Session)), so a chain can add
    /// or reorder candidates without any page-boundary awareness.
    pub fn new(translators: Vec<Box<dyn Translator>>, filters: Vec<Box<dyn Filter>>) -> Self {
        Self { translators, filters }
    }

    /// The MVP chain: one 形码 table source, then de-duplicate. Each translator
    /// is asked for [`FETCH_CANDIDATES`] entries, so the window spans more than
    /// one page and [`Session`](crate::session::Session) can page through it.
    pub fn table(dict: Arc<Dictionary>) -> Self {
        Self::new(vec![Box::new(TableTranslator::new(dict))], vec![Box::new(DedupFilter)])
    }

    /// The full candidate window for `code`, best first: every match the
    /// translators report after the filters run, wider than one page.
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
    use crate::{FETCH_CANDIDATES, MAX_CANDIDATES};

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
    /// holds it three times. The window must be free of repeats yet still span
    /// more than one page, so `Session` can page through it.
    #[test]
    fn table_chain_returns_the_whole_window_without_repeats() {
        let pipeline = Pipeline::table(dict(concat!(
            "a\t工\t100\n",
            "a\t戈\t90\n",
            "aa\t式\t100\n",
            "aaa\t工\t100\n",
            "aaaa\t工\t100\n",
            "aaaa\t㠭\t80\n",
            "ad\t匿\t100\n",
        )));
        let window = pipeline.retrieve("a");
        assert_eq!(texts(&window), vec!["工", "戈", "式", "匿", "㠭"]);
        assert!(window.len() <= FETCH_CANDIDATES);
    }

    /// The chain no longer cuts to a page: it hands back every match, wider
    /// than the default page, and the caller does the page slicing.
    #[test]
    fn retrieve_spans_more_than_one_page() {
        let rows: String = (0..12)
            .map(|i| format!("a\t字{i}\t{}\n", 100 - i))
            .collect();
        let window = Pipeline::table(dict(&rows)).retrieve("a");
        assert_eq!(window.len(), 12, "every distinct match survives to the caller");
        assert!(window.len() > MAX_CANDIDATES, "wider than one page");
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
