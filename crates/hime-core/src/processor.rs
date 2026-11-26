//! Key interpretation (SPEC §3).
//!
//! [`Processor`] is the single place that decides what a key *means*: append
//! to the encoding, delete from it, pick a candidate, abandon the composition,
//! do nothing, or hand the key back to the host application. It reads the
//! composition and never writes it — [`Session`](crate::session::Session)
//! applies the returned [`Action`] — so the whole key model is a pure table
//! that can be tested without any state.
//!
//! Paging (翻页) moves a page cursor over the candidate list without touching
//! the encoding: `-` turns to the previous page and `=` to the next, reported
//! as [`Action::PageUp`] / [`Action::PageDown`] and applied by
//! [`Session`](crate::session::Session). They take effect only while a candidate is on
//! screen; while composing with nothing to page through both keys are
//! swallowed, so neither reaches the host nor commits any text. Idle, they stay
//! ordinary keys the application types.
//!
//! Arrow keys move the focus through the candidates on the current page.
//! The layout decides the axis: horizontal candidates use Left/Right, vertical
//! ones use Up/Down. The other axis is swallowed while a candidate stands, and
//! both axes are swallowed while composing without candidates.
//!
//! Two encodings are longer than a word code: 全拼 (a code is a run of pinyin
//! syllables, capped at [`crate::pinyin::MAX_CODE_LEN`]) and 反查 (the same
//! cap, reached by `` ` `` plus pinyin). Neither overflows and neither takes
//! part in 顶码提交 (SPEC §8.2/§8.3).

use crate::config::{CodeType, Config};
use crate::pinyin::MAX_CODE_LEN as PINYIN_MAX_CODE_LEN;
use crate::punct;
use crate::sentence::config::max_raw_length;
use crate::{
    KEY_ARROW_DOWN, KEY_ARROW_LEFT, KEY_ARROW_RIGHT, KEY_ARROW_UP, KEY_BACKSPACE, KEY_ENTER,
    KEY_ESCAPE, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_SPACE,
};

/// What the engine should do about one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Don't intercept: the host application receives the key as-is.
    Pass,
    /// Append one lowercase ASCII letter to the encoding.
    AppendCode(u8),
    /// Drop the last letter of the encoding.
    DeleteCode,
    /// Drop the encoding without committing anything.
    Discard,
    /// Commit candidate `index` of the rendered list (0-based).
    Select(usize),
    /// Commit the focused candidate (the one Space names); the session owns
    /// the focused index.
    SelectFocused,
    /// Move the focus one candidate toward the start of the page.
    FocusPrevious,
    /// Move the focus one candidate toward the end of the page.
    FocusNext,
    /// Turn to the previous candidate page (`KEY_PAGE_UP`, the `-` key). The
    /// encoding is untouched; `Session` moves a page cursor. Emitted only
    /// while a candidate is on screen.
    PageUp,
    /// Turn to the next candidate page (`KEY_PAGE_DOWN`, the `=` key). The
    /// encoding is untouched; `Session` moves a page cursor. Emitted only
    /// while a candidate is on screen.
    PageDown,
    /// Commit the encoding's current best candidate (the raw letters when no
    /// candidate matches) and start a fresh encoding with this letter. The
    /// `Config::commit_next_code` rule: the N-th letter commits the first
    /// N-1 letters.
    OverflowCommit(u8),
    /// Commit the encoding letters as typed.
    CommitRaw,
    /// Commit the 中文标点 text of this ASCII punctuation key (the
    /// `Config::ascii_punct = 1` mode) and stay idle.
    CommitPunct(u8),
    /// Release this ASCII punctuation key to the host as-is, the first one
    /// right after English letters or digits (`Config::english_punct_after_alnum`,
    /// the `12:30` case). The session remembers the release so the same key
    /// pressed again can correct it (SPEC §3.1).
    ReleasePunct(u8),
    /// Replace the ASCII punctuation the previous press released with this
    /// key's 中文标点 text: the same punctuation key pressed twice.
    CorrectPunct(u8),
    /// Commit the focused candidate (the raw letters when no candidate
    /// matches) followed by the 中文标点 text of this key, and end the
    /// composition.
    CommitCandidatePunct(u8),
    /// Intercepted with no effect, so the host's Marked Text keeps running.
    Noop,
    /// Open 反查 (SPEC §8.3): the following letters are full pinyin and the
    /// candidates carry the active scheme's codes.
    StartReverse,
    /// Commit the literal `` ` `` (the user pressed `` ` `` then Space without
    /// typing a query) and leave 反查.
    CommitBacktick,
}

/// Whether 反查 is possible right now (SPEC §8.3). Only a 形码 or 形码整句
/// scheme has codes to look up, so a 全拼 session always answers
/// [`ReverseState::Unsupported`] and `` ` `` keeps its punctuation meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReverseState {
    /// The scheme has no reverse lookup.
    Unsupported,
    /// The scheme has one and it is closed; `` ` `` opens it.
    Closed,
    /// 反查 is open: the encoding starts with `` ` ``.
    Open,
}

/// The key that opens 反查.
pub const KEY_BACKTICK: u32 = 96;

/// Printable-key ASCII codes; digits `1`..=`5` select candidates 0..=4.
const ASCII_A: u32 = 97;
const ASCII_Z: u32 = 122;
const ASCII_A_UPPER: u32 = 65;
const ASCII_Z_UPPER: u32 = 90;
const ASCII_1: u32 = 49;
/// 分号/引号二三选 selection keys (`Config::semicolon_quote_select`).
const ASCII_SEMICOLON: u32 = 59; // `;`
const ASCII_QUOTE: u32 = 39; // `'`

/// Maps keys to [`Action`]s. Stateless, so one instance serves every session.
pub struct Processor;

impl Processor {
    /// Decides what `key` means for the current composition.
    ///
    /// `code` is the encoding typed so far — empty means idle — and
    /// `candidate_count` is the length of the list the shell is rendering, so
    /// a selection is validated against exactly what the user can see.
    /// `config` supplies this session's code-length cap, page size and
    /// punctuation mode. `after_alnum` reports whether the text the engine
    /// last sent or released to the host ended in an English letter or digit
    /// (the state the `english_punct_after_alnum` rule reads; [`Session`]
    /// owns it). `released_punct` is the punctuation key that rule released
    /// last, still standing at the host's caret; `Some(key)` makes a second
    /// press of that key a correction. `reverse` says whether 反查 is
    /// available and open; while it is open `code` is the pinyin query
    /// behind the `` ` ``.
    pub fn decide(
        key: u32,
        code: &str,
        candidate_count: usize,
        config: Config,
        after_alnum: bool,
        released_punct: Option<u8>,
        reverse: ReverseState,
    ) -> Action {
        let letter = (ASCII_A..=ASCII_Z).contains(&key).then_some(key as u8);

        if reverse == ReverseState::Open {
            return decide_reverse(key, code, candidate_count, config);
        }

        // Idle: a letter starts a composition, 中文标点 mode commits its
        // mapped punctuation, everything else belongs to the application.
        // With `english_punct_after_alnum` on, the first punctuation right
        // after English letters or digits is released instead: a time like
        // `12:30` must reach the host in ASCII. A second press of the same
        // key replaces that released character with 中文标点, which is how
        // the ASCII time is fixed up without reaching for the host. `` ` ``
        // opens 反查 in a scheme that has codes to look up (SPEC §8.3).
        if code.is_empty() {
            if key == KEY_BACKTICK && reverse == ReverseState::Closed {
                return Action::StartReverse;
            }
            return match letter {
                Some(byte) => Action::AppendCode(byte),
                None if chinese_punct(config) && is_punct_key(key) => {
                    if config.english_punct_after_alnum
                        && released_punct == u8::try_from(key).ok()
                        && punct::is_correctable(key as u8)
                    {
                        Action::CorrectPunct(key as u8)
                    } else if config.english_punct_after_alnum && after_alnum {
                        Action::ReleasePunct(key as u8)
                    } else {
                        Action::CommitPunct(key as u8)
                    }
                }
                None => Action::Pass,
            };
        }

        if let Some(byte) = letter {
            // 全拼: one encoding is a run of pinyin syllables, so it grows to
            // the pinyin table's code cap and never overflow-commits.
            if config.code_type == CodeType::Quanpin {
                return if code.len() < PINYIN_MAX_CODE_LEN {
                    Action::AppendCode(byte)
                } else {
                    Action::Noop
                };
            }
            // 形码整句: the key stream is one whole sentence, so it is capped
            // only by the decoder's own limit and never overflows — a further
            // letter extends the sentence, and selecting or Space commits it.
            // With auto-commit on the cap is the tighter `max_raw_length(true)`
            // so the near-cap fallback commits a mature prefix before the key
            // stream can leak; with it off the full manual cap applies. The
            // word-code cap and the overflow rule do not apply here.
            if config.code_type == CodeType::XingmaSentence {
                return if code.len() < max_raw_length(config.auto_commit) {
                    Action::AppendCode(byte)
                } else {
                    Action::Noop
                };
            }
            // The overflow rule wins over the cap: at exactly N-1 letters the
            // N-th letter commits and restarts the encoding, even where the
            // cap would otherwise swallow it (琉璃: max_code 4, N 5).
            if config.commit_next_code >= 2 && code.len() == config.commit_next_code - 1 {
                return Action::OverflowCommit(byte);
            }
            return if code.len() < config.max_code_len {
                Action::AppendCode(byte)
            } else {
                Action::Noop
            };
        }

        match key {
            KEY_BACKSPACE => Action::DeleteCode,
            KEY_ESCAPE => Action::Discard,
            // Space commits the focused candidate; with nothing selectable,
            // commit the raw encoding instead.
            KEY_SPACE if candidate_count > 0 => Action::SelectFocused,
            KEY_SPACE => Action::CommitRaw,
            KEY_ENTER => Action::CommitRaw,
            // The candidate-window orientation picks the moving axis; the
            // other axis is swallowed so it cannot move the host's caret
            // through a standing composition.
            key @ (KEY_ARROW_LEFT | KEY_ARROW_RIGHT | KEY_ARROW_UP | KEY_ARROW_DOWN) => {
                arrow_focus(key, candidate_count, config)
            }
            // Paging turns the candidate list but only while a candidate is
            // shown. With nothing to page through the key is swallowed:
            // neither `-` nor `=` may leak to the host mid-composition, and
            // neither commits punctuation.
            KEY_PAGE_UP if candidate_count > 0 => Action::PageUp,
            KEY_PAGE_UP => Action::Noop,
            KEY_PAGE_DOWN if candidate_count > 0 => Action::PageDown,
            KEY_PAGE_DOWN => Action::Noop,
            // 分号/引号二三选 (`Config::semicolon_quote_select`): once any
            // candidate is on screen, `;` picks the 2nd candidate and `'`
            // the 3rd. A page shorter than the pick clamps to its last
            // entry, so with one candidate both commit the 1st, and with
            // two candidates both commit the 2nd. This runs before the
            // punctuation arms, so `;` and `'` stop emitting 中文标点 while
            // a candidate stands.
            key @ (ASCII_SEMICOLON | ASCII_QUOTE)
                if config.semicolon_quote_select && candidate_count > 0 =>
            {
                let wanted = if key == ASCII_QUOTE { 2 } else { 1 };
                Action::Select(wanted.min(candidate_count - 1))
            }
            // A digit whose candidate is missing, or one beyond the keys the
            // page can offer, is swallowed rather than released.
            key if is_selection_key(key, config) => {
                let index = (key - ASCII_1) as usize;
                if index < candidate_count.min(config.page_size) {
                    Action::Select(index)
                } else {
                    Action::Noop
                }
            }
            // 中文标点 mode consumes the keys its table maps; the mapped
            // text joins the candidate on commit.
            key if chinese_punct(config) && is_punct_key(key) => {
                Action::CommitCandidatePunct(key as u8)
            }
            // Printable punctuation the mode does not map stays with the
            // application: releasing it leaves the Marked Text standing.
            key if is_symbol_key(key) => Action::Pass,
            // A capital letter (the shell folds plain letters to lowercase,
            // so 65..=90 is a deliberate Shift + letter) never joins a
            // lowercase encoding: released like a symbol, the host inserts
            // the capital and the Marked Text keeps running.
            ASCII_A_UPPER..=ASCII_Z_UPPER => Action::Pass,
            // Any other key code is swallowed: composition state
            // stays untouched so the host's Marked Text is never interrupted
            // mid-composition.
            _ => Action::Noop,
        }
    }
}

/// The key table while 反查 is open (SPEC §8.3). `code` is the pinyin query
/// behind the `` ` ``: empty right after opening, letters as they come.
fn decide_reverse(key: u32, code: &str, candidate_count: usize, config: Config) -> Action {
    if let Some(byte) = (ASCII_A..=ASCII_Z).contains(&key).then_some(key as u8) {
        return if code.len() < PINYIN_MAX_CODE_LEN {
            Action::AppendCode(byte)
        } else {
            Action::Noop
        };
    }
    match key {
        // Backspace eats the query one letter at a time; on an empty query
        // there is nothing left to eat, so the lookup closes — the same
        // meaning Esc has.
        KEY_BACKSPACE if code.is_empty() => Action::Discard,
        KEY_BACKSPACE => Action::DeleteCode,
        // Esc, Enter and a second `` ` `` close the lookup; nothing is
        // committed because the query was never text the user wanted.
        KEY_ESCAPE | KEY_ENTER | KEY_BACKTICK => Action::Discard,
        // Space with no query yet is the literal backtick (the documented way
        // to type `` ` `` at all); with a query it commits the focused
        // candidate, and with nothing to commit the lookup just ends.
        KEY_SPACE if code.is_empty() => Action::CommitBacktick,
        KEY_SPACE if candidate_count > 0 => Action::SelectFocused,
        KEY_SPACE => Action::Discard,
        key @ (KEY_ARROW_LEFT | KEY_ARROW_RIGHT | KEY_ARROW_UP | KEY_ARROW_DOWN) => {
            arrow_focus(key, candidate_count, config)
        }
        KEY_PAGE_UP if candidate_count > 0 => Action::PageUp,
        KEY_PAGE_UP => Action::Noop,
        KEY_PAGE_DOWN if candidate_count > 0 => Action::PageDown,
        KEY_PAGE_DOWN => Action::Noop,
        key @ (ASCII_SEMICOLON | ASCII_QUOTE)
            if config.semicolon_quote_select && candidate_count > 0 =>
        {
            let wanted = if key == ASCII_QUOTE { 2 } else { 1 };
            Action::Select(wanted.min(candidate_count - 1))
        }
        key if is_selection_key(key, config) => {
            let index = (key - ASCII_1) as usize;
            if index < candidate_count.min(config.page_size) {
                Action::Select(index)
            } else {
                Action::Noop
            }
        }
        key if chinese_punct(config) && is_punct_key(key) => {
            Action::CommitCandidatePunct(key as u8)
        }
        key if is_symbol_key(key) => Action::Pass,
        ASCII_A_UPPER..=ASCII_Z_UPPER => Action::Pass,
        _ => Action::Noop,
    }
}

/// Focus movement for one arrow key. Only the axis matching the candidate
/// layout moves, and only while the current page holds a candidate; an
/// orthogonal arrow (and any arrow with nothing to focus) is a no-op.
fn arrow_focus(key: u32, candidate_count: usize, config: Config) -> Action {
    if candidate_count == 0 {
        return Action::Noop;
    }
    match (key, config.vertical_candidates) {
        (KEY_ARROW_LEFT, false) | (KEY_ARROW_UP, true) => Action::FocusPrevious,
        (KEY_ARROW_RIGHT, false) | (KEY_ARROW_DOWN, true) => Action::FocusNext,
        _ => Action::Noop,
    }
}

/// Digits that can name a candidate: `1` up to the configured page size.
fn is_selection_key(key: u32, config: Config) -> bool {
    (ASCII_1..ASCII_1 + config.page_size as u32).contains(&key)
}

/// The 中文标点 mode of `Config::ascii_punct`; any other value is English.
fn chinese_punct(config: Config) -> bool {
    config.ascii_punct == 1
}

/// An ASCII punctuation key whose key the 中文标点 mapping covers.
fn is_punct_key(key: u32) -> bool {
    u8::try_from(key).is_ok_and(punct::is_mapped)
}

/// A printable ASCII punctuation/symbol key (letters and digits excluded).
fn is_symbol_key(key: u32) -> bool {
    u8::try_from(key).is_ok_and(punct::is_symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sentence::config::{AUTO_COMMIT_MAX_RAW_LENGTH, MAX_RAW_LENGTH};
    use crate::{
        KEY_ARROW_DOWN, KEY_ARROW_LEFT, KEY_ARROW_RIGHT, KEY_ARROW_UP, KEY_BACKSPACE, KEY_ENTER,
        KEY_ESCAPE, KEY_SPACE,
    };

    const KEY_A: u32 = 97;
    const KEY_D: u32 = 100;
    const KEY_1: u32 = 49;
    const KEY_2: u32 = 50;
    const KEY_5: u32 = 53;
    const KEY_6: u32 = 54;
    const KEY_7: u32 = 55;
    const KEY_9: u32 = 57;

    /// Ordinary decision with 反查 unavailable: the shape nearly every case
    /// below has.
    fn decide(key: u32, code: &str, candidates: usize, config: Config, after_alnum: bool) -> Action {
        Processor::decide(
            key,
            code,
            candidates,
            config,
            after_alnum,
            None,
            ReverseState::Unsupported,
        )
    }

    fn idle(key: u32) -> Action {
        decide(key, "", 0, Config::default(), false)
    }

    fn composing(key: u32, code: &str, candidates: usize) -> Action {
        decide(key, code, candidates, Config::default(), false)
    }

    #[test]
    fn idle_only_starts_on_a_letter() {
        assert_eq!(idle(KEY_A), Action::AppendCode(b'a'));
        assert_eq!(idle(KEY_D), Action::AppendCode(b'd'));
        for key in [
            KEY_SPACE,
            KEY_BACKSPACE,
            KEY_ENTER,
            KEY_ESCAPE,
            KEY_1,
            KEY_9,
            b'-' as u32,
            0,
        ] {
            assert_eq!(idle(key), Action::Pass, "idle key {key} must pass through");
        }
    }

    #[test]
    fn letters_append_until_the_code_is_full() {
        assert_eq!(composing(KEY_A, "a", 1), Action::AppendCode(b'a'));
        assert_eq!(
            composing(KEY_A, "abcd", 1),
            Action::Noop,
            "a letter over MAX_CODE_LEN is swallowed, not appended or released"
        );
    }

    #[test]
    fn editing_keys_map_to_their_actions() {
        assert_eq!(composing(KEY_BACKSPACE, "a", 1), Action::DeleteCode);
        assert_eq!(composing(KEY_ESCAPE, "a", 1), Action::Discard);
        assert_eq!(composing(KEY_ENTER, "a", 1), Action::CommitRaw);
    }

    #[test]
    fn space_commits_the_focused_candidate_or_the_raw_code() {
        assert_eq!(composing(KEY_SPACE, "a", 3), Action::SelectFocused);
        assert_eq!(composing(KEY_SPACE, "q", 0), Action::CommitRaw);
    }

    /// The candidate layout picks the arrow axis. While a candidate stands,
    /// the matching pair moves the focus and the other pair is swallowed;
    /// with no candidate every arrow is swallowed, and idle they pass.
    #[test]
    fn arrow_keys_move_along_the_candidate_layout_axis() {
        let horizontal = Config::default();
        assert_eq!(
            decide(KEY_ARROW_LEFT, "a", 3, horizontal, false),
            Action::FocusPrevious
        );
        assert_eq!(
            decide(KEY_ARROW_RIGHT, "a", 3, horizontal, false),
            Action::FocusNext
        );
        assert_eq!(
            decide(KEY_ARROW_UP, "a", 3, horizontal, false),
            Action::Noop,
            "the vertical pair is inert in a horizontal window"
        );
        assert_eq!(
            decide(KEY_ARROW_DOWN, "a", 3, horizontal, false),
            Action::Noop
        );

        let vertical = Config { vertical_candidates: true, ..Config::default() };
        assert_eq!(
            decide(KEY_ARROW_UP, "a", 3, vertical, false),
            Action::FocusPrevious
        );
        assert_eq!(
            decide(KEY_ARROW_DOWN, "a", 3, vertical, false),
            Action::FocusNext
        );
        assert_eq!(
            decide(KEY_ARROW_LEFT, "a", 3, vertical, false),
            Action::Noop,
            "the horizontal pair is inert in a vertical column"
        );
        assert_eq!(
            decide(KEY_ARROW_RIGHT, "a", 3, vertical, false),
            Action::Noop
        );

        // Nothing to focus: every arrow is swallowed mid-composition.
        for key in [KEY_ARROW_LEFT, KEY_ARROW_RIGHT, KEY_ARROW_UP, KEY_ARROW_DOWN] {
            assert_eq!(decide(key, "q", 0, horizontal, false), Action::Noop);
            assert_eq!(decide(key, "q", 0, vertical, false), Action::Noop);
            assert_eq!(idle(key), Action::Pass);
        }
    }

    /// Digits are valid only against the list the shell is rendering, which is
    /// what keeps key labels and commits in agreement.
    #[test]
    fn digits_select_only_within_the_rendered_list() {
        assert_eq!(composing(KEY_1, "a", 1), Action::Select(0));
        assert_eq!(composing(KEY_2, "a", 5), Action::Select(1));
        assert_eq!(composing(KEY_5, "a", 5), Action::Select(4));
        assert_eq!(
            composing(KEY_2, "ad", 1),
            Action::Noop,
            "no candidate behind 2"
        );
        assert_eq!(
            composing(KEY_6, "a", 5),
            Action::Noop,
            "6 is beyond the page keys"
        );
        assert_eq!(
            composing(KEY_9, "a", 5),
            Action::Noop,
            "9 is beyond the page keys"
        );
        assert_eq!(composing(KEY_1, "a", 0), Action::Noop);
    }

    #[test]
    fn anything_else_is_swallowed_while_composing() {
        for key in [b'0' as u32, 0, 0x1000] {
            assert_eq!(composing(key, "a", 5), Action::Noop, "key {key}");
        }
    }

    /// English punctuation mode (the default) never claims a punctuation
    /// key: idle releases it and composing releases it too. The paging keys
    /// `-`/`=` are handled separately (see `page_keys_only_turn_with_candidates`).
    #[test]
    fn english_mode_releases_punctuation() {
        for key in [b',' as u32, b'.' as u32, b'?' as u32, b'"' as u32] {
            assert_eq!(idle(key), Action::Pass, "idle key {key}");
            assert_eq!(
                composing(key, "a", 1),
                Action::Pass,
                "composing key {key} must reach the host"
            );
        }
    }

    /// The paging keys turn the list only while a candidate is shown. With no
    /// candidate they are swallowed — nothing is committed and nothing reaches
    /// the host — and only when idle do they stay ordinary typed keys.
    #[test]
    fn page_keys_only_turn_with_candidates() {
        assert_eq!(composing(KEY_PAGE_UP, "a", 1), Action::PageUp);
        assert_eq!(composing(KEY_PAGE_DOWN, "a", 1), Action::PageDown);
        assert_eq!(
            composing(KEY_PAGE_UP, "q", 0),
            Action::Noop,
            "no candidate: - does nothing"
        );
        assert_eq!(
            composing(KEY_PAGE_DOWN, "q", 0),
            Action::Noop,
            "no candidate: = does nothing"
        );
        assert_eq!(idle(KEY_PAGE_UP), Action::Pass);
        assert_eq!(idle(KEY_PAGE_DOWN), Action::Pass);
    }

    /// Swallowing the page keys does not depend on 中文标点: `-` emits no
    /// punctuation there either, with or without a candidate.
    #[test]
    fn page_keys_stay_silent_in_chinese_punct_mode() {
        let cn = Config {
            ascii_punct: 1,
            ..Config::default()
        };
        assert_eq!(decide(KEY_PAGE_UP, "a", 1, cn, false), Action::PageUp);
        assert_eq!(
            decide(KEY_PAGE_UP, "q", 0, cn, false),
            Action::Noop,
            "no candidate: - emits nothing"
        );
        assert_eq!(
            decide(KEY_PAGE_DOWN, "q", 0, cn, false),
            Action::Noop,
            "no candidate: = emits nothing"
        );
    }

    /// The 中文标点 mode claims only the keys its table maps; letters,
    /// digits and editing keys keep their own meanings.
    #[test]
    fn chinese_mode_claims_mapped_punctuation() {
        let cn = Config {
            ascii_punct: 1,
            ..Config::default()
        };
        assert_eq!(
            decide(b',' as u32, "", 0, cn, false),
            Action::CommitPunct(b',')
        );
        assert_eq!(
            decide(b',' as u32, "a", 1, cn, false),
            Action::CommitCandidatePunct(b','),
            "mid-composition punctuation joins the candidate commit"
        );
        assert_eq!(
            decide(b'%' as u32, "", 0, cn, false),
            Action::Pass,
            "unmapped keys stay passed through"
        );
        assert_eq!(
            decide(b'%' as u32, "a", 1, cn, false),
            Action::Pass,
            "unmapped keys are released even mid-composition"
        );
        assert_eq!(
            decide(KEY_A, "", 0, cn, false),
            Action::AppendCode(b'a')
        );
        assert_eq!(decide(KEY_1, "a", 1, cn, false), Action::Select(0));
        assert_eq!(
            decide(KEY_BACKSPACE, "a", 1, cn, false),
            Action::DeleteCode
        );
        assert_eq!(decide(KEY_SPACE, "a", 1, cn, false), Action::SelectFocused);
    }

    /// The preferences window moves both limits, and the decision table
    /// follows the session's config rather than the compiled-in defaults.
    #[test]
    fn config_moves_the_page_and_code_limits() {
        let custom = Config {
            page_size: 6,
            max_code_len: 2,
            ..Config::default()
        };
        assert_eq!(decide(KEY_6, "a", 6, custom, false), Action::Select(5));
        assert_eq!(
            decide(KEY_7, "a", 6, custom, false),
            Action::Noop,
            "7 is beyond a 6-key page"
        );
        assert_eq!(
            decide(KEY_A, "ab", 1, custom, false),
            Action::Noop,
            "a letter over the configured code cap is swallowed"
        );
    }

    /// `commit_next_code`: the N-th letter commits and restarts instead of
    /// appending or swallowing; 0 keeps the old capped behavior.
    #[test]
    fn commit_next_code_overflows_at_the_nth_letter() {
        let overflow = Config {
            max_code_len: 4,
            commit_next_code: 5,
            ..Config::default()
        };
        assert_eq!(
            decide(KEY_D, "abcd", 3, overflow, false),
            Action::OverflowCommit(b'd'),
            "the 5th letter commits at N=5"
        );
        assert_eq!(
            decide(KEY_D, "abc", 3, overflow, false),
            Action::AppendCode(b'd'),
            "the 4th letter still extends the encoding"
        );
        assert_eq!(
            decide(KEY_D, "abcd", 0, overflow, false),
            Action::OverflowCommit(b'd'),
            "without a candidate the raw letters are what overflow commits"
        );

        let off = Config {
            max_code_len: 4,
            commit_next_code: 0,
            ..Config::default()
        };
        assert_eq!(
            decide(KEY_D, "abcd", 1, off, false),
            Action::Noop,
            "0 turns the rule off"
        );

        // Unreachable above the cap: the cap keeps swallowing.
        let unreachable = Config {
            max_code_len: 4,
            commit_next_code: 6,
            ..Config::default()
        };
        assert_eq!(
            decide(KEY_D, "abcd", 1, unreachable, false),
            Action::Noop
        );

        // Reachable below the cap: overflow wins before the code grows on.
        let below_cap = Config {
            max_code_len: 6,
            commit_next_code: 4,
            ..Config::default()
        };
        assert_eq!(
            decide(KEY_D, "abc", 1, below_cap, false),
            Action::OverflowCommit(b'd')
        );
    }

    /// 形码整句 ignores the word-code cap and the overflow rule: a further
    /// letter extends the sentence rather than committing at the 5th key or
    /// stopping at `max_code_len`.
    #[test]
    fn sentence_mode_ignores_the_word_cap_and_never_overflows() {
        let word = Config { max_code_len: 4, commit_next_code: 5, ..Config::default() };
        assert_eq!(
            decide(KEY_D, "abcd", 1, word, false),
            Action::OverflowCommit(b'd'),
            "the same config overflows in word mode"
        );

        let sentence = Config {
            code_type: CodeType::XingmaSentence,
            max_code_len: 4,
            commit_next_code: 5,
            ..Config::default()
        };
        assert_eq!(
            decide(KEY_D, "abcd", 1, sentence, false),
            Action::AppendCode(b'd'),
            "no overflow at the 5th letter in 形码整句"
        );
        let long = "a".repeat(64);
        assert_eq!(
            decide(KEY_D, &long, 1, sentence, false),
            Action::AppendCode(b'd'),
            "a long code keeps extending"
        );
    }

    /// The sentence code is capped at the decoder's own limit; letters past it
    /// are swallowed, matching how a word code treats letters over its cap.
    /// Auto-commit off keeps the full manual cap.
    #[test]
    fn sentence_mode_caps_the_code_at_the_decoder_limit() {
        let sentence = Config { code_type: CodeType::XingmaSentence, ..Config::default() };
        let at_limit = "a".repeat(MAX_RAW_LENGTH - 1);
        assert_eq!(
            decide(KEY_A, &at_limit, 1, sentence, false),
            Action::AppendCode(b'a'),
            "the letter that reaches the limit is accepted"
        );
        let full = "a".repeat(MAX_RAW_LENGTH);
        assert_eq!(
            decide(KEY_A, &full, 1, sentence, false),
            Action::Noop,
            "letters past the limit are swallowed"
        );
    }

    /// With auto-commit on the cap drops to the tighter auto-commit limit, so
    /// the near-cap fallback commits a mature prefix before a long code can
    /// reach the manual cap (the reference `SentenceConfig.maxRawLength`).
    #[test]
    fn sentence_mode_caps_lower_with_auto_commit_on() {
        let sentence = Config {
            code_type: CodeType::XingmaSentence,
            auto_commit: true,
            ..Config::default()
        };
        let at_limit = "a".repeat(AUTO_COMMIT_MAX_RAW_LENGTH - 1);
        assert_eq!(
            decide(KEY_A, &at_limit, 1, sentence, false),
            Action::AppendCode(b'a'),
            "the letter reaching the auto-commit cap is accepted"
        );
        let full = "a".repeat(AUTO_COMMIT_MAX_RAW_LENGTH);
        assert_eq!(
            decide(KEY_A, &full, 1, sentence, false),
            Action::Noop,
            "letters past the auto-commit cap are swallowed"
        );
        // Well below the manual cap: a code the manual cap would still allow.
        assert_eq!(
            decide(KEY_A, &full, 1, Config { auto_commit: false, ..sentence }, false),
            Action::AppendCode(b'a'),
            "auto-commit off keeps extending past the 64-key cap"
        );
    }

    /// Sentence mode changes only how letters accumulate: Space, digits, the
    /// editing keys and Enter keep their meanings, so the chosen sentence
    /// commits through the same actions the table path uses.
    #[test]
    fn sentence_mode_keeps_selection_and_editing_keys() {
        let sentence = Config { code_type: CodeType::XingmaSentence, ..Config::default() };
        assert_eq!(
            decide(KEY_SPACE, "jin", 3, sentence, false),
            Action::SelectFocused
        );
        assert_eq!(decide(KEY_SPACE, "q", 0, sentence, false), Action::CommitRaw);
        assert_eq!(decide(KEY_2, "jin", 3, sentence, false), Action::Select(1));
        assert_eq!(decide(KEY_BACKSPACE, "jin", 3, sentence, false), Action::DeleteCode);
        assert_eq!(decide(KEY_ESCAPE, "jin", 3, sentence, false), Action::Discard);
        assert_eq!(decide(KEY_ENTER, "jin", 3, sentence, false), Action::CommitRaw);
    }

    /// `english_punct_after_alnum` on: the first mapped punctuation right
    /// after English letters or digits is released to the host as ASCII;
    /// with the flag down, mid-composition or with the switch off the rule
    /// stays out of the way.
    #[test]
    fn english_punct_after_alnum_releases_the_first_mapped_punctuation() {
        let cn = Config {
            ascii_punct: 1,
            english_punct_after_alnum: true,
            ..Config::default()
        };
        assert_eq!(
            decide(b',' as u32, "", 0, cn, true),
            Action::ReleasePunct(b','),
            "after alphanumerics , reaches the host as ,"
        );
        assert_eq!(decide(b',' as u32, "", 0, cn, false), Action::CommitPunct(b','));
        assert_eq!(
            decide(b',' as u32, "a", 1, cn, true),
            Action::CommitCandidatePunct(b','),
            "mid-composition punctuation joins the candidate regardless"
        );
        let off = Config { ascii_punct: 1, ..Config::default() };
        assert_eq!(
            decide(b',' as u32, "", 0, off, true),
            Action::CommitPunct(b','),
            "the rule is inert while the switch is off"
        );
    }

    /// A second press of the released key is a correction: 中文标点 in place of
    /// the ASCII character the host inserted. Only the very key that was
    /// released corrects, only while it still stands at the caret.
    #[test]
    fn second_press_of_the_released_punctuation_corrects_it() {
        let cn = Config {
            ascii_punct: 1,
            english_punct_after_alnum: true,
            ..Config::default()
        };
        assert_eq!(
            Processor::decide(
                b':' as u32, "", 0, cn, false, Some(b':'), ReverseState::Unsupported
            ),
            Action::CorrectPunct(b':'),
            "the same key pressed twice becomes 中文标点 even with the rule idle"
        );
        assert_eq!(
            Processor::decide(
                b';' as u32, "", 0, cn, false, Some(b':'), ReverseState::Unsupported
            ),
            Action::CommitPunct(b';'),
            "another punctuation key is an ordinary 中文标点 commit"
        );
        let off = Config { ascii_punct: 1, ..Config::default() };
        assert_eq!(
            Processor::decide(
                b':' as u32, "", 0, off, false, Some(b':'), ReverseState::Unsupported
            ),
            Action::CommitPunct(b':'),
            "with the switch off nothing is ever released, so nothing corrects"
        );
        assert_eq!(
            Processor::decide(
                b':' as u32, "a", 1, cn, false, Some(b':'), ReverseState::Unsupported
            ),
            Action::CommitCandidatePunct(b':'),
            "mid-composition the punctuation still joins the candidate commit"
        );
        // `/`, `` ` `` and `~` read the same in both widths, so a second
        // press types a second character rather than rewriting the first.
        assert_eq!(
            Processor::decide(
                b'/' as u32, "", 0, cn, false, Some(b'/'), ReverseState::Unsupported
            ),
            Action::CommitPunct(b'/'),
            "a key whose mapping equals its ASCII form is not correctable"
        );
    }

    /// Capital letters (the shell forwards Shift + letter unchanged) never
    /// join a lowercase encoding: idle or composing they are released to the
    /// host like unmapped symbols, and the composition keeps running.
    #[test]
    fn capitals_pass_to_the_host() {
        assert_eq!(idle(b'H' as u32), Action::Pass);
        assert_eq!(composing(b'H' as u32, "a", 3), Action::Pass, "a capital never extends the code");
        let cn = Config { ascii_punct: 1, ..Config::default() };
        assert_eq!(decide(b'H' as u32, "a", 1, cn, false), Action::Pass);
    }

    /// 分号/引号二三选: `;` selects the 2nd candidate, `'` the 3rd, and a
    /// shorter page clamps both to its last entry — with one candidate all
    /// three selection keys commit the 1st, with two candidates both `;`
    /// and `'` commit the 2nd.
    #[test]
    fn semicolon_quote_select_second_and_third_candidates() {
        let sel = Config { semicolon_quote_select: true, ..Config::default() };
        let key_semicolon = b';' as u32;
        let key_quote = b'\'' as u32;
        assert_eq!(decide(key_semicolon, "a", 1, sel, false), Action::Select(0));
        assert_eq!(decide(key_quote, "a", 1, sel, false), Action::Select(0));
        assert_eq!(decide(key_semicolon, "a", 2, sel, false), Action::Select(1));
        assert_eq!(decide(key_quote, "a", 2, sel, false), Action::Select(1));
        assert_eq!(decide(key_semicolon, "a", 3, sel, false), Action::Select(1));
        assert_eq!(decide(key_quote, "a", 3, sel, false), Action::Select(2));
        assert_eq!(decide(key_quote, "a", 5, sel, false), Action::Select(2));
    }

    /// The selection keys hold only while a candidate is on screen. With
    /// nothing to select, `;` and `'` keep their punctuation meanings —
    /// 中文标点 commits them, English mode releases them — and while idle
    /// they are plain punctuation. With the switch off they never select.
    #[test]
    fn semicolon_quote_fall_back_to_punctuation() {
        let sel_cn = Config {
            semicolon_quote_select: true,
            ascii_punct: 1,
            ..Config::default()
        };
        let sel_en = Config { semicolon_quote_select: true, ..Config::default() };
        assert_eq!(
            decide(b';' as u32, "q", 0, sel_cn, false),
            Action::CommitCandidatePunct(b';'),
            "no candidate: ; is punctuation again"
        );
        assert_eq!(
            decide(b'\'' as u32, "q", 0, sel_cn, false),
            Action::CommitCandidatePunct(b'\'')
        );
        assert_eq!(decide(b';' as u32, "q", 0, sel_en, false), Action::Pass);
        assert_eq!(decide(b';' as u32, "", 0, sel_cn, false), Action::CommitPunct(b';'));
        let off_cn = Config { ascii_punct: 1, ..Config::default() };
        assert_eq!(
            decide(b';' as u32, "a", 3, off_cn, false),
            Action::CommitCandidatePunct(b';'),
            "off: ; joins the candidate commit as before"
        );
    }

    // --- 全拼 and 反查 (SPEC §8) ---

    /// 全拼 accepts letters up to the pinyin cap and ignores the word cap and
    /// 顶码 rule entirely.
    #[test]
    fn quanpin_caps_at_the_pinyin_code_length_and_never_overflows() {
        let quanpin = Config {
            code_type: CodeType::Quanpin,
            max_code_len: 4,
            commit_next_code: 5,
            ..Config::default()
        };
        assert_eq!(
            Processor::decide(KEY_A, "abcd", 3, quanpin, false, None, ReverseState::Unsupported),
            Action::AppendCode(b'a'),
            "a word cap of 4 does not apply to a pinyin code"
        );
        assert_eq!(
            Processor::decide(KEY_A, "abcd", 3, quanpin, false, None, ReverseState::Unsupported),
            Action::AppendCode(b'a'),
            "nor does the overflow rule"
        );
        let full = "a".repeat(PINYIN_MAX_CODE_LEN);
        assert_eq!(
            Processor::decide(KEY_A, &full, 1, quanpin, false, None, ReverseState::Unsupported),
            Action::Noop,
            "the pinyin cap still ends the code"
        );
    }

    /// `` ` `` opens 反查 only where a lookup exists; without one it keeps the
    /// punctuation meaning the mapping table gives it.
    #[test]
    fn backtick_opens_reverse_only_when_available() {
        let config = Config::default();
        assert_eq!(
            Processor::decide(KEY_BACKTICK, "", 0, config, false, None, ReverseState::Closed),
            Action::StartReverse
        );
        assert_eq!(
            Processor::decide(KEY_BACKTICK, "", 0, config, false, None, ReverseState::Unsupported),
            Action::Pass,
            "an unsupported lookup leaves the key to the host in English mode"
        );
        let cn = Config { ascii_punct: 1, ..Config::default() };
        assert_eq!(
            Processor::decide(KEY_BACKTICK, "", 0, cn, false, None, ReverseState::Unsupported),
            Action::CommitPunct(b'`'),
            "and to the punctuation table in 中文标点 mode"
        );
        assert_eq!(
            Processor::decide(KEY_A, "", 0, config, false, None, ReverseState::Closed),
            Action::AppendCode(b'a'),
            "ordinary idle keys are unaffected by an available lookup"
        );
    }

    /// The 反查 key table: letters extend the query, Space commits the marker
    /// when the query is empty and the focused candidate otherwise, and the
    /// closing keys commit nothing.
    #[test]
    fn reverse_keys_extend_select_and_close() {
        let config = Config::default();
        let open = |key, code: &str, candidates| {
            Processor::decide(key, code, candidates, config, false, None, ReverseState::Open)
        };
        assert_eq!(open(KEY_A, "", 0), Action::AppendCode(b'a'));
        assert_eq!(open(KEY_A, "nihao", 3), Action::AppendCode(b'a'));
        let full = "a".repeat(PINYIN_MAX_CODE_LEN);
        assert_eq!(open(KEY_A, &full, 1), Action::Noop, "the pinyin cap applies");

        assert_eq!(open(KEY_SPACE, "", 0), Action::CommitBacktick);
        assert_eq!(open(KEY_SPACE, "ni", 2), Action::SelectFocused);
        assert_eq!(open(KEY_SPACE, "zz", 0), Action::Discard, "nothing to commit");

        assert_eq!(open(KEY_ESCAPE, "ni", 2), Action::Discard);
        assert_eq!(open(KEY_ENTER, "ni", 2), Action::Discard);
        assert_eq!(open(KEY_BACKTICK, "ni", 2), Action::Discard);
        assert_eq!(open(KEY_BACKSPACE, "ni", 2), Action::DeleteCode);
        assert_eq!(
            open(KEY_BACKSPACE, "", 0),
            Action::Discard,
            "deleting past the query closes the lookup"
        );

        assert_eq!(open(KEY_1, "ni", 3), Action::Select(0));
        assert_eq!(open(KEY_PAGE_DOWN, "ni", 3), Action::PageDown);
        assert_eq!(open(KEY_ARROW_RIGHT, "ni", 3), Action::FocusNext);
        assert_eq!(open(b'%' as u32, "ni", 3), Action::Pass);
        assert_eq!(open(b'H' as u32, "ni", 3), Action::Pass);
        assert_eq!(open(0x1000, "ni", 3), Action::Noop);
    }

    /// In 反查 the punctuation table still applies, so a 中文标点 comma commits
    /// the focused candidate with the punctuation after it.
    #[test]
    fn reverse_keeps_the_punctuation_rules() {
        let cn = Config { ascii_punct: 1, ..Config::default() };
        assert_eq!(
            Processor::decide(b',' as u32, "ni", 2, cn, false, None, ReverseState::Open),
            Action::CommitCandidatePunct(b',')
        );
        let sel = Config { semicolon_quote_select: true, ..Config::default() };
        assert_eq!(
            Processor::decide(b';' as u32, "ni", 3, sel, false, None, ReverseState::Open),
            Action::Select(1)
        );
    }
}
