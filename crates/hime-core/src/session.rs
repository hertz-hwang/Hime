//! Session state machine (SPEC §3).
//!
//! `Session` is the only owner of typing state. It keeps the encoding and the
//! rendered candidate list, asks [`Processor`] what a key means and applies
//! the answer, and asks [`Pipeline`] for candidates whenever the encoding
//! changes. Nothing here knows where candidates come from, so adding a pinyin
//! source or a converter touches the pipeline and not this file.

use std::mem;
use std::sync::Arc;

use crate::config::{CodeType, Config};
use crate::dictionary::Dictionary;
use crate::diff::annotate_candidate_diffs;
use crate::pipeline::Pipeline;
use crate::processor::{Action, Processor};
use crate::punct::{self, QuotePair};
use crate::sentence::auto_commit::SentenceAutoCommit;
use crate::sentence::SentenceEngine;
use crate::types::{Candidate, EngineResponse};

/// 形码整句 auto-commit state plus the engine it re-decodes with. Present only
/// for a [`Session::sentence`] session; the auto-commit itself is inert unless
/// `Config::auto_commit` is on (SPEC §7.6).
#[derive(Debug)]
struct SentenceComposer {
    engine: Arc<SentenceEngine>,
    auto_commit: SentenceAutoCommit,
}

/// Which way a paging key turns the candidate list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageDirection {
    Previous,
    Next,
}

/// One typing session. All typing state lives here; shells just render the
/// [`EngineResponse`] they get back.
#[derive(Debug)]
pub struct Session {
    /// The lexicon behind a table pipeline; `None` for a
    /// [`Session::with_pipeline`] session, whose pipeline cannot be rebuilt.
    dict: Option<Arc<Dictionary>>,
    config: Config,
    pipeline: Pipeline,
    input_buffer: String,
    /// Every candidate for the encoding after the pipeline's filters: wider
    /// than one page, so paging has somewhere to go.
    window: Vec<Candidate>,
    /// Zero-based index into the pages of `window`; resets to 0 whenever the
    /// encoding changes. `candidates` is the page this selects.
    page: usize,
    /// The page `page` selects: the list the shell numbers 1.. and the one
    /// `Processor` validates digits and the paging keys against.
    candidates: Vec<Candidate>,
    /// Zero-based focused candidate within `candidates`; Space commits it.
    /// Resets to the first entry whenever the encoding or page changes, and
    /// stays clamped to the rendered page.
    focus: usize,
    /// Quote pairing for 中文标点; deliberately survives [`Session::clear`]
    /// so a quote opened before a commit still closes after it.
    quotes: QuotePair,
    /// Whether the text the engine last sent or released to the host ends in
    /// an ASCII letter or digit — the state the `english_punct_after_alnum`
    /// rule reads on the next idle punctuation key (SPEC §3.1). The engine
    /// only knows what crosses it, so the flag tracks exactly the keystrokes
    /// and commits it forwarded. Like [`Self::quotes`] it survives
    /// [`Session::clear`]: a committed `12` still arms the next punctuation.
    after_alnum: bool,
    /// 形码整句 probabilistic/empty-code auto-commit state; `None` for every
    /// non-sentence session, which keeps the sentence path fully gated.
    sentence: Option<SentenceComposer>,
}

impl Session {
    /// Session over a lexicon with the default configuration, using the default
    /// pipeline (table lookup, then de-duplicate, then cut the page).
    pub fn new(dict: Arc<Dictionary>) -> Self {
        Self::with_config(dict, Config::default())
    }

    /// Session over a lexicon with a user configuration (see
    /// [`Config`]). Values outside the supported range are clamped.
    pub fn with_config(dict: Arc<Dictionary>, config: Config) -> Self {
        let config = config.sanitized();
        let pipeline = Pipeline::table(Arc::clone(&dict));
        Self {
            dict: Some(dict),
            config,
            pipeline,
            input_buffer: String::new(),
            window: Vec::new(),
            page: 0,
            candidates: Vec::new(),
            focus: 0,
            quotes: QuotePair::default(),
            after_alnum: false,
            sentence: None,
        }
    }

    /// Session over a custom pipeline, for a shell or test that wants to pick
    /// the candidate sources and post-processing itself.
    pub fn with_pipeline(pipeline: Pipeline) -> Self {
        Self {
            dict: None,
            config: Config::default(),
            pipeline,
            input_buffer: String::new(),
            window: Vec::new(),
            page: 0,
            candidates: Vec::new(),
            focus: 0,
            quotes: QuotePair::default(),
            after_alnum: false,
            sentence: None,
        }
    }

    /// Session for 形码整句: encodings decode to whole sentences through
    /// `engine` instead of being looked up as words. This switches the key
    /// model to sentence mode — a code runs to the decoder's length limit and
    /// never overflow-commits — by forcing
    /// [`Config::code_type`] to [`CodeType::XingmaSentence`]; every other
    /// knob (page size, punctuation) comes from `config`. Paging, digit
    /// select and Space then list and commit the sentences exactly like table
    /// candidates.
    pub fn sentence(engine: Arc<SentenceEngine>, config: Config) -> Self {
        let config = Config { code_type: CodeType::XingmaSentence, ..config }.sanitized();
        let pipeline = Pipeline::sentence(Arc::clone(&engine));
        Self {
            dict: None,
            config,
            pipeline,
            input_buffer: String::new(),
            window: Vec::new(),
            page: 0,
            candidates: Vec::new(),
            focus: 0,
            quotes: QuotePair::default(),
            after_alnum: false,
            sentence: Some(SentenceComposer {
                engine,
                auto_commit: SentenceAutoCommit::default(),
            }),
        }
    }

    /// The configuration this session applies to keys and pages.
    #[must_use]
    pub fn config(&self) -> Config {
        self.config
    }

    /// Switch to another lexicon (a scheme change): rebuilds the table
    /// pipeline under the current config and drops the in-progress
    /// composition. Sessions built with [`Session::with_pipeline`] keep a
    /// custom chain they own; here the pipeline is the table chain again.
    pub fn set_dict(&mut self, dict: Arc<Dictionary>) {
        self.pipeline = Pipeline::table(Arc::clone(&dict));
        self.dict = Some(dict);
        self.clear();
    }

    /// Apply a new configuration while live. Out-of-range values are
    /// clamped. The retrieval chain no longer depends on the page size, so
    /// only the page is re-cut for the new size (clamping the page cursor); a
    /// change made mid-composition takes effect on the next rendered response.
    pub fn set_config(&mut self, config: Config) {
        self.config = config.sanitized();
        self.apply_page();
    }

    /// Feed one key (see the `KEY_*` constants and ASCII codes in
    /// `hime_core`) and return the full state the shell must render.
    ///
    /// While idle, letters and — in the 中文标点 mode of
    /// [`Config::ascii_punct`] — mapped punctuation keys are consumed; every
    /// other key returns `handled: false`. Once composing, every key returns
    /// `handled: true` except printable punctuation the punctuation mode
    /// releases, which keeps `handled: false` so the character reaches the
    /// host; unrecognized keys (letters over the cap, digits without a
    /// candidate behind them, any other key code) are swallowed as no-ops so
    /// the host's Marked Text keeps running. The paging keys `-`/`=` are the
    /// one page-control exception: while a candidate is shown they turn the
    /// page and are always consumed, so the digits keep selecting inside
    /// whichever page is currently rendered.
    pub fn process_key(&mut self, key: u32) -> EngineResponse {
        let decision = Processor::decide(
            key,
            &self.input_buffer,
            self.candidates.len(),
            self.config,
            self.after_alnum,
        );
        let response = match decision {
            Action::Pass => self.response(false, None),
            Action::Noop => self.response(true, None),
            Action::AppendCode(byte) => self.push_letter(byte, None),
            Action::OverflowCommit(byte) => {
                // The full encoding's best candidate (or the letters
                // themselves) is committed; the pressing letter starts the
                // next encoding and stays visible in the preedit.
                let commit = self
                    .candidates
                    .first()
                    .map(|candidate| candidate.text.clone())
                    .or_else(|| Some(mem::take(&mut self.input_buffer)));
                self.clear();
                self.push_letter(byte, commit)
            }
            Action::DeleteCode => {
                self.input_buffer.pop();
                self.refresh_candidates();
                if let Some(composer) = self.sentence.as_mut() {
                    composer.auto_commit.evidence_invalidated();
                }
                self.response(true, None)
            }
            Action::Discard => {
                self.clear();
                self.response(true, None)
            }
            Action::CommitRaw => {
                let commit = mem::take(&mut self.input_buffer);
                self.clear();
                self.response(true, Some(commit))
            }
            Action::CommitPunct(byte) => match punct::map(byte, &mut self.quotes) {
                Some(text) => self.response(true, Some(text.to_owned())),
                // Processor gates on the same table, so this only guards
                // the two from ever disagreeing.
                None => self.response(false, None),
            },
            Action::CommitCandidatePunct(byte) => {
                let Some(punctuation) = punct::map(byte, &mut self.quotes) else {
                    return self.response(true, None);
                };
                // Same commit choice as Space: the focused candidate, or the
                // letters themselves when nothing matches.
                let mut commit = self
                    .candidates
                    .get(self.focus)
                    .or_else(|| self.candidates.first())
                    .map(|candidate| candidate.text.clone())
                    .or_else(|| Some(mem::take(&mut self.input_buffer)))
                    .unwrap_or_default();
                commit.push_str(punctuation);
                self.clear();
                self.response(true, Some(commit))
            }
            Action::Select(index) => self.commit_candidate(index),
            Action::SelectFocused => self.commit_candidate(self.focus),
            Action::FocusPrevious => {
                if !self.candidates.is_empty() {
                    self.focus = self.focus.saturating_sub(1);
                }
                self.suspend_sentence_auto_commit();
                self.response(true, None)
            }
            Action::FocusNext => {
                if let Some(last) = self.candidates.len().checked_sub(1) {
                    self.focus = (self.focus + 1).min(last);
                }
                self.suspend_sentence_auto_commit();
                self.response(true, None)
            }
            Action::PageUp => {
                self.turn_page(PageDirection::Previous);
                self.suspend_sentence_auto_commit();
                self.response(true, None)
            }
            Action::PageDown => {
                self.turn_page(PageDirection::Next);
                self.suspend_sentence_auto_commit();
                self.response(true, None)
            }
        };
        self.track_after_alnum(key, &response);
        response
    }

    /// Records whether the text now sitting at the host's caret ends in an
    /// English letter or digit, the state [`Config::english_punct_after_alnum`]
    /// reads (SPEC §3.1). Only events that put a character at the caret move
    /// it: a commit updates from the committed text's last character, and a
    /// key the engine released (`handled: false`) updates from that key.
    /// Composition-only events (append, delete, page, no-op) leave the host
    /// caret untouched, so the flag stands — a released digit keeps it armed,
    /// the punctuation that consumes it (or any Chinese commit) clears it.
    fn track_after_alnum(&mut self, key: u32, response: &EngineResponse) {
        self.after_alnum = match &response.commit_text {
            Some(text) => text.chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric()),
            None if !response.handled => match u8::try_from(key) {
                Ok(byte) if byte.is_ascii_alphanumeric() => true,
                Ok(byte) if byte == b' ' || punct::is_symbol(byte) => false,
                // Control keys (Backspace, Enter, arrows, …) hide what the
                // caret now holds: the flag keeps its value, and a stale one
                // can only ever release one ASCII key before disarming.
                _ => return,
            },
            None => return,
        };
    }

    /// Append one letter, re-retrieve, then apply the auto-commit rule: with
    /// the setting on, once the encoding is at least `auto_commit_length`
    /// letters and exactly one candidate remains, that candidate is committed
    /// right away. `pending` carries an already-earned commit (from overflow)
    /// that joins this response in front of the auto-committed text.
    ///
    /// The rule triggers on appended letters only; Backspace or a config
    /// change that happens to leave one candidate does not commit.
    fn push_letter(&mut self, byte: u8, pending: Option<String>) -> EngineResponse {
        self.input_buffer.push(char::from(byte));
        self.refresh_candidates();
        // 形码整句 replaces the unique-candidate rule with the probabilistic
        // auto-commit; the word-code rule below only applies to table sessions.
        if self.sentence.is_some() {
            return self.sentence_auto_commit(pending);
        }
        if self.config.auto_commit
            && self.input_buffer.len() >= self.config.auto_commit_length
            && self.candidates.len() == 1
        {
            let text = self.candidates[0].text.clone();
            self.clear();
            let commit = match pending {
                Some(mut committed) => {
                    committed.push_str(&text);
                    committed
                }
                None => text,
            };
            return self.response(true, Some(commit));
        }
        self.response(true, pending)
    }

    /// 形码整句: after a letter extends the code, ask the auto-commit state
    /// machine whether a matured prefix or a now-uncoverable tail should go to
    /// the host now (SPEC §7.6). On a hit, the fragment is emitted and the
    /// still-unconsumed tail stays in the composition, so typing continues
    /// straight into the next character. `pending` carries an overflow commit,
    /// which sentence mode never produces, so it stays `None` in practice.
    fn sentence_auto_commit(&mut self, pending: Option<String>) -> EngineResponse {
        let commit = {
            let composer = self
                .sentence
                .as_mut()
                .expect("sentence session owns the auto-commit state");
            composer.auto_commit.key_pressed(
                composer.engine.as_ref(),
                &self.input_buffer,
                true,
                self.config.auto_commit,
            )
        };
        match commit {
            Some(outcome) => {
                self.input_buffer = outcome.retained_raw;
                self.refresh_candidates();
                let text = match pending {
                    Some(mut committed) => {
                        committed.push_str(&outcome.text);
                        committed
                    }
                    None => outcome.text,
                };
                self.response(true, Some(text))
            }
            None => self.response(true, pending),
        }
    }

    /// Commit candidate `index` if the rendered page holds it; otherwise keep
    /// composing rather than commit the wrong text.
    fn commit_candidate(&mut self, index: usize) -> EngineResponse {
        match self.candidates.get(index) {
            Some(candidate) => {
                let commit = candidate.text.clone();
                self.clear();
                self.response(true, Some(commit))
            }
            None => self.response(true, None),
        }
    }

    /// Hold auto-commit for the rest of the composition (paging, focus
    /// movement): only meaningful for a sentence session.
    fn suspend_sentence_auto_commit(&mut self) {
        if let Some(composer) = self.sentence.as_mut() {
            composer.auto_commit.suspend();
        }
    }

    /// Re-retrieve the whole candidate window for the current encoding and
    /// show its first page: an encoding change always restarts at the top,
    /// with the first candidate focused.
    fn refresh_candidates(&mut self) {
        self.window = if self.input_buffer.is_empty() {
            Vec::new()
        } else {
            self.pipeline.retrieve(&self.input_buffer)
        };
        self.page = 0;
        self.focus = 0;
        self.apply_page();
    }

    /// Re-cut [`Self::candidates`] from [`Self::window`] for the current page,
    /// clamping the page cursor into the range the window can offer.
    fn apply_page(&mut self) {
        let size = self.config.page_size;
        let last_page = self.window.len().saturating_sub(1) / size;
        self.page = self.page.min(last_page);
        let start = self.page * size;
        let end = (start + size).min(self.window.len());
        self.candidates = self.window[start..end].to_vec();
        // A config change can resize the page under a standing focus; keep
        // the index inside the entries the shell renders.
        self.focus = if self.candidates.is_empty() {
            0
        } else {
            self.focus.min(self.candidates.len() - 1)
        };
        // Whole-sentence candidates share most of their text with the page's
        // top candidate; describe each one relative to it so the shell colors
        // substitutions, deletions and insertions without any logic of its own.
        if self.sentence.is_some() {
            annotate_candidate_diffs(&mut self.candidates);
        }
    }

    /// Move the page cursor, clamped to the first and last page: turning past
    /// an end holds on it, so the paging keys never leave the list.
    fn turn_page(&mut self, direction: PageDirection) {
        match direction {
            PageDirection::Previous => self.page = self.page.saturating_sub(1),
            PageDirection::Next => self.page = self.page.saturating_add(1),
        }
        self.focus = 0;
        self.apply_page();
    }

    fn clear(&mut self) {
        self.input_buffer.clear();
        self.window.clear();
        self.page = 0;
        self.candidates.clear();
        self.focus = 0;
        // A whole-code commit or abandon ends the composition: drop all
        // auto-commit state except the committed-fragment context.
        if let Some(composer) = self.sentence.as_mut() {
            composer.auto_commit.reset();
        }
    }

    /// The encoding to display: the first candidate's word-separated code
    /// split when it covers the raw encoding, otherwise the raw letters. The
    /// split covers the whole code for a whole-sentence candidate, so the
    /// shown string is an automatic segmentation of exactly what was typed.
    fn code_line(&self) -> String {
        let Some(first) = self.candidates.first() else {
            return self.input_buffer.clone();
        };
        if first.code.is_empty() {
            return self.input_buffer.clone();
        }
        let compact: String = first.code.split_ascii_whitespace().collect();
        if compact == self.input_buffer {
            first.code.clone()
        } else {
            self.input_buffer.clone()
        }
    }

    /// Pages the retrieved window spans; 0 while nothing was retrieved.
    fn page_count(&self) -> usize {
        if self.window.is_empty() {
            0
        } else {
            self.window.len().div_ceil(self.config.page_size)
        }
    }

    /// Bundle current session state into the response a shell renders as-is.
    fn response(&self, handled: bool, commit_text: Option<String>) -> EngineResponse {
        EngineResponse {
            handled,
            commit_text,
            preedit: self.input_buffer.clone(),
            code_line: self.code_line(),
            code_in_window: self.config.show_code_in_window,
            candidates: self.candidates.clone(),
            focus: self.focus,
            page: if self.window.is_empty() { 0 } else { self.page + 1 },
            page_count: self.page_count(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        KEY_ARROW_DOWN, KEY_ARROW_LEFT, KEY_ARROW_RIGHT, KEY_ARROW_UP, KEY_BACKSPACE, KEY_ENTER,
        KEY_ESCAPE, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_SPACE, MAX_CODE_LEN, MAX_CANDIDATES,
    };

    const SAMPLE: &str = "a\t工\t100\n\
                          a\t戈\t90\n\
                          aa\t式\t100\n\
                          ad\t匿\t100\n\
                          g\t王\t100\n\
                          gg\t五\t100\n";

    /// Same words reachable from longer encodings, which is what produced the
    /// candidate window that showed keys 2, 3 and 5 with 1 and 4 blank.
    const REPEATED_TEXT: &str = "a\t工\t100\n\
                                 a\t戈\t90\n\
                                 aa\t式\t100\n\
                                 aaa\t工\t100\n\
                                 aaaa\t工\t100\n\
                                 aaaa\t㠭\t80\n\
                                 ad\t匿\t100\n";

    const KEY_A: u32 = 97;
    const KEY_G: u32 = 103;
    const KEY_Q: u32 = 113;
    const KEY_1: u32 = 49;
    const KEY_2: u32 = 50;
    const KEY_3: u32 = 51;
    const KEY_9: u32 = 57;

    fn session() -> Session {
        Session::new(Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses")))
    }

    /// The flow required by SPEC: `a` -> `Backspace` -> `a` -> `Space`
    /// must commit "工" and leave the session idle.
    #[test]
    fn retype_after_backspace_then_space_commits_first_candidate() {
        let mut s = session();

        let r = s.process_key(KEY_A);
        assert!(r.handled);
        assert!(r.commit_text.is_none());
        assert_eq!(r.preedit, "a");
        assert_eq!(r.candidates[0], Candidate::new("工", ""));

        let r = s.process_key(KEY_BACKSPACE);
        assert!(r.handled);
        assert!(r.commit_text.is_none());
        assert_eq!(r.preedit, "");
        assert!(r.candidates.is_empty());

        let r = s.process_key(KEY_A);
        assert!(r.handled);
        assert_eq!(r.preedit, "a");
        assert_eq!(r.candidates[0].text, "工");

        let r = s.process_key(KEY_SPACE);
        assert!(r.handled);
        assert_eq!(r.commit_text.as_deref(), Some("工"));
        assert_eq!(r.preedit, "");
        assert!(r.candidates.is_empty());

        // Idle again: space now belongs to the application.
        let r = s.process_key(KEY_SPACE);
        assert!(!r.handled);
        assert!(r.commit_text.is_none());
    }

    #[test]
    fn idle_passes_through_non_letters() {
        let mut s = session();
        for key in [KEY_SPACE, KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, KEY_1, KEY_9,
                    KEY_ARROW_LEFT, KEY_ARROW_RIGHT, KEY_ARROW_UP, KEY_ARROW_DOWN,
                    b'-' as u32, b',' as u32, b'"' as u32] {
            let r = s.process_key(key);
            assert!(!r.handled, "idle key {key} must pass through");
            assert!(r.commit_text.is_none());
            assert_eq!(r.preedit, "");
            assert!(r.candidates.is_empty());
        }
    }

    #[test]
    fn idle_letter_opens_composition_with_prefix_comments() {
        let mut s = session();
        let r = s.process_key(KEY_A);
        assert!(r.handled);
        assert_eq!(
            r.candidates,
            vec![
                Candidate::new("工", ""),
                Candidate::new("戈", ""),
                Candidate::new("式", "~a"),
                Candidate::new("匿", "~d"),
            ]
        );
    }

    /// The candidate list the shell numbers 1..=N must contain each text once,
    /// so a key label always names one word.
    #[test]
    fn candidate_list_offers_each_text_once_and_fills_the_page() {
        let mut s = Session::new(Arc::new(
            Dictionary::parse_str(REPEATED_TEXT).expect("sample parses"),
        ));
        let r = s.process_key(KEY_A);
        let texts: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["工", "戈", "式", "匿", "㠭"]);
        assert_eq!(
            texts.iter().collect::<std::collections::HashSet<_>>().len(),
            texts.len(),
            "duplicate text in {texts:?}"
        );
    }

    #[test]
    fn letters_keep_narrowing_candidates() {
        let mut s = session();
        s.process_key(KEY_A);
        let r = s.process_key(b'd' as u32);
        assert!(r.handled);
        assert_eq!(r.preedit, "ad");
        assert_eq!(r.candidates, vec![Candidate::new("匿", "")]);
    }

    #[test]
    fn escape_clears_without_committing() {
        let mut s = session();
        s.process_key(KEY_G);
        let r = s.process_key(KEY_ESCAPE);
        assert!(r.handled);
        assert!(r.commit_text.is_none());
        assert_eq!(r.preedit, "");
        assert!(r.candidates.is_empty());
    }

    /// A horizontal candidate row moves its focus with Left/Right; the
    /// vertical pair stays inert, and Space commits whichever entry has the
    /// focus instead of always the first.
    #[test]
    fn horizontal_arrows_focus_and_space_commits_the_focused_candidate() {
        let mut s = session();
        let r = s.process_key(KEY_A);
        assert_eq!(r.focus, 0, "the first candidate starts focused");

        let r = s.process_key(KEY_ARROW_RIGHT);
        assert!(r.handled);
        assert_eq!(r.focus, 1);
        assert_eq!(r.preedit, "a", "moving focus leaves the encoding alone");

        let r = s.process_key(KEY_ARROW_LEFT);
        assert_eq!(r.focus, 0);
        let r = s.process_key(KEY_ARROW_LEFT);
        assert_eq!(r.focus, 0, "focus holds on the first candidate");

        let r = s.process_key(KEY_ARROW_RIGHT);
        assert_eq!(r.focus, 1);
        for key in [KEY_ARROW_UP, KEY_ARROW_DOWN] {
            let r = s.process_key(key);
            assert!(r.handled, "the orthogonal axis is swallowed");
            assert_eq!(r.focus, 1, "and does not move the focus");
        }

        let r = s.process_key(KEY_SPACE);
        assert_eq!(r.commit_text.as_deref(), Some("戈"), "Space commits the focus");
        assert_eq!(r.focus, 0, "a commit resets the focus");
    }

    /// A vertical candidate column swaps the axis: Up/Down move, Left/Right
    /// are swallowed. Moving past the last entry holds there.
    #[test]
    fn vertical_arrows_focus_and_clamp_to_the_page() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(dict, Config { vertical_candidates: true, ..Config::default() });
        let r = s.process_key(KEY_A);
        assert_eq!(r.candidates.len(), 4);

        let r = s.process_key(KEY_ARROW_DOWN);
        assert_eq!(r.focus, 1);
        let r = s.process_key(KEY_ARROW_UP);
        assert_eq!(r.focus, 0);
        for key in [KEY_ARROW_LEFT, KEY_ARROW_RIGHT] {
            let r = s.process_key(key);
            assert!(r.handled);
            assert_eq!(r.focus, 0, "the horizontal pair is inert in a vertical column");
        }
        for _ in 0..10 {
            s.process_key(KEY_ARROW_DOWN);
        }
        let r = s.process_key(KEY_SPACE);
        assert_eq!(r.commit_text.as_deref(), Some("匿"), "focus stops on the last candidate");
    }

    /// Any change to the rendered page starts the focus over: a new encoding,
    /// Backspace, and paging all focus the first entry of the new list.
    #[test]
    fn focus_resets_when_the_candidate_page_changes() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(dict, Config { page_size: 2, ..Config::default() });
        s.process_key(KEY_A);
        assert_eq!(s.process_key(KEY_ARROW_RIGHT).focus, 1);

        let r = s.process_key(b'd' as u32); // "ad" narrows to 匿
        assert_eq!(r.focus, 0);
        assert_eq!(r.candidates[0].text, "匿");

        let r = s.process_key(KEY_BACKSPACE); // back to "a"
        assert_eq!(r.focus, 0);
        assert_eq!(r.candidates[0].text, "工");
        s.process_key(KEY_ARROW_RIGHT);
        let r = s.process_key(KEY_PAGE_DOWN);
        assert_eq!(r.page, 2);
        assert_eq!(r.focus, 0, "turning the page focuses its first candidate");
        assert_eq!(r.candidates[0].text, "式");
    }

    /// The 中文标点 commit shares Space's choice: a focused candidate takes
    /// the punctuation instead of the first entry.
    #[test]
    fn punctuation_follows_the_focused_candidate() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(dict, Config { ascii_punct: 1, ..Config::default() });
        s.process_key(KEY_A);
        s.process_key(KEY_ARROW_RIGHT);
        let r = s.process_key(b',' as u32);
        assert_eq!(r.commit_text.as_deref(), Some("戈，"));
        assert_eq!(r.preedit, "");
    }

    #[test]
    fn space_without_candidates_commits_raw_encoding() {
        let mut s = session();
        let r = s.process_key(KEY_Q); // 'q' has no entry in the sample
        assert!(r.handled);
        assert_eq!(r.preedit, "q");
        assert!(r.candidates.is_empty());

        let r = s.process_key(KEY_SPACE);
        assert!(r.handled);
        assert_eq!(r.commit_text.as_deref(), Some("q"));

        let r = s.process_key(KEY_SPACE);
        assert!(!r.handled);
    }

    #[test]
    fn digits_select_candidates_by_index() {
        for (digit, expected) in [(KEY_1, "工"), (KEY_2, "戈"), (KEY_3, "式")] {
            let mut s = session();
            s.process_key(KEY_A);
            let r = s.process_key(digit);
            assert!(r.handled, "digit {digit} must be handled");
            assert_eq!(r.commit_text.as_deref(), Some(expected));
            assert_eq!(r.preedit, "");
        }
    }

    #[test]
    fn digit_beyond_candidate_list_is_swallowed() {
        let mut s = session();
        s.process_key(KEY_A);
        s.process_key(b'd' as u32); // "ad" has exactly one candidate: 匿
        let r = s.process_key(KEY_2);
        assert!(r.handled, "a digit with no candidate must be swallowed");
        assert!(r.commit_text.is_none());
        assert_eq!(r.preedit, "ad");
        assert_eq!(r.candidates, vec![Candidate::new("匿", "")]);
        // '9' can never map to a candidate (list is capped at 5): swallowed too.
        let r = s.process_key(KEY_9);
        assert!(r.handled);
        assert_eq!(r.preedit, "ad");
        // The composition survives; space still commits the candidate.
        let r = s.process_key(KEY_SPACE);
        assert_eq!(r.commit_text.as_deref(), Some("匿"));
    }

    #[test]
    fn enter_commits_raw_letters_and_resets() {
        let mut s = session();
        s.process_key(KEY_G);
        s.process_key(KEY_G); // "gg" has candidates, yet Enter commits the letters
        let r = s.process_key(KEY_ENTER);
        assert!(r.handled);
        assert_eq!(r.commit_text.as_deref(), Some("gg"));
        assert_eq!(r.preedit, "");
        assert!(r.candidates.is_empty());
    }

    /// `commit_next_code`: the N-th letter commits the first N-1 letters'
    /// candidate and stays in the preedit as the start of the next encoding.
    #[test]
    fn overflow_commits_the_candidate_and_keeps_the_pressing_letter() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s =
            Session::with_config(dict, Config { commit_next_code: 3, ..Config::default() });
        s.process_key(KEY_G);
        let r = s.process_key(KEY_G); // "gg" = 五
        assert_eq!(r.candidates[0].text, "五");

        let r = s.process_key(KEY_A);
        assert_eq!(r.commit_text.as_deref(), Some("五"), "the 3rd letter commits gg");
        assert_eq!(r.preedit, "a", "the pressing letter starts the next encoding");
        assert_eq!(r.candidates[0].text, "工");

        let r = s.process_key(KEY_SPACE);
        assert_eq!(r.commit_text.as_deref(), Some("工"), "the new encoding types on");
    }

    /// With no candidate behind the full encoding, overflow commits the
    /// letters as typed — the same fallback Space performs.
    #[test]
    fn overflow_without_candidates_commits_raw_letters() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(
            dict,
            Config { max_code_len: 4, commit_next_code: 5, ..Config::default() },
        );
        for key in [KEY_G, KEY_G, KEY_G, KEY_G] {
            s.process_key(key);
        }
        let r = s.process_key(KEY_A);
        assert_eq!(r.commit_text.as_deref(), Some("gggg"));
        assert_eq!(r.preedit, "a");
    }

    #[test]
    fn encoding_capped_at_max_code_len() {
        let mut s = session();
        for _ in 0..MAX_CODE_LEN {
            let r = s.process_key(KEY_G);
            assert!(r.handled);
        }
        assert_eq!(s.input_buffer, "gggg");
        let r = s.process_key(KEY_G);
        assert!(r.handled, "a letter over the cap must be swallowed");
        assert!(r.commit_text.is_none());
        assert_eq!(r.preedit, "gggg");
        assert_eq!(s.input_buffer.len(), MAX_CODE_LEN);
        // The full encoding still behaves normally.
        let r = s.process_key(KEY_ENTER);
        assert_eq!(r.commit_text.as_deref(), Some("gggg"));
    }

    #[test]
    fn undefined_keys_are_swallowed_while_composing() {
        let mut s = session();
        s.process_key(KEY_A);
        for key in [b'0' as u32, 0, 0x1000] {
            let r = s.process_key(key);
            assert!(r.handled, "key {key} must be swallowed while composing");
            assert!(r.commit_text.is_none());
            assert_eq!(r.preedit, "a");
            assert!(!r.candidates.is_empty());
        }
        // Printable punctuation is the exception: English mode releases it
        // to the host even mid-composition, without disturbing the encoding.
        let r = s.process_key(b',' as u32);
        assert!(!r.handled, "English punctuation must reach the host");
        assert_eq!(r.preedit, "a");
        // The composition is untouched and still functional.
        let r = s.process_key(KEY_SPACE);
        assert_eq!(r.commit_text.as_deref(), Some("工"));
    }

    #[test]
    fn response_json_is_a_stable_contract() {
        let mut s = session();
        s.process_key(KEY_A);
        let r = s.process_key(KEY_SPACE);
        let json = serde_json::to_string(&r).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        for field in [
            "handled",
            "commit_text",
            "preedit",
            "code_line",
            "code_in_window",
            "candidates",
            "focus",
            "page",
            "page_count",
        ] {
            assert!(v.get(field).is_some(), "missing field {field} in {json}");
        }
        assert_eq!(v["handled"], serde_json::json!(true));
        assert_eq!(v["commit_text"], serde_json::json!("工"));
        assert_eq!(v["preedit"], serde_json::json!(""));
        assert_eq!(v["code_line"], serde_json::json!(""));
        assert_eq!(v["code_in_window"], serde_json::json!(false));
        assert_eq!(v["candidates"], serde_json::json!([]));
        assert_eq!(v["focus"], serde_json::json!(0));
        assert_eq!(v["page"], serde_json::json!(0));
        assert_eq!(v["page_count"], serde_json::json!(0));

        let back: EngineResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    /// A custom chain reaches the user without any change to the state
    /// machine, which is the point of the pipeline split.
    #[test]
    fn a_custom_pipeline_drives_the_same_state_machine() {
        struct OnlyStar;
        impl crate::translator::Translator for OnlyStar {
            fn translate(&self, input: &str, _limit: usize) -> Vec<Candidate> {
                if input == "x" {
                    vec![Candidate::new("星", "")]
                } else {
                    Vec::new()
                }
            }
        }
        let pipeline = Pipeline::new(vec![Box::new(OnlyStar)], vec![]);
        let mut s = Session::with_pipeline(pipeline);
        assert_eq!(s.process_key(b'x' as u32).candidates[0].text, "星");
        assert_eq!(s.process_key(KEY_SPACE).commit_text.as_deref(), Some("星"));
    }

    /// A candidate that carries a word-separated code split covering the raw
    /// encoding makes the response's display line that split; a table match
    /// without one shows the raw letters. The page cursor is reported
    /// one-based with the number of pages the window spans.
    #[test]
    fn code_line_is_segmented_and_pages_are_reported() {
        struct Segmented;
        impl crate::translator::Translator for Segmented {
            fn translate(&self, input: &str, _limit: usize) -> Vec<Candidate> {
                if input == "pms" {
                    vec![Candidate {
                        text: "嗨".into(),
                        comment: String::new(),
                        code: "p m s".into(),
                        spans: Vec::new(),
                    }]
                } else {
                    Vec::new()
                }
            }
        }
        let mut s = Session::with_pipeline(Pipeline::new(vec![Box::new(Segmented)], vec![]));
        for key in [b'p' as u32, b'm' as u32, b's' as u32] {
            s.process_key(key);
        }
        let r = s.process_key(b's' as u32); // "pmss": no candidate, raw line
        assert_eq!(r.code_line, "pmss");
        assert_eq!(r.page, 0);
        assert_eq!(r.page_count, 0);

        let r = {
            // A fresh session lands on the candidate again.
            let mut t = Session::with_pipeline(Pipeline::new(vec![Box::new(Segmented)], vec![]));
            for key in [b'p' as u32, b'm' as u32, b's' as u32] {
                t.process_key(key);
            }
            let r = t.process_key(KEY_PAGE_DOWN); // one page only: stays
            assert_eq!(r.code_line, "p m s", "the candidate supplies the split");
            r
        };
        assert_eq!(r.page, 1);
        assert_eq!(r.page_count, 1);

        // A table session has no code split and reports its raw encoding.
        let mut table = session();
        let r = table.process_key(KEY_A);
        assert_eq!(r.code_line, "a");
        assert_eq!(r.page, 1);
        assert_eq!(r.page_count, 1);
    }

    /// What the preferences window promises: a smaller page means fewer
    /// rendered candidates and fewer digit keys that reach one.
    #[test]
    fn config_shrinks_the_page_and_the_digit_keys() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(dict, Config { page_size: 2, max_code_len: 2, ..Config::default() });
        let r = s.process_key(KEY_A);
        let texts: Vec<&str> = r.candidates.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, vec!["工", "戈"]);

        let r = s.process_key(KEY_3);
        assert!(r.handled, "3 is swallowed while the page only has 2 keys");
        assert!(r.commit_text.is_none());
        assert_eq!(r.preedit, "a");

        let r = s.process_key(KEY_2);
        assert_eq!(r.commit_text.as_deref(), Some("戈"));
    }

    /// A config pushed into a live session must reach the next response,
    /// including one typed mid-composition. Shrinking the page to 1 also
    /// proves the paging keys follow the new page size.
    #[test]
    fn set_config_takes_effect_mid_composition() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::new(dict);
        let r = s.process_key(KEY_A);
        assert_eq!(r.candidates.len(), 4);

        s.set_config(Config { page_size: 1, max_code_len: 1, ..Config::default() });
        let r = s.process_key(KEY_PAGE_DOWN); // re-renders one page, then turns it
        assert_eq!(r.candidates.len(), 1, "the new page size holds");
        assert_eq!(r.candidates[0].text, "戈", "= turns to the next 1-candidate page");

        let r = s.process_key(KEY_PAGE_UP);
        assert_eq!(r.candidates[0].text, "工", "- turns back to the first page");

        let r = s.process_key(KEY_G);
        assert_eq!(r.preedit, "a", "the code cap of 1 swallows further letters");
        let r = s.process_key(KEY_1);
        assert_eq!(r.commit_text.as_deref(), Some("工"));
    }

    /// Paging walks the whole window: `-`/`=` move a page, the window resets
    /// to the first page when the encoding changes, and turning past an end
    /// holds there. The fixture is the shipped default scheme (琉璃v1), whose
    /// `d` encoding returns far more candidates than one page.
    #[test]
    fn paging_walks_pages_and_digits_select_within_the_page() {
        let dict = Arc::new(Dictionary::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/schemes/liuli_v1/liuli_table.txt"
        ))
        .expect("default scheme table must load"));
        let mut s = Session::new(Arc::clone(&dict));

        let first = s.process_key(b'd' as u32);
        assert_eq!(first.candidates.len(), MAX_CANDIDATES, "the first page is full");
        assert_eq!(first.candidates[0].text, "的", "琉璃v1 ranks 的 first behind d");
        assert_eq!(first.page, 1, "the page cursor is reported one-based");
        assert!(first.page_count > 1, "the window spans more than one page");

        // Digits keep addressing the page the shell renders, never a later one.
        assert_eq!(
            s.process_key(KEY_2).commit_text.as_deref(),
            Some(first.candidates[1].text.as_str())
        );

        let second = {
            let mut t = Session::new(Arc::clone(&dict));
            t.process_key(b'd' as u32);
            t.process_key(KEY_PAGE_DOWN)
        };
        assert_eq!(second.candidates.len(), MAX_CANDIDATES, "the second page is full");
        assert_eq!(second.page, 2, "the page cursor is reported one-based");
        assert_eq!(second.preedit, "d", "paging leaves the encoding untouched");
        assert_ne!(
            second.candidates[0].text, first.candidates[0].text,
            "page 2 shows different candidates"
        );

        // Paging past the first page then back returns to the first page.
        let back = {
            let mut t = Session::new(Arc::clone(&dict));
            t.process_key(b'd' as u32);
            t.process_key(KEY_PAGE_DOWN);
            t.process_key(KEY_PAGE_UP)
        };
        assert_eq!(back.candidates, first.candidates, "- returns to the first page");

        // `-` on the first page holds (the key is consumed, nothing moves).
        let held_up = {
            let mut t = Session::new(Arc::clone(&dict));
            t.process_key(b'd' as u32);
            t.process_key(KEY_PAGE_UP)
        };
        assert!(held_up.handled);
        assert_eq!(held_up.candidates, first.candidates);
    }

    /// Paging resets to the first page when the encoding changes, so a
    /// deeper page never survives an edit.
    #[test]
    fn paging_resets_when_the_encoding_changes() {
        let dict = Arc::new(Dictionary::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/schemes/liuli_v1/liuli_table.txt"
        ))
        .expect("default scheme table must load"));

        // The page "d" shows on entry, for the final comparison.
        let d_first_page = Session::new(Arc::clone(&dict)).process_key(b'd' as u32).candidates;

        let mut s = Session::new(dict);
        s.process_key(b'd' as u32);
        let dd_first = s.process_key(b'd' as u32); // "dd", first page
        let dd_second = s.process_key(KEY_PAGE_DOWN);
        assert_eq!(dd_second.preedit, "dd");
        assert_ne!(dd_second.candidates, dd_first.candidates, "turning changes the page");

        // Backspace re-retrieves and must land on the first page of "d", not
        // the page the deeper encoding was left on.
        let r = s.process_key(KEY_BACKSPACE); // back to "d"
        assert_eq!(r.preedit, "d");
        assert_eq!(r.candidates, d_first_page, "editing resets to page 1");
    }

    /// Without a candidate to page through, `-`/`=` are consumed silently: no
    /// commit, nothing reaching the host, composition left intact.
    #[test]
    fn paging_keys_do_nothing_without_candidates() {
        let mut s = session();
        s.process_key(KEY_Q); // "q" has no candidate in the sample
        for key in [KEY_PAGE_UP, KEY_PAGE_DOWN] {
            let r = s.process_key(key);
            assert!(r.handled, "no candidate: the key stays with the engine");
            assert_eq!(r.commit_text, None, "key {key} commits nothing");
            assert_eq!(r.preedit, "q", "the composition stands");
        }
    }

    /// Switching lexicons (a scheme change) replaces the candidates and
    /// drops the in-progress composition, never showing stale entries.
    #[test]
    fn set_dict_swaps_the_lexicon_and_clears_the_composition() {
        let mut s = Session::new(Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses")));
        s.process_key(KEY_A);
        s.set_dict(Arc::new(Dictionary::parse_str("a\t乙\t100\n").expect("sample parses")));
        // The composition was discarded, so idle keys pass through again.
        assert!(!s.process_key(KEY_SPACE).handled);
        let r = s.process_key(KEY_A);
        assert_eq!(r.candidates[0].text, "乙", "the new lexicon answers");
    }

    /// Clamping protects typing from a broken stored value: the session
    /// still runs with the nearest usable limits.
    #[test]
    fn out_of_range_config_is_clamped() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let s = Session::with_config(dict, Config { page_size: 0, max_code_len: 100, ..Config::default() });
        assert_eq!(s.config().page_size, 1);
        assert_eq!(s.config().max_code_len, crate::MAX_CODE_LEN_LIMIT);
    }

    /// `auto_commit`: once the code is long enough and exactly one candidate
    /// remains, the engine commits without waiting for a selection key.
    #[test]
    fn auto_commits_a_unique_candidate_once_the_code_is_long_enough() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(
            dict,
            Config { auto_commit: true, auto_commit_length: 2, ..Config::default() },
        );
        let r = s.process_key(KEY_A); // four candidates: nothing to commit
        assert!(r.commit_text.is_none());
        assert_eq!(r.preedit, "a");

        let r = s.process_key(b'd' as u32); // "ad" narrows to exactly 匿
        assert_eq!(r.commit_text.as_deref(), Some("匿"));
        assert_eq!(r.preedit, "");
        assert!(r.candidates.is_empty());
        // The session is idle again; keys belong to the application.
        assert!(!s.process_key(KEY_SPACE).handled);
    }

    /// Auto-commit needs both conditions: a long-enough code and a unique
    /// candidate. Either one missing keeps normal composition.
    #[test]
    fn auto_commit_waits_while_short_or_ambiguous() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));

        // Unique ("ad" = 匿) but shorter than the requested length.
        let mut s = Session::with_config(
            Arc::clone(&dict),
            Config { auto_commit: true, auto_commit_length: 4, ..Config::default() },
        );
        s.process_key(KEY_A);
        let r = s.process_key(b'd' as u32);
        assert!(r.commit_text.is_none(), "unique but below auto_commit_length");
        assert_eq!(r.preedit, "ad");
        assert_eq!(r.candidates[0].text, "匿");
        s.process_key(KEY_ESCAPE);

        // Long enough but ambiguous: 'a' already reaches length 1.
        let mut s = Session::with_config(
            dict,
            Config { auto_commit: true, auto_commit_length: 1, ..Config::default() },
        );
        let r = s.process_key(KEY_A);
        assert!(r.commit_text.is_none(), "several candidates keep the composition open");
        assert_eq!(r.preedit, "a");
    }

    /// An overflow commit and the auto-commit it restarts share one response:
    /// the phrase for the full code, then the restart letter's own word.
    #[test]
    fn overflow_then_auto_commit_join_into_one_commit() {
        let dict = Arc::new(
            Dictionary::parse_str("g\t王\t100\ngg\t五\t100\nggg\t武\t100\na\t工\t100\n")
                .expect("sample parses"),
        );
        let mut s = Session::with_config(
            dict,
            Config {
                commit_next_code: 3,
                auto_commit: true,
                auto_commit_length: 1,
                ..Config::default()
            },
        );
        s.process_key(KEY_G); // 王, 五(~g)
        let r = s.process_key(KEY_G); // "gg" = 五, 武(~g): still ambiguous
        assert!(r.commit_text.is_none());

        let r = s.process_key(KEY_A);
        assert_eq!(r.commit_text.as_deref(), Some("五工"), "overflow for gg plus auto-commit for a");
        assert_eq!(r.preedit, "");
        assert!(!s.process_key(KEY_SPACE).handled);
    }

    /// 中文标点 mode: mapped keys commit their full-width text straight
    /// away, with no composition ever opening.
    #[test]
    fn chinese_mode_commits_full_width_punctuation_when_idle() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(dict, Config { ascii_punct: 1, ..Config::default() });
        let r = s.process_key(b',' as u32);
        assert!(r.handled, "the engine claims the key");
        assert_eq!(r.commit_text.as_deref(), Some("\u{FF0C}"));
        assert!(r.preedit.is_empty());
        assert!(r.candidates.is_empty());
        // A letter still opens a composition after a punctuation commit.
        assert!(s.process_key(KEY_A).handled);
        assert_eq!(s.process_key(KEY_A).preedit, "aa");
    }

    /// Mid-composition punctuation goes on screen with the first candidate;
    /// without any candidate it commits the raw letters (the Space fallback).
    #[test]
    fn chinese_mode_punctuation_commits_the_candidate_with_it() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(dict, Config { ascii_punct: 1, ..Config::default() });
        s.process_key(KEY_A); // 工/戈/式/匿
        let r = s.process_key(b',' as u32);
        assert_eq!(r.commit_text.as_deref(), Some("\u{5DE5}\u{FF0C}"));
        assert!(r.preedit.is_empty());
        assert!(!s.process_key(KEY_SPACE).handled, "the composition ended");

        s.process_key(KEY_Q); // "q" has no candidate in the sample
        let r = s.process_key(b'.' as u32);
        assert_eq!(r.commit_text.as_deref(), Some("q\u{3002}"));
    }

    /// Quote pairing is session state that crosses commits: the second quote
    /// typed after an unrelated commit is still the closing glyph.
    #[test]
    fn chinese_mode_quote_pairing_crosses_commits() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(dict, Config { ascii_punct: 1, ..Config::default() });
        assert_eq!(s.process_key(b'"' as u32).commit_text.as_deref(), Some("\u{201C}"));
        s.process_key(KEY_A);
        let r = s.process_key(KEY_1); // commits 工
        assert_eq!(r.commit_text.as_deref(), Some("\u{5DE5}"));
        assert_eq!(s.process_key(b'"' as u32).commit_text.as_deref(), Some("\u{201D}"));
        // Unmapped symbols still pass through in Chinese mode.
        assert!(!s.process_key(b'%' as u32).handled);
    }

    /// The `12:30` case: with `english_punct_after_alnum` on, the first
    /// punctuation right after digits is released to the host as ASCII, and
    /// the whole time string types straight through. The punctuation after
    /// it is 中文标点 again, and a Chinese commit ends any English-punct
    /// window.
    #[test]
    fn digits_let_the_next_punctuation_pass_as_english() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(
            dict,
            Config { ascii_punct: 1, english_punct_after_alnum: true, ..Config::default() },
        );
        for key in [b'1' as u32, b'2' as u32] {
            assert!(!s.process_key(key).handled, "digit {key} passes through");
        }
        assert!(
            !s.process_key(b':' as u32).handled,
            "the first punctuation after digits stays English"
        );
        assert_eq!(
            s.process_key(b':' as u32).commit_text.as_deref(),
            Some("\u{FF1A}"),
            "the second one is 中文标点 again"
        );
        for key in [b'1' as u32, b'2' as u32, b':' as u32, b'3' as u32, b'0' as u32] {
            assert!(!s.process_key(key).handled, "12:30 types straight through (key {key})");
        }
        // A Chinese commit closes the English-punct window.
        s.process_key(KEY_A);
        assert_eq!(s.process_key(KEY_SPACE).commit_text.as_deref(), Some("\u{5DE5}"));
        assert_eq!(s.process_key(b',' as u32).commit_text.as_deref(), Some("\u{FF0C}"));
    }

    /// With the switch off (the default) digits still pass but the colon
    /// right after them commits 中文标点: behavior is unchanged.
    #[test]
    fn switch_off_keeps_digits_followed_by_chinese_punctuation() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(dict, Config { ascii_punct: 1, ..Config::default() });
        for key in [b'1' as u32, b'2' as u32] {
            assert!(!s.process_key(key).handled);
        }
        assert_eq!(s.process_key(b':' as u32).commit_text.as_deref(), Some("\u{FF1A}"));
    }

    /// What arms and disarms the rule at the session level: committing raw
    /// letters arms it, a capital released to the host arms it, and a space
    /// disarms it. An abandoned composition never touches the caret, so the
    /// digits typed before it still arm the punctuation that follows.
    #[test]
    fn letter_commits_and_releases_drive_the_rule() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let config =
            Config { ascii_punct: 1, english_punct_after_alnum: true, ..Config::default() };

        let mut s = Session::with_config(Arc::clone(&dict), config);
        s.process_key(b'q' as u32); // "q" has no candidate
        assert_eq!(s.process_key(KEY_ENTER).commit_text.as_deref(), Some("q"));
        assert!(!s.process_key(b',' as u32).handled, "after a raw-letter commit , stays English");

        let mut s = Session::with_config(Arc::clone(&dict), config);
        assert!(!s.process_key(b'H' as u32).handled, "the capital reaches the host");
        assert!(!s.process_key(b',' as u32).handled, "and arms the rule");
        s.process_key(b'K' as u32);
        assert!(!s.process_key(b' ' as u32).handled, "a space reaches the host");
        assert_eq!(
            s.process_key(b',' as u32).commit_text.as_deref(),
            Some("\u{FF0C}"),
            "the space disarmed the rule"
        );

        let mut s = Session::with_config(dict, config);
        assert!(!s.process_key(b'1' as u32).handled);
        s.process_key(KEY_G); // a composition opens…
        let r = s.process_key(KEY_ESCAPE); // …and is abandoned without committing
        assert!(r.handled);
        assert!(!s.process_key(b',' as u32).handled, "the digit still arms the rule");
    }

    /// 分号/引号二三选: `;` and `'` commit the 2nd and 3rd candidate of the
    /// rendered page.
    #[test]
    fn semicolon_and_quote_commit_alternates() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let config = Config { semicolon_quote_select: true, ..Config::default() };
        let mut s = Session::with_config(Arc::clone(&dict), config);
        s.process_key(KEY_A); // 工 戈 式 匿
        assert_eq!(s.process_key(b';' as u32).commit_text.as_deref(), Some("戈"));
        let mut s = Session::with_config(dict, config);
        s.process_key(KEY_A);
        assert_eq!(s.process_key(b'\'' as u32).commit_text.as_deref(), Some("式"));
    }

    /// A page shorter than the pick clamps to its last candidate: a unique
    /// candidate commits on `;` and `'` with no punctuation attached (even
    /// in 中文标点 mode), and a two-candidate page commits its 2nd on
    /// either key.
    #[test]
    fn selection_keys_clamp_to_short_pages() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(
            Arc::clone(&dict),
            Config { semicolon_quote_select: true, ascii_punct: 1, ..Config::default() },
        );
        s.process_key(KEY_A);
        s.process_key(b'd' as u32); // "ad" = 匿, unique
        assert_eq!(
            s.process_key(b';' as u32).commit_text.as_deref(),
            Some("匿"),
            "the unique candidate commits without ；"
        );
        let mut s = Session::with_config(
            Arc::clone(&dict),
            Config { semicolon_quote_select: true, ascii_punct: 1, ..Config::default() },
        );
        s.process_key(KEY_A);
        s.process_key(b'd' as u32);
        assert_eq!(s.process_key(b'\'' as u32).commit_text.as_deref(), Some("匿"));

        let mut s = Session::with_config(
            dict,
            Config { semicolon_quote_select: true, page_size: 2, ..Config::default() },
        );
        s.process_key(KEY_A); // page 1: 工 戈
        assert_eq!(
            s.process_key(b'\'' as u32).commit_text.as_deref(),
            Some("戈"),
            "the quote clamps to the 2nd candidate of the 2-key page"
        );
    }

    /// The selection keys need a candidate: with none, `;` and `'` commit
    /// the raw encoding plus their punctuation as before, and with the
    /// switch off they never select.
    #[test]
    fn selection_keys_need_a_candidate() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(
            Arc::clone(&dict),
            Config { semicolon_quote_select: true, ascii_punct: 1, ..Config::default() },
        );
        s.process_key(b'q' as u32); // "q" has no candidate
        assert_eq!(
            s.process_key(b';' as u32).commit_text.as_deref(),
            Some("q\u{FF1B}"),
            "without candidates ; is punctuation"
        );
        let mut t = Session::new(dict);
        t.process_key(KEY_A);
        assert!(
            !t.process_key(b';' as u32).handled,
            "off (English mode): ; still reaches the host mid-composition"
        );
    }

    /// `;` and `'` address the page the shell renders, exactly like the
    /// digits: after paging they commit within the new page.
    #[test]
    fn selection_keys_follow_the_page() {
        let dict = Arc::new(Dictionary::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/schemes/liuli_v1/liuli_table.txt"
        ))
        .expect("default scheme table must load"));
        let mut s = Session::with_config(
            dict,
            Config { semicolon_quote_select: true, ..Config::default() },
        );
        s.process_key(b'd' as u32);
        let second = s.process_key(KEY_PAGE_DOWN);
        assert_eq!(second.candidates.len(), MAX_CANDIDATES, "the second page is full");
        assert_eq!(
            s.process_key(b';' as u32).commit_text.as_deref(),
            Some(second.candidates[1].text.as_str()),
            "; picks the 2nd candidate of the current page"
        );
    }
}
