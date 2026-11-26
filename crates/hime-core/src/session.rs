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
use crate::pinyin::PinyinLexicon;
use crate::dictionary::Dictionary;
use crate::diff::annotate_candidate_diffs;
use crate::pipeline::Pipeline;
use crate::processor::{Action, Processor, ReverseState};
use crate::punct::{self, QuotePair};
use crate::reverse::ReverseLookup;
use crate::sentence::auto_commit::SentenceAutoCommit;
use crate::sentence::SentenceEngine;
use crate::types::{Candidate, EngineResponse};
use crate::user_ngram::UserNgram;

/// Marker that opens 反查 (SPEC §8.3). The session keeps it out of the
/// encoding itself: `input_buffer` only ever holds the pinyin query, so the
/// translator, paging and digits work on it unchanged.
const REVERSE_PREFIX: char = '`';

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
    /// The punctuation key [`Config::english_punct_after_alnum`] released to
    /// the host last, while that ASCII character still stands at the caret
    /// (SPEC §3.1). A second press of the same key corrects it to 中文标点;
    /// anything else the engine learns about the caret clears it.
    released_punct: Option<u8>,
    /// 形码整句 probabilistic/empty-code auto-commit state; `None` for every
    /// non-sentence session, which keeps the sentence path fully gated.
    sentence: Option<SentenceComposer>,
    /// 反查 source (SPEC §8.3): present exactly when the scheme has an
    /// encoding worth looking up, i.e. 形码 or 形码整句. A 全拼 session never
    /// has one, so `` ` `` keeps its punctuation meaning there.
    reverse: Option<Arc<ReverseLookup>>,
    /// Whether 反查 is open. Held here rather than as a `` ` `` inside
    /// `input_buffer`, so the query alone reaches the pipeline and the
    /// encoding grows exactly like a 全拼 one.
    reversing: bool,
    /// The learned per-user n-gram model, when the scheme declares
    /// `user_ngram = true` (SPEC §7.7). Committed text is recorded here and
    /// the sources behind the pipeline blend it into their score.
    user_ngram: Option<Arc<UserNgram>>,
    /// The tail (at most two characters) of what this session committed, used
    /// as model history for ranking and as the learning chain's seed. A new
    /// session starts empty (BOS), so one session's end never chains into the
    /// next session's beginning.
    commit_context: String,
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
            released_punct: None,
            sentence: None,
            reverse: None,
            reversing: false,
            user_ngram: None,
            commit_context: String::new(),
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
            released_punct: None,
            sentence: None,
            reverse: None,
            reversing: false,
            user_ngram: None,
            commit_context: String::new(),
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
        let user_ngram = engine.user_ngram().cloned();
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
            released_punct: None,
            sentence: Some(SentenceComposer {
                engine,
                auto_commit: SentenceAutoCommit::default(),
            }),
            reverse: None,
            reversing: false,
            user_ngram,
            commit_context: String::new(),
        }
    }

    /// Session for 全拼: encodings are full pinyin and candidates come from
    /// `lexicon`. The key model is the table one, with the longer pinyin code
    /// cap and no overflow commit (`Processor` reads `code_type`); paging,
    /// digits, Space and punctuation behave exactly as they do for 形码.
    ///
    /// This constructor serves table words (including 简拼). Use
    /// [`Self::pinyin_sentence`] to add whole-sentence decoding.
    pub fn pinyin(lexicon: Arc<PinyinLexicon>, config: Config) -> Self {
        let pipeline = Pipeline::pinyin(Arc::clone(&lexicon));
        Self::pinyin_with(pipeline, config)
    }

    /// Session for 全拼 with whole-sentence decoding: a word the table does
    /// not contain still becomes a candidate when `model` scores the
    /// character chain well (`huangzhao` → 皇朝 / 黄照). The sentence
    /// candidates head the window; table words, 简拼 and prefix words follow.
    /// `user` is the learned n-gram model the scheme's `user_ngram` key
    /// enables; both it and the default model re-rank single characters.
    pub fn pinyin_sentence(
        lexicon: Arc<PinyinLexicon>,
        model: Arc<crate::ngram::NgramModel>,
        user: Option<Arc<UserNgram>>,
        config: Config,
    ) -> Self {
        let pipeline =
            Pipeline::pinyin_sentence(Arc::clone(&lexicon), model, user.clone());
        let mut session = Self::pinyin_with(pipeline, config);
        session.user_ngram = user;
        session
    }

    fn pinyin_with(pipeline: Pipeline, config: Config) -> Self {
        let config = Config { code_type: CodeType::Quanpin, ..config }.sanitized();
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
            released_punct: None,
            sentence: None,
            reverse: None,
            reversing: false,
            user_ngram: None,
            commit_context: String::new(),
        }
    }

    /// Enable 反查 for this session (SPEC §8.3): `` ` `` then full pinyin
    /// lists pinyin words with the active scheme's codes in their comment.
    /// Only a scheme that has codes to look up should call this — a 全拼
    /// session must not, or `` ` `` would stop being the punctuation key the
    /// mapping table defines.
    #[must_use]
    pub fn with_reverse(mut self, lookup: Arc<ReverseLookup>) -> Self {
        self.reverse = Some(lookup);
        self
    }

    /// Attach the learned user model (SPEC §7.7). The pipeline sources must
    /// already share it — [`Session::sentence`] and
    /// [`Session::pinyin_sentence`] take it directly; this setter serves a
    /// session whose sources read the model through their own handle.
    #[must_use]
    pub fn with_user_ngram(mut self, model: Option<Arc<UserNgram>>) -> Self {
        self.user_ngram = model;
        self
    }

    /// The user model this session records into, if any.
    #[must_use]
    pub fn user_ngram(&self) -> Option<&Arc<UserNgram>> {
        self.user_ngram.as_ref()
    }

    /// The text this session committed last, at most two characters — the
    /// model history for ranking and the seed of the next learned chain.
    #[must_use]
    pub fn commit_context(&self) -> &str {
        &self.commit_context
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
        // The correction window lasts exactly one key: if this key is not the
        // released punctuation pressed again, the release no longer stands at
        // the caret (or something else happened to it), so drop it.
        let released_punct = self.released_punct.take();
        let decision = Processor::decide(
            key,
            &self.input_buffer,
            self.candidates.len(),
            self.config,
            self.after_alnum,
            released_punct,
            self.reverse_state(),
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
                if !self.reversing {
                    if let Some(composer) = self.sentence.as_mut() {
                        composer.auto_commit.evidence_invalidated();
                    }
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
            Action::StartReverse => {
                self.reversing = true;
                self.refresh_candidates();
                self.response(true, None)
            }
            Action::CommitBacktick => {
                self.clear();
                self.response(true, Some(REVERSE_PREFIX.to_string()))
            }
            Action::CommitPunct(byte) => match punct::map(byte, &mut self.quotes) {
                Some(text) => self.response(true, Some(text.to_owned())),
                // Processor gates on the same table, so this only guards
                // the two from ever disagreeing.
                None => self.response(false, None),
            },
            Action::ReleasePunct(byte) => {
                // The host inserts the ASCII character itself; the response
                // only records it so a second press can correct it.
                self.released_punct = Some(byte);
                self.response(false, None)
            }
            Action::CorrectPunct(byte) => match punct::map(byte, &mut self.quotes) {
                Some(text) => {
                    // The ASCII character already stands in the host text:
                    // the shell swaps the 中文标点 for it in place.
                    let response = self.response(true, Some(text.to_owned()));
                    EngineResponse { replace_text: Some(char::from(byte).to_string()), ..response }
                }
                None => self.response(true, None),
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
        // Neither runs while 反查 is open: the query is pinyin, not a code the
        // scheme could decode, so nothing may go to the host on its own.
        if self.sentence.is_some() && !self.reversing {
            return self.sentence_auto_commit(pending);
        }
        // 反查 never auto-commits: the user is looking up codes, and the
        // candidates are the pinyin words, not what they meant to type. 全拼
        // does not either: a pinyin encoding is usually a prefix of a longer
        // word, so a unique match is not a decision the engine may make for
        // the user (the reference implementation behaves the same way).
        if !self.reversing
            && self.config.code_type != CodeType::Quanpin
            && self.config.auto_commit
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
            let context = self.commit_context.clone();
            composer.auto_commit.key_pressed(
                composer.engine.as_ref(),
                &self.input_buffer,
                true,
                self.config.auto_commit,
                &context,
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

    /// What `` ` `` means right now (SPEC §8.3). Only a session with a reverse
    /// lookup has one at all, so a 全拼 session reports
    /// [`ReverseState::Unsupported`] and `` ` `` stays punctuation.
    fn reverse_state(&self) -> ReverseState {
        if self.reverse.is_none() {
            ReverseState::Unsupported
        } else if self.reversing {
            ReverseState::Open
        } else {
            ReverseState::Closed
        }
    }

    /// The chain 反查 retrieves from: pinyin words carrying the scheme codes.
    ///
    /// Built per keystroke from the shared lookup rather than swapped into
    /// `pipeline`, so leaving 反查 restores the scheme chain by simply
    /// clearing [`Self::reversing`] — there is nothing to put back.
    fn reverse_pipeline(&self) -> Pipeline {
        match &self.reverse {
            Some(lookup) => Pipeline::reverse(Arc::clone(lookup)),
            None => Pipeline::new(vec![], vec![]),
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
        } else if self.reversing {
            self.reverse_pipeline().retrieve(&self.input_buffer, "")
        } else {
            let context = self.commit_context.clone();
            self.pipeline.retrieve(&self.input_buffer, &context)
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
        // 反查 lists pinyin words, not competing decodings of one code: diff
        // colors there would only decorate unrelated words.
        if self.sentence.is_some() && !self.reversing {
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
        self.reversing = false;
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

    /// Teach the learned model what the user just committed (SPEC §7.7) and
    /// move the session's context to the committed text's tail. The chain is
    /// seeded with the session's own earlier commits (BOS at the start of the
    /// session), so a new session never links to the previous one's end.
    fn learn(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.config.user_ngram {
            // The scheme switched the model off, or never turned it on.
            return;
        }
        if let Some(model) = self.user_ngram.as_deref() {
            model.record(text, &self.commit_context);
            model.save_if_due();
        }
        // The context is the two characters left of the caret: the previous
        // tail plus this commit's own characters.
        self.commit_context.push_str(text);
        let chars: Vec<char> = self.commit_context.chars().collect();
        if chars.len() > 2 {
            self.commit_context = chars[chars.len() - 2..].iter().collect();
        }
    }


    /// The encoding to display: the first candidate's word-separated code
    /// split when it covers the raw encoding, otherwise the raw letters. The
    /// split covers the whole code for a whole-sentence or pinyin candidate,
    /// so the shown string is an automatic segmentation of exactly what was
    /// typed. 反查 keeps its marker in front, so the line reads `` `ni hao ``
    /// and it is obvious the window is a lookup.
    fn code_line(&self) -> String {
        let line = self.source_code_line();
        if self.reversing {
            format!("{REVERSE_PREFIX}{line}")
        } else {
            line
        }
    }

    fn source_code_line(&self) -> String {
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
    /// Sending text is also the one moment the session learns: the learned
    /// model takes the committed text, and its tail becomes the session's
    /// context.
    fn response(&mut self, handled: bool, commit_text: Option<String>) -> EngineResponse {
        if let Some(text) = commit_text.as_deref() {
            self.learn(text);
        }
        EngineResponse {
            handled,
            commit_text,
            replace_text: None,
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
pub(crate) mod tests {
    use super::*;
    use crate::{
        KEY_ARROW_DOWN, KEY_ARROW_LEFT, KEY_ARROW_RIGHT, KEY_ARROW_UP, KEY_BACKSPACE, KEY_ENTER,
        KEY_ESCAPE, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_SPACE, MAX_CODE_LEN, MAX_CANDIDATES,
    };
    use crate::user_ngram::{UserNgramQuery, UserNgramScope};

    /// The shipped whole-sentence model, when this machine has it; the pinyin
    /// sentence tests skip without it, exactly like the 形码整句 ones.
    pub(crate) fn shipped_model() -> Option<Arc<crate::ngram::NgramModel>> {
        let path = crate::ngram::find_model_path()?;
        match crate::ngram::NgramModel::load(path) {
            Ok(model) => Some(Arc::new(model)),
            Err(e) => {
                eprintln!("session test: skipped ({e})");
                None
            }
        }
    }

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
            "replace_text",
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
        assert_eq!(v["replace_text"], serde_json::Value::Null);
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

    /// Pressing the released key a second time replaces the ASCII character
    /// the host typed with 中文标点: `12` `:` `:` ends as `12：`. Only that
    /// key does it, and only while the release is still the last thing that
    /// reached the caret.
    #[test]
    fn second_press_corrects_the_released_punctuation() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let config =
            Config { ascii_punct: 1, english_punct_after_alnum: true, ..Config::default() };

        let mut s = Session::with_config(Arc::clone(&dict), config);
        s.process_key(b'1' as u32);
        s.process_key(b'2' as u32);
        let r = s.process_key(b':' as u32);
        assert!(!r.handled, "the first colon reaches the host");
        let r = s.process_key(b':' as u32);
        assert!(r.handled, "the engine claims the correction");
        assert_eq!(r.commit_text.as_deref(), Some("\u{FF1A}"));
        assert_eq!(
            r.replace_text.as_deref(),
            Some(":"),
            "it replaces the ASCII colon already on screen"
        );
        assert!(r.preedit.is_empty());
        // The correction leaves 中文标点 behind, so the next key is ordinary.
        let r = s.process_key(b':' as u32);
        assert_eq!(r.commit_text.as_deref(), Some("\u{FF1A}"));
        assert!(r.replace_text.is_none(), "a third press appends, it does not correct");

        // Another punctuation key is not a correction: `:` then `;` stays
        // the ordinary 中文标点 commit of `;`.
        let mut s = Session::with_config(Arc::clone(&dict), config);
        s.process_key(b'1' as u32);
        assert!(!s.process_key(b':' as u32).handled);
        let r = s.process_key(b';' as u32);
        assert!(r.handled);
        assert_eq!(r.commit_text.as_deref(), Some("\u{FF1B}"));
        assert!(r.replace_text.is_none());

        // A later character at the caret drops the window: after a capital
        // reaches the host, the next colon is a fresh first release, and the
        // correction after it replaces that colon.
        let mut s = Session::with_config(Arc::clone(&dict), config);
        s.process_key(b'1' as u32);
        assert!(!s.process_key(b':' as u32).handled);
        assert!(!s.process_key(b'A' as u32).handled, "a capital reaches the host");
        let r = s.process_key(b':' as u32);
        assert!(!r.handled, "the colon after the capital is released again");
        let r = s.process_key(b':' as u32);
        assert_eq!(r.commit_text.as_deref(), Some("\u{FF1A}"));
        assert_eq!(r.replace_text.as_deref(), Some(":"));

        // With the switch off nothing is released, so a repeated mapped key
        // stays two ordinary 中文标点 commits.
        let mut s = Session::with_config(
            Arc::clone(&dict),
            Config { ascii_punct: 1, ..Config::default() },
        );
        assert_eq!(s.process_key(b',' as u32).replace_text, None);
        assert_eq!(s.process_key(b',' as u32).commit_text.as_deref(), Some("\u{FF0C}"));
        assert_eq!(s.process_key(b',' as u32).replace_text, None);
    }

    /// A released quote corrects to the glyph its own press would have
    /// produced, and the pairing advances exactly once.
    #[test]
    fn second_press_corrects_a_released_quote() {
        let dict = Arc::new(Dictionary::parse_str(SAMPLE).expect("sample parses"));
        let mut s = Session::with_config(
            dict,
            Config { ascii_punct: 1, english_punct_after_alnum: true, ..Config::default() },
        );
        s.process_key(b'H' as u32); // a capital released to the host arms the rule
        let r = s.process_key(b'\'' as u32);
        assert!(!r.handled);
        let r = s.process_key(b'\'' as u32);
        assert_eq!(r.commit_text.as_deref(), Some("\u{2018}"), "an opening quote");
        assert_eq!(r.replace_text.as_deref(), Some("'"), "the ASCII quote is what it replaces");
        // The pairing state advanced once: the next quote outside the window
        // is the closing glyph.
        assert_eq!(s.process_key(b'\'' as u32).commit_text.as_deref(), Some("\u{2019}"));
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

    // --- 全拼 (SPEC §8.2) ---

    /// A commit teaches the user model: the text arrives with the session's
    /// own earlier commits as context, and the model reports it once saved.
    #[test]
    fn commits_are_recorded_into_the_user_model() {
        let path = std::env::temp_dir().join(format!(
            "hime-session-learn-{}.hng",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let model = Arc::new(UserNgram::empty(&path));
        let lexicon = Arc::new(PinyinLexicon::parse("你好\tnihao\t200\n你\tni\t100\n"));
        let config = Config { user_ngram: true, ..Config::default() };
        let mut s = Session::pinyin_sentence(lexicon, shipped_model().expect("model present"), Some(Arc::clone(&model)), config);
        for byte in "nihao".bytes() {
            s.process_key(u32::from(byte));
        }
        let r = s.process_key(KEY_SPACE);
        assert_eq!(r.commit_text.as_deref(), Some("你好"));
        assert_eq!(s.commit_context(), "你好", "the session keeps the committed tail");

        assert!(model.has_data(), "the commit reached the model");
        let info = model.query(&UserNgramQuery {
            scope: UserNgramScope::Bigram,
            query: "你好".into(),
            limit: 10,
            offset: 0,
        });
        assert_eq!(info.total, 1, "{:?}", info.entries);
        assert_eq!(info.entries[0].prev, "你");
        assert_eq!(info.entries[0].count, 1);

        // A second commit chains onto the first inside one session.
        let _ = s.process_key(KEY_ESCAPE);
        for _ in 0..2 {
            for byte in "ni".bytes() {
                s.process_key(u32::from(byte));
            }
            s.process_key(KEY_SPACE);
        }
        assert_eq!(s.commit_context(), "你你");
        let chained = model.query(&UserNgramQuery {
            scope: UserNgramScope::Trigram,
            query: "你好你".into(),
            limit: 10,
            offset: 0,
        });
        assert_eq!(chained.total, 1, "好 你 你: {:?}", chained.entries);
        let _ = std::fs::remove_file(&path);
    }

    /// A new session starts at BOS: the previous session's last characters are
    /// never the context of the next session's first ones.
    #[test]
    fn a_new_session_does_not_chain_onto_the_previous_one() {
        let path = std::env::temp_dir().join(format!(
            "hime-session-fresh-{}.hng",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let model = Arc::new(UserNgram::empty(&path));
        let lexicon = Arc::new(PinyinLexicon::parse("你好\tnihao\t200\n你\tni\t100\n"));
        let config = Config { user_ngram: true, ..Config::default() };

        let mut first = Session::pinyin_sentence(Arc::clone(&lexicon), shipped_model().expect("model present"), Some(Arc::clone(&model)), config);
        for byte in "nihao".bytes() {
            first.process_key(u32::from(byte));
        }
        first.process_key(KEY_SPACE);
        assert_eq!(first.commit_context(), "你好");

        let mut second = Session::pinyin_sentence(lexicon, shipped_model().expect("model present"), Some(Arc::clone(&model)), config);
        assert_eq!(second.commit_context(), "", "a fresh session has no history");
        for byte in "nihao".bytes() {
            second.process_key(u32::from(byte));
        }
        second.process_key(KEY_SPACE);

        let chained = model.query(&UserNgramQuery {
            scope: UserNgramScope::Trigram,
            query: "好你好".into(),
            limit: 10,
            offset: 0,
        });
        assert_eq!(chained.total, 0, "the sessions stay apart: {:?}", chained.entries);
        let fresh = model.query(&UserNgramQuery {
            scope: UserNgramScope::Bigram,
            query: "你好".into(),
            limit: 10,
            offset: 0,
        });
        assert_eq!(fresh.total, 1);
        assert_eq!(fresh.entries[0].total, 2, "two 你 → 好, both from BOS");
        let _ = std::fs::remove_file(&path);
    }

    fn pinyin_lexicon() -> Arc<crate::PinyinLexicon> {
        Arc::new(crate::PinyinLexicon::parse(
            "你好\tnihao\t200\n\
             你\tni\t100\n\
             开\tkai\t90\n\
             开发\tkaifa\t100\n\
             先\txian\t100\n",
        ))
    }

    fn pinyin_session() -> Session {
        Session::pinyin(pinyin_lexicon(), Config::default())
    }

    /// 全拼 accepts a whole run of pinyin letters, shows the syllable split
    /// and commits the focused candidate with Space.
    #[test]
    fn pinyin_types_a_long_code_and_commits_with_space() {
        let mut s = pinyin_session();
        let mut response = s.process_key(b'n' as u32);
        assert!(response.handled);
        assert_eq!(response.candidates[0].text, "你");
        for key in [b'i' as u32, b'h' as u32, b'a' as u32, b'o' as u32] {
            response = s.process_key(key);
            assert!(response.handled);
        }
        assert_eq!(response.preedit, "nihao");
        assert_eq!(response.code_line, "ni hao", "the split comes from the candidate");
        assert_eq!(response.candidates[0].text, "你好");
        assert_eq!(response.candidates[0].code, "ni hao");
        assert!(response.candidates[0].comment.is_empty());

        let committed = s.process_key(KEY_SPACE);
        assert_eq!(committed.commit_text.as_deref(), Some("你好"));
        assert!(committed.candidates.is_empty());
    }

    /// A pinyin encoding outgrows the 形码 word cap: `nihao` is five letters.
    #[test]
    fn pinyin_codes_grow_past_the_word_cap() {
        let mut s = pinyin_session();
        for key in [b'k' as u32, b'a' as u32, b'i' as u32, b'f' as u32, b'a' as u32] {
            let r = s.process_key(key);
            assert!(r.handled, "key {} was swallowed", key as u8 as char);
        }
        assert_eq!(s.input_buffer, "kaifa");
        let r = s.process_key(KEY_SPACE);
        assert_eq!(r.commit_text.as_deref(), Some("开发"));
    }

    /// 全拼 ignores the 顶码规则: the 5th letter extends the code instead of
    /// committing the first four.
    #[test]
    fn pinyin_never_overflow_commits() {
        let conf = Config { commit_next_code: 5, max_code_len: 4, ..Config::default() };
        let mut s = Session::pinyin(pinyin_lexicon(), conf);
        for key in [b'k' as u32, b'a' as u32, b'i' as u32, b'f' as u32] {
            assert!(s.process_key(key).commit_text.is_none(), "no commit at 4 letters");
        }
        let r = s.process_key(b'a' as u32);
        assert!(r.commit_text.is_none(), "the 5th letter must not overflow");
        assert_eq!(r.preedit, "kaifa");
    }

    /// Digits, paging and punctuation follow the table rules in 全拼 too.
    #[test]
    fn pinyin_keeps_the_table_key_model() {
        let conf = Config { ascii_punct: 1, ..Config::default() };
        let mut s = Session::pinyin(pinyin_lexicon(), conf);
        s.process_key(b'n' as u32);
        s.process_key(b'i' as u32);
        let digits = s.process_key(KEY_2);
        assert_eq!(digits.commit_text.as_deref(), Some("你好"), "the 2nd `ni` candidate");

        s.process_key(b'n' as u32);
        let punct = s.process_key(b',' as u32);
        assert_eq!(punct.commit_text.as_deref(), Some("你，"), "中文标点 joins the candidate");
    }

    /// A 全拼 session with a model decodes a combination the table lacks and
    /// commits it with Space, which is the whole point of the sentence stage:
    /// the shipped table has no `huangzhao` row.
    #[test]
    fn pinyin_sentence_session_decodes_out_of_table_words() {
        let Some(model) = crate::ngram::find_model_path()
            .and_then(|path| crate::ngram::NgramModel::load(path).ok())
        else {
            return;
        };
        let lexicon = Arc::new(PinyinLexicon::parse("黄\thuang\t5843\n照\tzhao\t100\n"));
        let conf = Config { ascii_punct: 1, ..Config::default() };
        let mut s = Session::pinyin_sentence(lexicon, Arc::new(model), None, conf);
        for key in b"huangzhao" {
            s.process_key(u32::from(*key));
        }
        let r = s.process_key(KEY_SPACE);
        let committed = r.commit_text.expect("the sentence commits");
        assert!(committed.contains('黄'), "{committed}");
    }

    /// 全拼 owns no reverse lookup: `` ` `` stays the punctuation key the
    /// mapping table defines and never opens a lookup.
    #[test]
    fn pinyin_backtick_stays_punctuation() {
        let conf = Config { ascii_punct: 1, ..Config::default() };
        let mut s = Session::pinyin(pinyin_lexicon(), conf);
        let r = s.process_key(b'`' as u32);
        assert!(r.handled);
        assert_eq!(r.commit_text.as_deref(), Some("`"), "the mapping outputs ` itself");
        assert!(r.preedit.is_empty());

        let mut english = pinyin_session();
        assert!(!english.process_key(b'`' as u32).handled, "English mode releases it");
    }

    // --- 反查 (SPEC §8.3) ---

    fn reverse_session() -> Session {
        let dict = Arc::new(
            Dictionary::parse_str("你\tfl\n你\tflll\n你好\tflol\n好\tolgu\n").expect("table"),
        );
        let lookup = Arc::new(crate::ReverseLookup::from_lexicon(
            Arc::new(crate::PinyinLexicon::parse(
                "你好\tnihao\t200\n你\tni\t100\n好\thao\t90\n",
            )),
            dict.clone(),
        ));
        Session::with_config(dict, Config::default()).with_reverse(lookup)
    }

    #[test]
    fn backtick_then_pinyin_lists_codes() {
        let mut s = reverse_session();
        let opened = s.process_key(b'`' as u32);
        assert!(opened.handled);
        assert!(opened.commit_text.is_none());
        assert_eq!(opened.preedit, "", "the marker lives in the session");
        assert!(opened.candidates.is_empty());

        let mut response = opened;
        for key in [b'n' as u32, b'i' as u32, b'h' as u32] {
            response = s.process_key(key);
        }
        assert_eq!(response.preedit, "nih");
        assert_eq!(
            response.code_line, "`nih",
            "an unfinished query shows the raw letters behind the 反查 marker"
        );
        assert_eq!(response.candidates[0].text, "你好");
        assert_eq!(response.candidates[0].comment, "flol", "the scheme's full code");
        assert_eq!(response.candidates[0].code, "ni hao");

        let committed = s.process_key(KEY_SPACE);
        assert_eq!(committed.commit_text.as_deref(), Some("你好"));
        assert!(committed.candidates.is_empty());
    }

    #[test]
    fn backtick_then_space_commits_the_marker() {
        let mut s = reverse_session();
        s.process_key(b'`' as u32);
        let r = s.process_key(KEY_SPACE);
        assert!(r.handled);
        assert_eq!(r.commit_text.as_deref(), Some("`"));
        assert_eq!(r.preedit, "");
        assert!(r.candidates.is_empty());

        // Idle again: a plain backtick still opens the lookup.
        assert!(s.process_key(b'`' as u32).handled);
    }

    #[test]
    fn escape_leaves_reverse_without_committing() {
        let mut s = reverse_session();
        s.process_key(b'`' as u32);
        s.process_key(b'n' as u32);
        let r = s.process_key(KEY_ESCAPE);
        assert!(r.handled);
        assert!(r.commit_text.is_none());
        assert_eq!(r.preedit, "");
        assert!(r.candidates.is_empty());

        // The next letter is an ordinary 形码 encoding again.
        let r = s.process_key(b'f' as u32);
        assert_eq!(r.preedit, "f");
        assert_eq!(r.candidates[0].text, "你");
    }

    #[test]
    fn backspace_narrows_the_reverse_query() {
        let mut s = reverse_session();
        s.process_key(b'`' as u32);
        for key in [b'n' as u32, b'i' as u32] {
            s.process_key(key);
        }
        let r = s.process_key(KEY_BACKSPACE);
        assert_eq!(r.preedit, "n");
        assert_eq!(r.candidates[0].text, "你", "one letter reads as one syllable");
        assert!(r.candidates.iter().any(|c| c.text == "你好"));
        let r = s.process_key(KEY_BACKSPACE);
        assert_eq!(r.preedit, "");
        assert!(r.candidates.is_empty(), "an empty query lists nothing");
        assert!(s.reversing, "still open until Space, Esc or a commit");
    }

    #[test]
    fn digits_select_a_reverse_candidate() {
        let mut s = reverse_session();
        s.process_key(b'`' as u32);
        s.process_key(b'n' as u32);
        s.process_key(b'i' as u32);
        let r = s.process_key(KEY_2);
        assert_eq!(r.commit_text.as_deref(), Some("你好"), "2nd `ni` candidate");
        assert!(!s.reversing, "a commit closes the lookup");
    }

    /// 反查 does not auto-commit even when the setting is on: one match is
    /// still something the user must choose.
    #[test]
    fn reverse_never_auto_commits() {
        let dict = Arc::new(Dictionary::parse_str("你好\tflol\n").expect("table"));
        let lookup = Arc::new(crate::ReverseLookup::from_lexicon(
            Arc::new(crate::PinyinLexicon::parse("你好\tnihao\t200\n")),
            dict.clone(),
        ));
        let mut s = Session::with_config(
            dict,
            Config { auto_commit: true, auto_commit_length: 1, ..Config::default() },
        )
        .with_reverse(lookup);
        s.process_key(b'`' as u32);
        let r = s.process_key(b'n' as u32);
        assert!(r.commit_text.is_none(), "the lookup waits for Space");
        assert_eq!(r.candidates[0].text, "你好");
    }

    /// A 形码 session without a lookup keeps `` ` `` as punctuation; the
    /// reverse state is what gates it, not the scheme type alone.
    #[test]
    fn a_session_without_a_lookup_keeps_backtick_as_punctuation() {
        let dict = Arc::new(Dictionary::parse_str("a\t工\t100\n").expect("table"));
        let mut s = Session::with_config(dict, Config { ascii_punct: 1, ..Config::default() });
        let r = s.process_key(b'`' as u32);
        assert_eq!(r.commit_text.as_deref(), Some("`"));
    }
}
