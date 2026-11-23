//! Session state machine (SPEC_MVP §3).
//!
//! `Session` is the only owner of typing state. It keeps the encoding and the
//! rendered candidate list, asks [`Processor`] what a key means and applies
//! the answer, and asks [`Pipeline`] for candidates whenever the encoding
//! changes. Nothing here knows where candidates come from, so adding a pinyin
//! source or a converter touches the pipeline and not this file.

use std::mem;
use std::sync::Arc;

use crate::config::Config;
use crate::dictionary::Dictionary;
use crate::pipeline::Pipeline;
use crate::processor::{Action, Processor};
use crate::punct::{self, QuotePair};
use crate::types::{Candidate, EngineResponse};

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
    /// Quote pairing for 中文标点; deliberately survives [`Session::clear`]
    /// so a quote opened before a commit still closes after it.
    quotes: QuotePair,
}

impl Session {
    /// Session over a lexicon with the default configuration, using the MVP
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
            quotes: QuotePair::default(),
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
            quotes: QuotePair::default(),
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
        let decision = Processor::decide(key, &self.input_buffer, self.candidates.len(), self.config);
        match decision {
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
                // Same commit choice as Space: the first candidate, or the
                // letters themselves when nothing matches.
                let mut commit = self
                    .candidates
                    .first()
                    .map(|candidate| candidate.text.clone())
                    .or_else(|| Some(mem::take(&mut self.input_buffer)))
                    .unwrap_or_default();
                commit.push_str(punctuation);
                self.clear();
                self.response(true, Some(commit))
            }
            Action::Select(index) => match self.candidates.get(index) {
                Some(candidate) => {
                    let commit = candidate.text.clone();
                    self.clear();
                    self.response(true, Some(commit))
                }
                // The rendered page no longer has that entry; keep composing
                // rather than commit the wrong text.
                None => self.response(true, None),
            },
            Action::PageUp => {
                self.turn_page(PageDirection::Previous);
                self.response(true, None)
            }
            Action::PageDown => {
                self.turn_page(PageDirection::Next);
                self.response(true, None)
            }
        }
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

    /// Re-retrieve the whole candidate window for the current encoding and
    /// show its first page: an encoding change always restarts at the top.
    fn refresh_candidates(&mut self) {
        self.window = if self.input_buffer.is_empty() {
            Vec::new()
        } else {
            self.pipeline.retrieve(&self.input_buffer)
        };
        self.page = 0;
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
    }

    /// Move the page cursor, clamped to the first and last page: turning past
    /// an end holds on it, so the paging keys never leave the list.
    fn turn_page(&mut self, direction: PageDirection) {
        match direction {
            PageDirection::Previous => self.page = self.page.saturating_sub(1),
            PageDirection::Next => self.page = self.page.saturating_add(1),
        }
        self.apply_page();
    }

    fn clear(&mut self) {
        self.input_buffer.clear();
        self.window.clear();
        self.page = 0;
        self.candidates.clear();
    }

    /// Bundle current session state into the response a shell renders as-is.
    fn response(&self, handled: bool, commit_text: Option<String>) -> EngineResponse {
        EngineResponse {
            handled,
            commit_text,
            preedit: self.input_buffer.clone(),
            candidates: self.candidates.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_SPACE, MAX_CODE_LEN,
        MAX_CANDIDATES,
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

    /// The flow required by SPEC_MVP: `a` -> `Backspace` -> `a` -> `Space`
    /// must commit "工" and leave the session idle.
    #[test]
    fn retype_after_backspace_then_space_commits_first_candidate() {
        let mut s = session();

        let r = s.process_key(KEY_A);
        assert!(r.handled);
        assert!(r.commit_text.is_none());
        assert_eq!(r.preedit, "a");
        assert_eq!(r.candidates[0], Candidate { text: "工".into(), comment: "".into() });

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
                Candidate { text: "工".into(), comment: "".into() },
                Candidate { text: "戈".into(), comment: "".into() },
                Candidate { text: "式".into(), comment: "~a".into() },
                Candidate { text: "匿".into(), comment: "~d".into() },
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
        assert_eq!(r.candidates, vec![Candidate { text: "匿".into(), comment: "".into() }]);
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
        assert_eq!(r.candidates, vec![Candidate { text: "匿".into(), comment: "".into() }]);
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
        for field in ["handled", "commit_text", "preedit", "candidates"] {
            assert!(v.get(field).is_some(), "missing field {field} in {json}");
        }
        assert_eq!(v["handled"], serde_json::json!(true));
        assert_eq!(v["commit_text"], serde_json::json!("工"));
        assert_eq!(v["preedit"], serde_json::json!(""));
        assert_eq!(v["candidates"], serde_json::json!([]));

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
                    vec![Candidate { text: "星".into(), comment: String::new() }]
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
}
