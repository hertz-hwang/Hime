//! Whole-sentence auto-commit (port of the reference `SentenceEngine` early-commit
//! and empty-code state machine; SPEC §7.6).
//!
//! The candidate pipeline (SPEC §7.3) is stateless — it answers a code with a
//! ranked list. Committing a sentence *before* the user presses Space is a
//! per-session decision made across keystrokes, so its state lives here, inside
//! [`crate::session::Session`] (Fat Core, AGENTS.md §1): the shell still only
//! forwards keys and renders the [`crate::types::EngineResponse`].
//!
//! Two mechanisms, both gated on `Config::auto_commit`:
//!
//! - **Probabilistic early commit.** Each keystroke re-decodes the whole code and
//!   measures how much of the sentence posterior is carried by each *text prefix
//!   ending at a closed raw boundary*. A prefix that holds a high share across
//!   several generations has "matured": it is committed and the still-unconsumed
//!   keys stay in the composition so typing continues. Past `first_keys_immunity`
//!   only, and voided whenever the beam truncated the posterior (except near the
//!   cap, where a last-resort commit triggers on the best closed boundary).
//! - **Empty-code commit.** When the previous code fully decoded but a new letter
//!   makes the whole tail uncoverable and the last segment is no longer a legal
//!   code prefix, the previous code's first candidate is committed.
//!
//! Paging, Backspace, or any manual selection suspends auto-commit for the rest
//! of the composition; committing the whole code (Space / digit) resets it.
//!
//! The deferred reference channels stay deferred: with no supplement (weighted-word)
//! data, no prefix is ever supplement-weighted, so the `acceptedTop` guard is
//! always satisfied; that reduces cleanly to the code below.

use std::collections::HashMap;

use super::config::{
    AUTO_COMMIT_MAX_RAW_LENGTH, EARLY_COMMIT_MAXIMUM_NEUTRAL_GAP, EARLY_COMMIT_MINIMUM_SHARE,
    EARLY_COMMIT_REQUIRED_EVIDENCE, EARLY_COMMIT_REQUIRED_STRONG, EARLY_COMMIT_RETAINED_RAW_LENGTH,
    EARLY_COMMIT_STRONG_SHARE, FIRST_KEYS_IMMUNITY, NEAR_CAP_COMMIT_GENERATIONS,
    NEAR_CAP_MINIMUM_SHARE,
};
use super::decoder::{
    eligible_edges, has_selection_suffix, normalize, parse_selector, CompletedView, PrefixEvidence,
    SentenceDecode,
};
use super::engine::SentenceEngine;
use super::lexicon::SentenceLexicon;

/// A sentence fragment the engine auto-commits, plus the keys the composition
/// keeps (the still-unfinished tail of the code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SentenceAutoCommitOutcome {
    /// Text to send to the host.
    pub(crate) text: String,
    /// Code that stays in the composition buffer after the commit.
    pub(crate) retained_raw: String,
}

/// Per-prefix maturity tracker (port of `SentenceTracker`), keyed by
/// `(text, raw_length)`.
#[derive(Debug, Clone)]
struct Tracker {
    text: String,
    text_char_count: usize,
    raw_length: usize,
    evidence_count: usize,
    strong_count: usize,
    gap_count: usize,
    last_share: f64,
}

/// A captured "previous code" candidate awaiting the empty-code commit (port of
/// `SentenceEmptyPending`).
#[derive(Debug, Clone)]
struct EmptyPending {
    candidate_text: String,
    base_raw_length: usize,
    last_segment_start: usize,
}

fn tracker_key(text: &str, raw_length: usize) -> String {
    format!("{text}\u{1F}{raw_length}")
}

/// The per-session auto-commit state. Owns only plain data (no `Rc`), so it
/// stays `Send` and lives directly in the session.
#[derive(Debug, Default, Clone)]
pub(crate) struct SentenceAutoCommit {
    trackers: HashMap<String, Tracker>,
    last_seen_raw: String,
    last_auto_commit_raw_length: usize,
    empty_pending: Option<EmptyPending>,
    suspended: bool,
    /// Set after an empty-code commit; reserved for future continuation rules,
    /// carried for parity with the reference implementation.
    continuation: bool,
    session_key_count: usize,
}

impl SentenceAutoCommit {
    /// Called after a letter is appended to `raw` (the full code). Returns the
    /// fragment to auto-commit and the keys to keep, or `None` to keep
    /// composing. Mirrors the order of the reference `keyPressed`: empty-code first,
    /// then the probabilistic early commit.
    pub(crate) fn key_pressed(
        &mut self,
        engine: &SentenceEngine,
        raw: &str,
        appended_letter: bool,
        auto_commit: bool,
        context: &str,
    ) -> Option<SentenceAutoCommitOutcome> {
        self.session_key_count += 1;

        if !auto_commit {
            self.reset_evidence();
            self.empty_pending = None;
            return None;
        }
        // A code holding anything but letters/selectors never auto-commits.
        if normalize(raw).is_none() {
            self.reset_evidence();
            self.empty_pending = None;
            return None;
        }

        if appended_letter && !self.suspended {
            if let Some(commit) = self.try_empty_code_commit(engine, raw, context) {
                return Some(commit);
            }
        } else {
            self.empty_pending = None;
        }

        if !self.suspended {
            return self.try_early_commit(engine, raw, context);
        }
        self.reset_evidence();
        None
    }

    /// Paging / a manual pick: hold auto-commit for the rest of the composition
    /// and drop accumulated evidence (the reference `suspendAutoCommit`).
    pub(crate) fn suspend(&mut self) {
        self.reset_evidence();
        self.suspended = true;
        self.empty_pending = None;
    }

    /// Backspace (or a selector key): the code just changed, so accumulated
    /// evidence is void and any pending empty-code capture is dropped, but the
    /// session stays active and re-accumulates on the next keys (the reference
    /// `evidenceInvalidated`). Unlike [`Self::suspend`] this does not set the
    /// suspended flag — editing the tail keeps auto-commit available.
    pub(crate) fn evidence_invalidated(&mut self) {
        self.reset_evidence();
        self.empty_pending = None;
    }

    /// The whole code was consumed (Space / digit) or the composition was
    /// dropped: clear all auto-commit state. The left context survives on the
    /// session, which owns it (the reference `resetAll`).
    pub(crate) fn reset(&mut self) {
        self.trackers.clear();
        self.last_seen_raw.clear();
        self.last_auto_commit_raw_length = 0;
        self.empty_pending = None;
        self.suspended = false;
        self.continuation = false;
        self.session_key_count = 0;
    }

    fn reset_evidence(&mut self) {
        self.trackers.clear();
        self.last_seen_raw.clear();
    }

    /// The reference `try_early_commit`: build the evidence, gate on the first-keys
    /// immunity and truncation, then accumulate per-prefix maturity and try to
    /// commit.
    fn try_early_commit(
        &mut self,
        engine: &SentenceEngine,
        raw: &str,
        context: &str,
    ) -> Option<SentenceAutoCommitOutcome> {
        if self.session_key_count <= FIRST_KEYS_IMMUNITY {
            self.reset_evidence();
            return None;
        }

        let decode = engine.decode_with_evidence(raw, context);
        let evidence = &decode.evidence;
        let near_cap = raw.len() >= AUTO_COMMIT_MAX_RAW_LENGTH - NEAR_CAP_COMMIT_GENERATIONS;
        // A truncated posterior voids the evidence, except near the cap where we
        // would rather emit a word than refuse the key.
        if evidence.confidence_truncated && !near_cap {
            self.reset_evidence();
            return None;
        }

        if self.last_seen_raw == raw {
            return self.commit_mature_prefix_or_near_cap_fallback(raw, &decode);
        }

        // A generation must extend the previous code by exactly one key, or the
        // accumulated history is meaningless.
        let extends_previous = self.last_seen_raw.is_empty()
            || (raw.len() == self.last_seen_raw.len() + 1
                && raw.as_bytes().starts_with(self.last_seen_raw.as_bytes()));
        if !extends_previous {
            self.trackers.clear();
        }
        self.last_seen_raw = raw.to_string();

        let minimum_share =
            if near_cap { NEAR_CAP_MINIMUM_SHARE } else { EARLY_COMMIT_MINIMUM_SHARE };
        let merged_incomplete_tail = evidence.merged_incomplete_tail;

        let mut qualifying: Vec<&PrefixEvidence> = Vec::new();
        for prefix in &evidence.prefixes {
            if prefix.text.is_empty() {
                continue;
            }
            let boundary_closed = prefix.boundary_closed
                || (near_cap && prefix.boundary_share >= NEAR_CAP_MINIMUM_SHARE);
            if !boundary_closed {
                continue;
            }
            if prefix.share < minimum_share {
                continue;
            }
            if prefix.raw_length == 0 || prefix.raw_length >= raw.len() {
                continue;
            }
            if prefix.text_char_count == 0 {
                continue;
            }
            if !merged_incomplete_tail && !prefix_belongs_to_visible(prefix, &decode.candidates) {
                continue;
            }
            qualifying.push(prefix);
        }

        // A neutral generation (low confidence or a merged unfinished tail) holds
        // the existing trackers without counting against them.
        if qualifying.is_empty() && (evidence.neutral_low_confidence || merged_incomplete_tail) {
            retain_trackers_without_counting(&mut self.trackers, &evidence.prefixes);
            return self.commit_mature_prefix_or_near_cap_fallback(raw, &decode);
        }

        let mut next: HashMap<String, Tracker> = HashMap::new();
        for prefix in qualifying {
            let key = tracker_key(&prefix.text, prefix.raw_length);
            let mut tracker = self.trackers.remove(&key).unwrap_or_else(|| Tracker {
                text: prefix.text.clone(),
                text_char_count: prefix.text_char_count,
                raw_length: prefix.raw_length,
                evidence_count: 0,
                strong_count: 0,
                gap_count: 0,
                last_share: 0.0,
            });
            tracker.evidence_count =
                EARLY_COMMIT_REQUIRED_EVIDENCE.min(tracker.evidence_count + 1);
            tracker.strong_count = if prefix.share >= EARLY_COMMIT_STRONG_SHARE {
                EARLY_COMMIT_REQUIRED_STRONG.min(tracker.strong_count + 1)
            } else {
                0
            };
            tracker.gap_count = 0;
            tracker.last_share = prefix.share;
            next.insert(key, tracker);
        }
        self.trackers = next;
        self.commit_mature_prefix_or_near_cap_fallback(raw, &decode)
    }

    /// Commit a matured prefix, else try the near-cap last-resort (the reference
    /// `commitMaturePrefixOrNearCapFallback`).
    fn commit_mature_prefix_or_near_cap_fallback(
        &mut self,
        raw: &str,
        decode: &SentenceDecode,
    ) -> Option<SentenceAutoCommitOutcome> {
        if let Some(commit) = self.commit_mature_prefix(raw) {
            return Some(commit);
        }
        self.near_cap_last_resort(raw, decode)
    }

    /// The reference `tryCommitMaturePrefix`: among matured trackers that leave enough
    /// keys, commit the best (more characters, then higher share, then earlier
    /// boundary).
    fn commit_mature_prefix(&mut self, raw: &str) -> Option<SentenceAutoCommitOutcome> {
        let retain = EARLY_COMMIT_RETAINED_RAW_LENGTH;
        let near_cap = raw.len() >= AUTO_COMMIT_MAX_RAW_LENGTH - NEAR_CAP_COMMIT_GENERATIONS;
        let required_evidence = if near_cap { 1 } else { EARLY_COMMIT_REQUIRED_EVIDENCE };

        let selected = {
            let mut best: Option<&Tracker> = None;
            for tracker in self.trackers.values() {
                let mature = tracker.evidence_count >= required_evidence
                    || tracker.strong_count >= EARLY_COMMIT_REQUIRED_STRONG;
                let fits = tracker.raw_length > 0
                    && tracker.raw_length <= raw.len()
                    && raw.len() >= tracker.raw_length + retain
                    && tracker.text_char_count > 0;
                if mature && fits && best.is_none_or(|current| tracker_better(tracker, current)) {
                    best = Some(tracker);
                }
            }
            best.cloned()
        };
        let tracker = selected?;

        // Respect the spacing between two automatic commits, unless near the cap.
        if !near_cap && raw.len() < self.last_auto_commit_raw_length + retain {
            return None;
        }
        if tracker.text.is_empty() {
            return None;
        }
        Some(self.commit(raw, tracker.text, tracker.raw_length))
    }

    /// The reference `tryNearCapLastResort`: at the cap with no qualifying evidence, emit
    /// to the deepest word boundary that still leaves `retain` keys, so the key
    /// never leaks to the host.
    fn near_cap_last_resort(
        &mut self,
        raw: &str,
        decode: &SentenceDecode,
    ) -> Option<SentenceAutoCommitOutcome> {
        let retain = EARLY_COMMIT_RETAINED_RAW_LENGTH;
        if raw.len() < AUTO_COMMIT_MAX_RAW_LENGTH - NEAR_CAP_COMMIT_GENERATIONS
            || raw.len() < retain + 1
        {
            return None;
        }
        let top = decode.candidates.first()?;
        let limit = raw.len() - retain;
        // Boundaries ascend by raw length; take the deepest one at or below the
        // limit (the reference loop walks up the path from the end).
        let (consumed, text) = top
            .boundaries
            .iter()
            .rev()
            .find(|(raw_length, text)| {
                *raw_length > 0 && *raw_length <= limit && !text.is_empty()
            })?;
        Some(self.commit(raw, text.clone(), *consumed))
    }

    /// The shared commit bookkeeping for the early-commit paths: append to the
    /// committed text, advance the spacing marker, clear evidence, and report the
    /// retained tail.
    fn commit(
        &mut self,
        raw: &str,
        text: String,
        consumed: usize,
    ) -> SentenceAutoCommitOutcome {
        let retained_raw = raw[consumed..].to_string();
        self.last_auto_commit_raw_length = consumed;
        self.continuation = false;
        self.reset_evidence();
        self.empty_pending = None;
        SentenceAutoCommitOutcome { text, retained_raw }
    }

    // MARK: - Empty-code commit (reference capture_empty_code_candidate /
    // try_empty_code_commit).

    fn try_empty_code_commit(
        &mut self,
        engine: &SentenceEngine,
        raw: &str,
        context: &str,
    ) -> Option<SentenceAutoCommitOutcome> {
        if raw.len() < 2 {
            return None;
        }
        let full_before = &raw[..raw.len() - 1];
        let pending = match self.empty_pending.clone() {
            Some(pending) => pending,
            None => self.capture_empty_code_candidate(engine, full_before, context)?,
        };
        self.empty_pending = Some(pending.clone());

        // The extended code is still fully coverable → keep waiting.
        if has_complete_candidate(engine.lexicon(), raw, true) {
            self.empty_pending = None;
            return None;
        }
        if pending.base_raw_length >= raw.len() || pending.last_segment_start >= raw.len() {
            self.empty_pending = None;
            return None;
        }
        // The last segment could still extend into a real word → the user is
        // still typing, so do not truncate.
        let extended = &raw[pending.last_segment_start..];
        if engine.lexicon().is_proper_prefix(extended) {
            return None;
        }
        if pending.candidate_text.is_empty() {
            self.empty_pending = None;
            return None;
        }

        let commit = pending.candidate_text;
        let retained_raw = raw[pending.base_raw_length..].to_string();
        self.last_auto_commit_raw_length = pending.base_raw_length;
        self.trackers.clear();
        self.last_seen_raw.clear();
        self.suspended = false;
        self.empty_pending = None;
        self.continuation = true;
        Some(SentenceAutoCommitOutcome { text: commit, retained_raw })
    }

    /// The reference `captureEmptyCodeCandidate`: decode the previous code and keep its
    /// first eligible candidate (only the table's first choice unless an explicit
    /// selector was typed). When several are eligible the top must hold the
    /// strong share and match the visible first candidate.
    fn capture_empty_code_candidate(
        &self,
        engine: &SentenceEngine,
        full_before: &str,
        context: &str,
    ) -> Option<EmptyPending> {
        let decoded = engine.decode_with_evidence(full_before, context);
        let visible_top = decoded.candidates.first()?;
        let restrict = !has_selection_suffix(full_before.as_bytes());
        let first = decoded
            .candidates
            .iter()
            .find(|candidate| !restrict || candidate.max_rank <= 1)?;
        if first.text.is_empty() {
            return None;
        }

        if decoded.candidates.iter().filter(|c| !restrict || c.max_rank <= 1).count() > 1 {
            if decoded.evidence.confidence_truncated {
                return None;
            }
            if first.text != visible_top.text {
                return None;
            }
            let mut max_confidence = f64::NEG_INFINITY;
            let mut total = 0.0;
            let mut own = 0.0;
            for candidate in decoded.candidates.iter().filter(|c| !restrict || c.max_rank <= 1) {
                max_confidence = max_confidence.max(candidate.confidence);
            }
            for candidate in decoded.candidates.iter().filter(|c| !restrict || c.max_rank <= 1) {
                let mass = (candidate.confidence - max_confidence).exp();
                total += mass;
                if std::ptr::eq(candidate, first) {
                    own += mass;
                }
            }
            if total <= 0.0 || own / total < EARLY_COMMIT_STRONG_SHARE {
                return None;
            }
        }

        Some(EmptyPending {
            candidate_text: first.text.clone(),
            base_raw_length: full_before.len(),
            last_segment_start: first.prev_raw_length,
        })
    }
}

/// Reference `trackerBetter`: more characters, then a higher share, then an earlier
/// boundary.
fn tracker_better(left: &Tracker, right: &Tracker) -> bool {
    if left.text_char_count != right.text_char_count {
        return left.text_char_count > right.text_char_count;
    }
    if left.last_share != right.last_share {
        return left.last_share > right.last_share;
    }
    left.raw_length < right.raw_length
}

/// Whether the prefix is a node on some visible candidate's path (so the user
/// could actually see it) and that candidate starts with it (reference
/// `prefixBelongsToVisible`).
fn prefix_belongs_to_visible(prefix: &PrefixEvidence, visible: &[CompletedView]) -> bool {
    visible.iter().any(|candidate| {
        candidate.text.starts_with(&prefix.text)
            && candidate
                .boundaries
                .iter()
                .any(|(raw_length, text)| *raw_length == prefix.raw_length && *text == prefix.text)
    })
}

/// Hold existing trackers through a neutral generation without counting it,
/// dropping any contradicted by a better diverging prefix (reference
/// `retainTrackersWithoutCounting`).
fn retain_trackers_without_counting(
    trackers: &mut HashMap<String, Tracker>,
    prefixes: &[PrefixEvidence],
) {
    let mut next: HashMap<String, Tracker> = HashMap::new();
    for (key, mut tracker) in std::mem::take(trackers) {
        if prefix_contradicted(&tracker, prefixes) {
            continue;
        }
        let Some(current) = find_prefix_evidence(prefixes, &tracker.text, tracker.raw_length)
        else {
            continue;
        };
        tracker.gap_count += 1;
        if tracker.gap_count <= EARLY_COMMIT_MAXIMUM_NEUTRAL_GAP {
            tracker.last_share = current.share;
            next.insert(key, tracker);
        }
    }
    *trackers = next;
}

fn find_prefix_evidence<'a>(
    prefixes: &'a [PrefixEvidence],
    text: &str,
    raw_length: usize,
) -> Option<&'a PrefixEvidence> {
    prefixes.iter().find(|prefix| prefix.text == text && prefix.raw_length == raw_length)
}

/// A tracker is contradicted when a different, better-supported prefix shares a
/// shorter stem with it (a real divergence in how the sentence starts).
fn prefix_contradicted(tracker: &Tracker, prefixes: &[PrefixEvidence]) -> bool {
    if prefixes.is_empty() {
        return false;
    }
    let self_evidence = find_prefix_evidence(prefixes, &tracker.text, tracker.raw_length);
    let self_share = self_evidence.map_or(0.0, |prefix| prefix.share);
    let tracker_chars = tracker.text.chars().count();
    for prefix in prefixes {
        if prefix.text.is_empty() || prefix.text == tracker.text {
            continue;
        }
        // One being a prefix of the other is the same trajectory, not a conflict.
        if tracker.text.starts_with(&prefix.text) || prefix.text.starts_with(&tracker.text) {
            continue;
        }
        let stem = common_prefix_char_count(&prefix.text, &tracker.text);
        if stem > 0 && stem < tracker_chars && (self_evidence.is_none() || prefix.share > self_share)
        {
            return true;
        }
    }
    false
}

fn common_prefix_char_count(left: &str, right: &str) -> usize {
    left.chars().zip(right.chars()).take_while(|(l, r)| l == r).count()
}

/// Whether `raw` can be covered end to end by whole lexicon edges (reference
/// `has_complete_candidate`). It walks reachable code positions the same way the
/// decoder does, so a "no" means some tail is now unsegmentable.
fn has_complete_candidate(lexicon: &SentenceLexicon, raw: &str, allow_duplicate_single: bool) -> bool {
    let Some(bytes) = normalize(raw) else {
        return false;
    };
    let length = bytes.len();
    if length == 0 {
        return false;
    }
    let mut reachable = vec![false; length + 1];
    reachable[0] = true;
    for position in 0..length {
        if !reachable[position] {
            continue;
        }
        for &code_length in lexicon.lengths() {
            let end = position + code_length;
            if end > length {
                continue;
            }
            let (rank, consumed_end) = parse_selector(&bytes, end);
            if consumed_end > length {
                continue;
            }
            if length > 1 && consumed_end - position < 2 {
                continue;
            }
            let Ok(code) = std::str::from_utf8(&bytes[position..end]) else {
                continue;
            };
            let Some(edges) = lexicon.edges(code) else {
                continue;
            };
            let whole_input_edge = position == 0 && consumed_end == length;
            if !eligible_edges(edges, rank, whole_input_edge, allow_duplicate_single).is_empty() {
                reachable[consumed_end] = true;
            }
        }
    }
    reachable[length]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker(chars: usize, share: f64, raw_length: usize) -> Tracker {
        Tracker {
            text: String::new(),
            text_char_count: chars,
            raw_length,
            evidence_count: 0,
            strong_count: 0,
            gap_count: 0,
            last_share: share,
        }
    }

    fn prefix(text: &str, raw_length: usize, share: f64) -> PrefixEvidence {
        PrefixEvidence {
            text: text.to_string(),
            raw_length,
            share,
            boundary_share: share,
            boundary_closed: true,
            text_char_count: text.chars().count(),
        }
    }

    fn view(text: &str, boundaries: Vec<(usize, String)>) -> CompletedView {
        CompletedView {
            text: text.to_string(),
            confidence: 0.0,
            max_rank: 1,
            prev_raw_length: boundaries.first().map_or(0, |(n, _)| *n),
            boundaries,
        }
    }

    #[test]
    fn tracker_better_prefers_more_chars_then_share_then_earlier_boundary() {
        assert!(tracker_better(&tracker(3, 0.9, 5), &tracker(2, 0.99, 1)));
        assert!(tracker_better(&tracker(2, 0.99, 5), &tracker(2, 0.9, 1)));
        assert!(tracker_better(&tracker(2, 0.9, 1), &tracker(2, 0.9, 5)));
        assert!(!tracker_better(&tracker(1, 0.5, 3), &tracker(1, 0.5, 3)));
    }

    #[test]
    fn common_prefix_counts_chars() {
        assert_eq!(common_prefix_char_count("今天天", "今天气"), 2);
        assert_eq!(common_prefix_char_count("abc", "def"), 0);
        assert_eq!(common_prefix_char_count("", "x"), 0);
    }

    #[test]
    fn prefix_contradicted_flags_a_better_diverging_stem() {
        let t = tracker_with_text("今天天");
        // "今非…" shares a 1-char stem, is not a prefix of the tracker and carries
        // more share → a real divergence in how the sentence opens.
        let diverging = vec![prefix("今非昔比", 4, 0.9)];
        assert!(prefix_contradicted(&t, &diverging));
    }

    #[test]
    fn prefix_contradicted_tolerates_stems_and_empty_pools() {
        let t = tracker_with_text("今天天");
        // A prefix of the tracker (or vice versa) is the same trajectory.
        assert!(!prefix_contradicted(&t, &[prefix("今天", 2, 0.99)]));
        assert!(!prefix_contradicted(&t, &[]));
        // No shared stem → no conflict.
        assert!(!prefix_contradicted(&t, &[prefix("昨天", 2, 0.99)]));
    }

    #[test]
    fn prefix_belongs_to_visible_requires_the_boundary_on_a_matching_candidate() {
        let p = prefix("今天", 2, 0.9);
        let on_path = view("今天天气", vec![(2, "今天".into()), (6, "今天天气".into())]);
        assert!(prefix_belongs_to_visible(&p, &[on_path]));

        // Same text but the boundary node is absent → not on a visible path.
        let no_boundary = view("今天天气", vec![(6, "今天天气".into())]);
        assert!(!prefix_belongs_to_visible(&p, &[no_boundary]));

        // A candidate carrying the boundary but not starting with the prefix.
        let wrong_start = view("昨天天气", vec![(2, "今天".into())]);
        assert!(!prefix_belongs_to_visible(&p, &[wrong_start]));
    }

    #[test]
    fn backspace_invalidates_evidence_without_suspending() {
        let mut ac = SentenceAutoCommit::default();
        ac.evidence_invalidated();
        assert!(!ac.suspended, "backspace keeps auto-commit available");

        // Once suspended (paging), a later backspace does NOT lift the hold.
        ac.suspend();
        assert!(ac.suspended);
        ac.evidence_invalidated();
        assert!(ac.suspended, "invalidation never clears an explicit suspension");
    }

    #[test]
    fn reset_clears_suspension_and_evidence() {
        let mut ac = SentenceAutoCommit::default();
        ac.suspend();
        assert!(ac.suspended);
        ac.reset();
        assert!(!ac.suspended, "a whole-code commit clears the suspension");
        assert!(ac.trackers.is_empty() && ac.last_seen_raw.is_empty());
    }

    fn tracker_with_text(text: &str) -> Tracker {
        Tracker {
            text: text.to_string(),
            text_char_count: text.chars().count(),
            raw_length: text.len(),
            evidence_count: 0,
            strong_count: 0,
            gap_count: 0,
            last_share: 0.5,
        }
    }
}
