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
//! [`Session`](crate::session::Session). They fire only while a candidate is on
//! screen; while composing with nothing to page through both keys are
//! swallowed, so neither reaches the host nor commits any text. Idle, they stay
//! ordinary keys the application types.

use crate::config::{CodeType, Config};
use crate::punct;
use crate::sentence::config::MAX_RAW_LENGTH;
use crate::{KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_SPACE};

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
    /// Commit the current encoding's first candidate (the raw letters when
    /// no candidate matches) followed by the 中文标点 text of this key, and
    /// end the composition.
    CommitCandidatePunct(u8),
    /// Intercepted with no effect, so the host's Marked Text keeps running.
    Noop,
}

/// Printable-key ASCII codes; digits `1`..=`5` select candidates 0..=4.
const ASCII_A: u32 = 97;
const ASCII_Z: u32 = 122;
const ASCII_1: u32 = 49;

/// Maps keys to [`Action`]s. Stateless, so one instance serves every session.
pub struct Processor;

impl Processor {
    /// Decides what `key` means for the current composition.
    ///
    /// `code` is the encoding typed so far — empty means idle — and
    /// `candidate_count` is the length of the list the shell is rendering, so
    /// a selection is validated against exactly what the user can see.
    /// `config` supplies this session's code-length cap, page size and
    /// punctuation mode.
    pub fn decide(key: u32, code: &str, candidate_count: usize, config: Config) -> Action {
        let letter = (ASCII_A..=ASCII_Z).contains(&key).then_some(key as u8);

        // Idle: a letter starts a composition, 中文标点 mode commits its
        // mapped punctuation, everything else belongs to the application.
        if code.is_empty() {
            return match letter {
                Some(byte) => Action::AppendCode(byte),
                None if chinese_punct(config) && is_punct_key(key) => {
                    Action::CommitPunct(key as u8)
                }
                None => Action::Pass,
            };
        }

        if let Some(byte) = letter {
            // 形码整句: the key stream is one whole sentence, so it is capped
            // only by the decoder's own limit and never overflows — a further
            // letter extends the sentence, and selecting or Space commits it.
            // The word-code cap and the overflow rule do not apply here.
            if config.code_type == CodeType::XingmaSentence {
                return if code.len() < MAX_RAW_LENGTH {
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
            // The first candidate wins; with nothing selectable, commit the
            // raw encoding instead.
            KEY_SPACE if candidate_count > 0 => Action::Select(0),
            KEY_SPACE => Action::CommitRaw,
            KEY_ENTER => Action::CommitRaw,
            // Paging turns the candidate list but only while a candidate is
            // shown. With nothing to page through the key is swallowed:
            // neither `-` nor `=` may leak to the host mid-composition, and
            // neither commits punctuation.
            KEY_PAGE_UP if candidate_count > 0 => Action::PageUp,
            KEY_PAGE_UP => Action::Noop,
            KEY_PAGE_DOWN if candidate_count > 0 => Action::PageDown,
            KEY_PAGE_DOWN => Action::Noop,
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
            // Any other key code is swallowed: composition state
            // stays untouched so the host's Marked Text is never interrupted
            // mid-composition.
            _ => Action::Noop,
        }
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
    use crate::{KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, KEY_SPACE};

    const KEY_A: u32 = 97;
    const KEY_D: u32 = 100;
    const KEY_1: u32 = 49;
    const KEY_2: u32 = 50;
    const KEY_5: u32 = 53;
    const KEY_6: u32 = 54;
    const KEY_7: u32 = 55;
    const KEY_9: u32 = 57;

    fn idle(key: u32) -> Action {
        Processor::decide(key, "", 0, Config::default())
    }

    fn composing(key: u32, code: &str, candidates: usize) -> Action {
        Processor::decide(key, code, candidates, Config::default())
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
    fn space_picks_the_first_candidate_or_commits_the_raw_code() {
        assert_eq!(composing(KEY_SPACE, "a", 3), Action::Select(0));
        assert_eq!(composing(KEY_SPACE, "q", 0), Action::CommitRaw);
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
    /// `-`/`=` are handled separately (see `page_keys_only_fire_with_candidates`).
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
    fn page_keys_only_fire_with_candidates() {
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
        assert_eq!(Processor::decide(KEY_PAGE_UP, "a", 1, cn), Action::PageUp);
        assert_eq!(
            Processor::decide(KEY_PAGE_UP, "q", 0, cn),
            Action::Noop,
            "no candidate: - emits nothing"
        );
        assert_eq!(
            Processor::decide(KEY_PAGE_DOWN, "q", 0, cn),
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
            Processor::decide(b',' as u32, "", 0, cn),
            Action::CommitPunct(b',')
        );
        assert_eq!(
            Processor::decide(b',' as u32, "a", 1, cn),
            Action::CommitCandidatePunct(b','),
            "mid-composition punctuation joins the candidate commit"
        );
        assert_eq!(
            Processor::decide(b'%' as u32, "", 0, cn),
            Action::Pass,
            "unmapped keys stay passed through"
        );
        assert_eq!(
            Processor::decide(b'%' as u32, "a", 1, cn),
            Action::Pass,
            "unmapped keys are released even mid-composition"
        );
        assert_eq!(
            Processor::decide(KEY_A, "", 0, cn),
            Action::AppendCode(b'a')
        );
        assert_eq!(Processor::decide(KEY_1, "a", 1, cn), Action::Select(0));
        assert_eq!(
            Processor::decide(KEY_BACKSPACE, "a", 1, cn),
            Action::DeleteCode
        );
        assert_eq!(Processor::decide(KEY_SPACE, "a", 1, cn), Action::Select(0));
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
        assert_eq!(Processor::decide(KEY_6, "a", 6, custom), Action::Select(5));
        assert_eq!(
            Processor::decide(KEY_7, "a", 6, custom),
            Action::Noop,
            "7 is beyond a 6-key page"
        );
        assert_eq!(
            Processor::decide(KEY_A, "ab", 1, custom),
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
            Processor::decide(KEY_D, "abcd", 3, overflow),
            Action::OverflowCommit(b'd'),
            "the 5th letter commits at N=5"
        );
        assert_eq!(
            Processor::decide(KEY_D, "abc", 3, overflow),
            Action::AppendCode(b'd'),
            "the 4th letter still extends the encoding"
        );
        assert_eq!(
            Processor::decide(KEY_D, "abcd", 0, overflow),
            Action::OverflowCommit(b'd'),
            "without a candidate the raw letters are what overflow commits"
        );

        let off = Config {
            max_code_len: 4,
            commit_next_code: 0,
            ..Config::default()
        };
        assert_eq!(
            Processor::decide(KEY_D, "abcd", 1, off),
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
            Processor::decide(KEY_D, "abcd", 1, unreachable),
            Action::Noop
        );

        // Reachable below the cap: overflow wins before the code grows on.
        let below_cap = Config {
            max_code_len: 6,
            commit_next_code: 4,
            ..Config::default()
        };
        assert_eq!(
            Processor::decide(KEY_D, "abc", 1, below_cap),
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
            Processor::decide(KEY_D, "abcd", 1, word),
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
            Processor::decide(KEY_D, "abcd", 1, sentence),
            Action::AppendCode(b'd'),
            "no overflow at the 5th letter in 形码整句"
        );
        let long = "a".repeat(64);
        assert_eq!(
            Processor::decide(KEY_D, &long, 1, sentence),
            Action::AppendCode(b'd'),
            "a long code keeps extending"
        );
    }

    /// The sentence code is capped at the decoder's own limit; letters past it
    /// are swallowed, matching how a word code treats letters over its cap.
    #[test]
    fn sentence_mode_caps_the_code_at_the_decoder_limit() {
        let sentence = Config { code_type: CodeType::XingmaSentence, ..Config::default() };
        let at_limit = "a".repeat(MAX_RAW_LENGTH - 1);
        assert_eq!(
            Processor::decide(KEY_A, &at_limit, 1, sentence),
            Action::AppendCode(b'a'),
            "the letter that reaches the limit is accepted"
        );
        let full = "a".repeat(MAX_RAW_LENGTH);
        assert_eq!(
            Processor::decide(KEY_A, &full, 1, sentence),
            Action::Noop,
            "letters past the limit are swallowed"
        );
    }

    /// Sentence mode changes only how letters accumulate: Space, digits, the
    /// editing keys and Enter keep their meanings, so the chosen sentence
    /// commits through the same actions the table path uses.
    #[test]
    fn sentence_mode_keeps_selection_and_editing_keys() {
        let sentence = Config { code_type: CodeType::XingmaSentence, ..Config::default() };
        assert_eq!(Processor::decide(KEY_SPACE, "jin", 3, sentence), Action::Select(0));
        assert_eq!(Processor::decide(KEY_SPACE, "q", 0, sentence), Action::CommitRaw);
        assert_eq!(Processor::decide(KEY_2, "jin", 3, sentence), Action::Select(1));
        assert_eq!(Processor::decide(KEY_BACKSPACE, "jin", 3, sentence), Action::DeleteCode);
        assert_eq!(Processor::decide(KEY_ESCAPE, "jin", 3, sentence), Action::Discard);
        assert_eq!(Processor::decide(KEY_ENTER, "jin", 3, sentence), Action::CommitRaw);
    }
}
