//! Character-level candidate differences for render coloring (SPEC §2.2).
//!
//! Whole-sentence candidates usually share most of their text with the top
//! candidate. The engine describes each candidate as [`DiffSpan`]s relative to
//! that base, so a shell only picks colors and never carries comparison logic.
//! The alignment is the usual LCS edit script: a change block is
//! [`DiffKind::Modified`] when both sides have the same length,
//! [`DiffKind::Deleted`] when the candidate side is shorter, and
//! [`DiffKind::Inserted`] when it is longer.

use crate::types::{Candidate, DiffKind, DiffSpan};

/// Character-level diff of `candidate` against `base`.
///
/// Returns the candidate's text as consecutive spans. A change block that
/// only removes characters has no candidate characters to render and is
/// dropped, matching how a candidate window shows the surviving text.
#[must_use]
pub fn sentence_diff(base: &str, candidate: &str) -> Vec<DiffSpan> {
    let old: Vec<char> = base.chars().collect();
    let new: Vec<char> = candidate.chars().collect();
    if old.is_empty() && new.is_empty() {
        return Vec::new();
    }
    if old.is_empty() {
        return vec![DiffSpan { text: candidate.to_owned(), kind: DiffKind::Inserted }];
    }

    // LCS length table. Whole sentences are short (the decoder caps the key
    // stream well below 256) and one page holds at most a handful of
    // candidates, so the quadratic table is negligible.
    let mut lcs = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut segments = Vec::new();
    let mut equal_run = String::new();
    let mut old_run = String::new();
    let mut candidate_run = String::new();

    fn flush_equal(segments: &mut Vec<DiffSpan>, equal_run: &mut String) {
        if !equal_run.is_empty() {
            segments.push(DiffSpan { text: std::mem::take(equal_run), kind: DiffKind::Equal });
        }
    }

    fn flush_change(
        segments: &mut Vec<DiffSpan>,
        old_run: &mut String,
        candidate_run: &mut String,
    ) {
        if old_run.is_empty() && candidate_run.is_empty() {
            return;
        }
        let kind = match candidate_run.chars().count().cmp(&old_run.chars().count()) {
            std::cmp::Ordering::Less => DiffKind::Deleted,
            std::cmp::Ordering::Greater => DiffKind::Inserted,
            std::cmp::Ordering::Equal => DiffKind::Modified,
        };
        if !candidate_run.is_empty() {
            segments.push(DiffSpan { text: std::mem::take(candidate_run), kind });
        }
        old_run.clear();
    }

    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            flush_change(&mut segments, &mut old_run, &mut candidate_run);
            equal_run.push(old[i]);
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            flush_equal(&mut segments, &mut equal_run);
            old_run.push(old[i]);
            i += 1;
        } else {
            flush_equal(&mut segments, &mut equal_run);
            candidate_run.push(new[j]);
            j += 1;
        }
    }
    while i < old.len() {
        flush_equal(&mut segments, &mut equal_run);
        old_run.push(old[i]);
        i += 1;
    }
    while j < new.len() {
        flush_equal(&mut segments, &mut equal_run);
        candidate_run.push(new[j]);
        j += 1;
    }
    flush_change(&mut segments, &mut old_run, &mut candidate_run);
    flush_equal(&mut segments, &mut equal_run);
    segments
}

/// Fill `spans` on every candidate after the first, using the first candidate
/// as the comparison base. The first candidate keeps an empty span list: a
/// shell renders it in the normal text color.
pub fn annotate_candidate_diffs(candidates: &mut [Candidate]) {
    for candidate in candidates.iter_mut() {
        candidate.spans.clear();
    }
    let Some(base) = candidates.first().map(|candidate| candidate.text.clone()) else {
        return;
    };
    for candidate in candidates.iter_mut().skip(1) {
        if candidate.text != base {
            candidate.spans = sentence_diff(&base, &candidate.text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, kind: DiffKind) -> DiffSpan {
        DiffSpan { text: text.into(), kind }
    }

    #[test]
    fn identical_text_is_one_equal_run() {
        assert_eq!(sentence_diff("今天天气好", "今天天气好"), vec![span("今天天气好", DiffKind::Equal)]);
        assert!(sentence_diff("", "").is_empty());
    }

    #[test]
    fn equal_length_replacement_is_modified() {
        assert_eq!(
            sentence_diff("abc", "axc"),
            vec![span("a", DiffKind::Equal), span("x", DiffKind::Modified), span("c", DiffKind::Equal)]
        );
    }

    #[test]
    fn removed_characters_leave_the_surviving_text_equal() {
        // The changed block has no candidate characters to color; only the
        // surviving text is rendered.
        assert_eq!(
            sentence_diff("abc", "ac"),
            vec![span("a", DiffKind::Equal), span("c", DiffKind::Equal)]
        );
    }

    #[test]
    fn additions_are_inserted() {
        assert_eq!(
            sentence_diff("ac", "abc"),
            vec![span("a", DiffKind::Equal), span("b", DiffKind::Inserted), span("c", DiffKind::Equal)]
        );
    }

    #[test]
    fn a_shorter_change_block_is_deleted() {
        assert_eq!(
            sentence_diff("嗨边里多笑是", "嗨边束人郞"),
            vec![
                span("嗨边", DiffKind::Equal),
                span("束人郞", DiffKind::Deleted),
            ]
        );
    }

    #[test]
    fn a_longer_change_block_is_inserted() {
        assert_eq!(
            sentence_diff("嗨边里多笑是", "叫常微里多笑是"),
            vec![
                span("叫常微", DiffKind::Inserted),
                span("里多笑是", DiffKind::Equal),
            ]
        );
    }

    #[test]
    fn no_common_characters_is_a_single_change_block() {
        assert_eq!(sentence_diff("甲乙", "丙丁"), vec![span("丙丁", DiffKind::Modified)]);
    }

    #[test]
    fn empty_candidate_has_nothing_to_color() {
        assert!(sentence_diff("abc", "").is_empty());
    }

    #[test]
    fn annotation_uses_the_first_candidate_as_base() {
        let mut candidates = vec![
            Candidate::new("嗨边里多笑是", ""),
            Candidate::new("嗨边里多笑是", ""),
            Candidate::new("叫常微里多笑是", ""),
        ];
        annotate_candidate_diffs(&mut candidates);
        assert!(candidates[0].spans.is_empty(), "the base needs no spans");
        assert!(candidates[1].spans.is_empty(), "an identical candidate keeps the base color");
        assert_eq!(
            candidates[2].spans,
            vec![span("叫常微", DiffKind::Inserted), span("里多笑是", DiffKind::Equal)]
        );
    }
}
