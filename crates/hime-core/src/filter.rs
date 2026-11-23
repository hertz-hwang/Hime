//! Candidate post-processing (SPEC_MVP §4).
//!
//! Filters run once, in registration order, after every translator has
//! reported, so each one sees the merged cross-source list. This is where
//! candidates get dropped, reordered or added: de-duplication lives here, and
//! later stages such as weight ranking, simplified/traditional conversion or
//! emoji injection attach here too.

use std::collections::HashSet;

use crate::types::Candidate;

/// One post-processing step over the merged candidate list.
///
/// Shares the thread-safety requirement of [`crate::translator::Translator`].
pub trait Filter: Send + Sync {
    /// Rewrites the candidate list; entries are moved, not copied.
    fn filter(&self, candidates: Vec<Candidate>) -> Vec<Candidate>;
}

/// Offers each candidate text once.
///
/// The same word can be retrieved several times for one encoding: it sits
/// under several codes (`a`, `aaa` and `aaaa` all map to `工` in the sample
/// lexicon), and a future pinyin source will offer it again beside the table
/// one. A candidate window identifies entries by their text, so repeated text
/// would collapse into a single cell while the numbering keeps counting
/// positions — the shell would show keys 1 and 4 blank with only 2, 3 and 5
/// filled. The first occurrence wins (exact code or highest weight, so its
/// `comment` is the informative one) and the freed slot is filled by whatever
/// comes next, because the page is cut afterwards by [`LimitFilter`].
pub struct DedupFilter;

impl Filter for DedupFilter {
    fn filter(&self, candidates: Vec<Candidate>) -> Vec<Candidate> {
        let mut seen: HashSet<String> = HashSet::with_capacity(candidates.len());
        candidates
            .into_iter()
            .filter(|c| seen.insert(c.text.clone()))
            .collect()
    }
}

/// Cuts the list down to what one page shows.
///
/// Runs last so that every drop, reorder and injection above it has had its
/// say about the full retrieved window.
pub struct LimitFilter {
    max: usize,
}

impl LimitFilter {
    pub fn new(max: usize) -> Self {
        Self { max }
    }
}

impl Filter for LimitFilter {
    fn filter(&self, candidates: Vec<Candidate>) -> Vec<Candidate> {
        candidates.into_iter().take(self.max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(text: &str, comment: &str) -> Candidate {
        Candidate { text: text.into(), comment: comment.into() }
    }

    fn texts(list: &[Candidate]) -> Vec<&str> {
        list.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn dedup_keeps_the_first_occurrence_and_its_comment() {
        let input = vec![cand("工", ""), cand("戈", ""), cand("工", "~aa"), cand("匿", "~d")];
        let out = DedupFilter.filter(input);
        assert_eq!(
            out,
            vec![cand("工", ""), cand("戈", ""), cand("匿", "~d")],
            "the exact-code 工 wins over the prefix-code copy"
        );
    }

    /// This is the reported defect: without dedup the page was
    /// `工 戈 式 工 工`, and the candidate window rendered three cells numbered
    /// 2, 3 and 5. Dropping the repeats leaves room for the entries that
    /// follow them in the retrieved window.
    #[test]
    fn dedup_frees_slots_for_later_matches() {
        let retrieved = vec![
            cand("工", ""),
            cand("戈", ""),
            cand("式", "~a"),
            cand("工", "~aa"),
            cand("工", "~aaa"),
            cand("匿", "~d"),
            cand("㠭", "~aaa"),
        ];
        let page = LimitFilter::new(5).filter(DedupFilter.filter(retrieved));
        assert_eq!(texts(&page), vec!["工", "戈", "式", "匿", "㠭"]);
    }

    #[test]
    fn limit_truncates_without_reordering() {
        let input = vec![cand("一", ""), cand("二", ""), cand("三", "")];
        assert_eq!(texts(&LimitFilter::new(2).filter(input)), vec!["一", "二"]);
        let again = vec![cand("一", ""), cand("二", ""), cand("三", "")];
        assert!(LimitFilter::new(0).filter(again).is_empty());
    }

    #[test]
    fn dedup_passes_an_empty_list_through() {
        assert!(DedupFilter.filter(Vec::new()).is_empty());
    }
}
