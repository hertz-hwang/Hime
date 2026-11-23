//! Session state machine (SPEC_MVP §3).

use std::mem;
use std::sync::Arc;

use crate::dictionary::Dictionary;
use crate::types::{Candidate, EngineResponse};
use crate::{
    MAX_CANDIDATES, MAX_CODE_LEN, KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, KEY_SPACE,
};

// Printable-key ASCII codes; digits `1`..=`5` select candidates 0..=4.
const ASCII_A: u32 = 97;
const ASCII_Z: u32 = 122;
const ASCII_1: u32 = 49;
const ASCII_5: u32 = 53;

/// One typing session. All typing state lives here; shells just render the
/// [`EngineResponse`] they get back.
#[derive(Debug)]
pub struct Session {
    dict: Arc<Dictionary>,
    input_buffer: String,
    candidates: Vec<Candidate>,
}

impl Session {
    pub fn new(dict: Arc<Dictionary>) -> Self {
        Self { dict, input_buffer: String::new(), candidates: Vec::new() }
    }

    /// Feed one key (see the `KEY_*` constants and ASCII codes in
    /// `hime_core`) and return the full state the shell must render.
    pub fn process_key(&mut self, key: u32) -> EngineResponse {
        // Idle: only a letter starts a composition; every other key passes
        // through untouched.
        if self.input_buffer.is_empty() {
            if is_letter(key) {
                self.push_letter(key);
                return self.response(true, None);
            }
            return self.response(false, None);
        }

        match key {
            KEY_BACKSPACE => {
                self.input_buffer.pop();
                self.refresh_candidates();
                self.response(true, None)
            }
            KEY_ESCAPE => {
                self.clear();
                self.response(true, None)
            }
            KEY_SPACE => {
                // First candidate wins; with nothing selectable, commit the
                // raw encoding instead.
                let commit = match self.candidates.first() {
                    Some(c) => c.text.clone(),
                    None => mem::take(&mut self.input_buffer),
                };
                self.clear();
                self.response(true, Some(commit))
            }
            KEY_ENTER => {
                let commit = mem::take(&mut self.input_buffer);
                self.clear();
                self.response(true, Some(commit))
            }
            k if is_letter(k) && self.input_buffer.len() < MAX_CODE_LEN => {
                self.push_letter(k);
                self.response(true, None)
            }
            ASCII_1..=ASCII_5 => {
                let index = (key - ASCII_1) as usize;
                match self.candidates.get(index) {
                    Some(c) => {
                        let commit = c.text.clone();
                        self.clear();
                        self.response(true, Some(commit))
                    }
                    // Digit with no candidate behind it: pass through, keep
                    // the composition on screen.
                    None => self.response(false, None),
                }
            }
            // Includes letters once the encoding is full: never swallow them.
            _ => self.response(false, None),
        }
    }

    fn push_letter(&mut self, key: u32) {
        self.input_buffer.push(char::from(key as u8));
        self.refresh_candidates();
    }

    fn refresh_candidates(&mut self) {
        self.candidates = if self.input_buffer.is_empty() {
            Vec::new()
        } else {
            self.dict.lookup(&self.input_buffer, MAX_CANDIDATES)
        };
    }

    fn clear(&mut self) {
        self.input_buffer.clear();
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

fn is_letter(key: u32) -> bool {
    (ASCII_A..=ASCII_Z).contains(&key)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "a\t工\t100\n\
                          a\t戈\t90\n\
                          aa\t式\t100\n\
                          ad\t匿\t100\n\
                          g\t王\t100\n\
                          gg\t五\t100\n";

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
        for key in [KEY_SPACE, KEY_BACKSPACE, KEY_ENTER, KEY_ESCAPE, KEY_1, KEY_9] {
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
    fn digit_beyond_candidate_list_passes_through() {
        let mut s = session();
        s.process_key(KEY_A);
        s.process_key(b'd' as u32); // "ad" has exactly one candidate: 匿
        let r = s.process_key(KEY_2);
        assert!(!r.handled);
        assert!(r.commit_text.is_none());
        assert_eq!(r.preedit, "ad");
        assert_eq!(r.candidates.len(), 1);
        // '9' can never map to a candidate (list is capped at 5).
        let r = s.process_key(KEY_9);
        assert!(!r.handled);
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

    #[test]
    fn encoding_capped_at_max_code_len() {
        let mut s = session();
        for _ in 0..MAX_CODE_LEN {
            let r = s.process_key(KEY_G);
            assert!(r.handled);
        }
        assert_eq!(s.input_buffer, "gggg");
        let r = s.process_key(KEY_G);
        assert!(!r.handled, "a letter over the cap must pass through");
        assert_eq!(r.preedit, "gggg");
        assert_eq!(s.input_buffer.len(), MAX_CODE_LEN);
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
}
