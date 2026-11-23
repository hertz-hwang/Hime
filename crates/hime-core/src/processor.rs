//! Key interpretation (SPEC_MVP §3).
//!
//! [`Processor`] is the single place that decides what a key *means*: append
//! to the encoding, delete from it, pick a candidate, abandon the composition,
//! do nothing, or hand the key back to the host application. It reads the
//! composition and never writes it — [`Session`](crate::session::Session)
//! applies the returned [`Action`] — so the whole key model is a pure table
//! that can be tested without any state.
//!
//! Paging (翻页) is not implemented yet: when it is, the page keys get their
//! own `Action` variants here and `Session` moves a page cursor instead of
//! touching the encoding.

use crate::config::Config;
use crate::{KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, KEY_SPACE};

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
    /// Commit the encoding letters as typed.
    CommitRaw,
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
    /// `config` supplies this session's code-length cap and page size.
    pub fn decide(key: u32, code: &str, candidate_count: usize, config: Config) -> Action {
        let letter = (ASCII_A..=ASCII_Z).contains(&key).then(|| key as u8);

        // Idle: only a letter starts a composition; everything else belongs to
        // the application.
        if code.is_empty() {
            return match letter {
                Some(byte) => Action::AppendCode(byte),
                None => Action::Pass,
            };
        }

        if let Some(byte) = letter {
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
            // Any other key code is swallowed the same way: composition state
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
        for key in [KEY_SPACE, KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, KEY_1, KEY_9, b'-' as u32, 0] {
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
        assert_eq!(composing(KEY_2, "ad", 1), Action::Noop, "no candidate behind 2");
        assert_eq!(composing(KEY_6, "a", 5), Action::Noop, "6 is beyond the page keys");
        assert_eq!(composing(KEY_9, "a", 5), Action::Noop, "9 is beyond the page keys");
        assert_eq!(composing(KEY_1, "a", 0), Action::Noop);
    }

    #[test]
    fn anything_else_is_swallowed_while_composing() {
        for key in [b'-' as u32, b'0' as u32, 0, 0x1000] {
            assert_eq!(composing(key, "a", 5), Action::Noop, "key {key}");
        }
    }

    /// The preferences window moves both limits, and the decision table
    /// follows the session's config rather than the compiled-in defaults.
    #[test]
    fn config_moves_the_page_and_code_limits() {
        let custom = Config { page_size: 6, max_code_len: 2 };
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
}
