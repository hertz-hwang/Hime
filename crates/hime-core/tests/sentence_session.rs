//! End-to-end 形码整句 through the full state machine: a 形码整句 session decodes
//! a long encoding (well past the word-code cap) into whole-sentence
//! candidates, and Space / digits / paging / Escape commit and edit them exactly
//! like table candidates. This is the behavior the thin macOS shell relies on.
//!
//! The codes below are real rank-1 简码 encodings (the same word coding the
//! `sentence_decode` test verifies); each is listed with the sentence the
//! decoder is required to rank first. Runs only when the `.hng` model and the
//! liuli sentence table are present; skips clean without them.

use std::path::Path;
use std::sync::Arc;

use hime_core::ngram::find_model_path;
use hime_core::sentence::SentenceEngine;
use hime_core::{
    CodeType, Config, EngineResponse, Session, KEY_BACKSPACE, KEY_ESCAPE, KEY_PAGE_DOWN, KEY_SPACE,
};

const TABLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/schemes/liuli_v1_sentence/liuli_table.txt"
);

const KEY_2: u32 = 50; // b'2'

fn real_engine() -> Option<Arc<SentenceEngine>> {
    let model = find_model_path()?;
    if !Path::new(TABLE).is_file() {
        return None;
    }
    match SentenceEngine::load(&model, TABLE) {
        Ok(engine) => Some(Arc::new(engine)),
        Err(e) => {
            eprintln!("sentence session: skipped ({e})");
            None
        }
    }
}

/// Type `code` one lowercase letter per key, returning the final response (the
/// last key already renders the full-code candidate page). Asserts every key is
/// consumed with no auto-commit.
fn type_all(s: &mut Session, code: &str) -> EngineResponse {
    let mut last = s.process_key(KEY_BACKSPACE); // no-op on an empty buffer
    for byte in code.bytes() {
        last = s.process_key(u32::from(byte));
        assert!(last.handled, "letter {byte} must be consumed while composing");
        assert!(last.commit_text.is_none(), "no auto-commit mid-sentence: {:?}", last.commit_text);
    }
    last
}

/// A 形码整句 session runs in sentence mode.
#[test]
fn sentence_session_runs_in_sentence_mode() {
    let Some(engine) = real_engine() else {
        eprintln!("skip: no sentence model/table");
        return;
    };
    let s = Session::sentence(engine, Config::default());
    assert_eq!(
        s.config().code_type,
        CodeType::XingmaSentence,
        "the constructor forces sentence mode"
    );
}

/// A 12-key encoding accumulates whole (past the 4-letter word cap), decodes to
/// its gold sentence first, and Space commits that sentence and returns to idle.
#[test]
fn long_code_decodes_a_sentence_and_space_commits_it() {
    let Some(engine) = real_engine() else { return };
    let mut s = Session::sentence(Arc::clone(&engine), Config::default());

    let r = type_all(&mut s, "gdakakdmtiol");
    assert_eq!(
        r.preedit, "gdakakdmtiol",
        "the full 12-key code is held, not capped at the word-code limit"
    );
    assert_eq!(r.candidates[0].text, "今天天气很好", "gold sentence first");

    let over = s.process_key(b'x' as u32);
    assert_eq!(over.preedit, "gdakakdmtiolx", "a 13th key keeps extending the sentence");
    s.process_key(KEY_ESCAPE);

    type_all(&mut s, "gdakakdmtiol");
    let r = s.process_key(KEY_SPACE);
    assert_eq!(r.commit_text.as_deref(), Some("今天天气很好"), "Space commits the whole sentence");
    assert!(r.preedit.is_empty(), "the composition ends");
    assert!(!s.process_key(KEY_SPACE).handled, "idle again: Space belongs to the host");
}

/// A digit commits the sentence behind that key, proving the page holds real
/// sentences rather than the raw code.
#[test]
fn digit_selects_an_alternative_sentence() {
    let Some(engine) = real_engine() else { return };
    let mut s = Session::sentence(Arc::clone(&engine), Config::default());
    let page = type_all(&mut s, "gdakakdmtiol");
    assert!(page.candidates.len() >= 2, "expected alternative sentences to select from");
    let second = page.candidates[1].text.clone();

    let r = s.process_key(KEY_2);
    assert_eq!(r.commit_text.as_deref(), Some(second.as_str()), "2 commits the second sentence");
    assert!(r.preedit.is_empty());
}

/// Paging walks the sentence candidates without touching the encoding.
#[test]
fn sentence_candidates_page_without_touching_the_encoding() {
    let Some(engine) = real_engine() else { return };
    let mut s = Session::sentence(Arc::clone(&engine), Config::default());
    let first = type_all(&mut s, "gdakakdmtiol");

    let next = s.process_key(KEY_PAGE_DOWN);
    assert_eq!(next.preedit, "gdakakdmtiol", "paging leaves the encoding untouched");
    assert!(!next.candidates.is_empty());
    assert_ne!(next.candidates[0].text, first.candidates[0].text, "the next page differs");
}

/// A code that decodes to a single sentence leaves other digit keys swallowed;
/// Space still commits the one sentence.
#[test]
fn single_candidate_swallows_other_digits() {
    let Some(engine) = real_engine() else { return };
    let mut s = Session::sentence(Arc::clone(&engine), Config::default());
    // 时间就是金钱 decodes to exactly one candidate.
    type_all(&mut s, "wozwnsjeluat");

    let r = s.process_key(KEY_2);
    assert!(r.handled, "a digit with no candidate is swallowed");
    assert!(r.commit_text.is_none());
    assert_eq!(r.preedit, "wozwnsjeluat", "the composition survives");

    let r = s.process_key(KEY_SPACE);
    assert_eq!(r.commit_text.as_deref(), Some("时间就是金钱"));
}

/// Escape abandons the in-progress sentence without committing anything.
#[test]
fn escape_discards_the_sentence() {
    let Some(engine) = real_engine() else { return };
    let mut s = Session::sentence(Arc::clone(&engine), Config::default());
    type_all(&mut s, "gdakakdmtiol");

    let r = s.process_key(KEY_ESCAPE);
    assert!(r.handled);
    assert!(r.commit_text.is_none(), "escape commits nothing");
    assert!(r.preedit.is_empty());
    assert!(r.candidates.is_empty());
    assert_eq!(s.process_key(KEY_BACKSPACE).preedit, "", "still idle");
}
