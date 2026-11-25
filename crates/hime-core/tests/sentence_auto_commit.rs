//! 形码整句 auto-commit (SPEC §7.6): with `Config::auto_commit` on, the
//! probabilistic early commit emits matured sentence prefixes while the still
//! un-typed tail stays in the composition; with it off the whole code is held
//! for a manual Space. Runs only when the `.hng` model and the liuli sentence
//! table are present; skips clean without them.

use std::path::Path;
use std::sync::Arc;

use hime_core::ngram::find_model_path;
use hime_core::sentence::SentenceEngine;
use hime_core::{Config, Session, KEY_SPACE};

const TABLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/schemes/liuli_v1_sentence/liuli_table.txt"
);

/// The gold whole-sentence text for the 12-key code below (same coding the
/// `sentence_decode` test pins).
const GOLD: &str = "今天天气很好";
const CODE: &str = "gdakakdmtiol";

fn real_engine() -> Option<Arc<SentenceEngine>> {
    let model = find_model_path()?;
    if !Path::new(TABLE).is_file() {
        return None;
    }
    match SentenceEngine::load(&model, TABLE) {
        Ok(engine) => Some(Arc::new(engine)),
        Err(e) => {
            eprintln!("sentence auto-commit: skipped ({e})");
            None
        }
    }
}

/// Type `code` one letter per key, collecting every auto-commit. Asserts each
/// key is consumed (nothing leaks past the cap) and that an auto-commit is
/// never empty and always leaves a tail in the preedit. Returns the concatenated
/// committed text and the per-commit `(text, retained_preedit)` log.
fn type_collect(s: &mut Session, code: &str) -> (String, Vec<(String, String)>) {
    let mut concat = String::new();
    let mut commits = Vec::new();
    for byte in code.bytes() {
        let response = s.process_key(u32::from(byte));
        assert!(response.handled, "letter {} must be consumed", char::from(byte));
        if let Some(text) = response.commit_text {
            assert!(!text.is_empty(), "an auto-commit is never empty");
            assert!(
                !response.preedit.is_empty(),
                "an auto-commit keeps the unconsumed tail visible, not a bare commit"
            );
            concat.push_str(&text);
            commits.push((text, response.preedit));
        }
    }
    (concat, commits)
}

/// Auto-commit off: the entire code accumulates with no intermediate commit, so
/// the thin shell sees the same whole-sentence composition it always did and
/// Space commits it in one go.
#[test]
fn auto_commit_off_holds_the_whole_code() {
    let Some(engine) = real_engine() else { return };
    let mut s = Session::sentence(engine, Config { auto_commit: false, ..Config::default() });

    let (concat, commits) = type_collect(&mut s, CODE);
    assert!(commits.is_empty(), "auto-commit off must never emit mid-sentence");
    assert!(concat.is_empty());

    let r = s.process_key(KEY_SPACE);
    assert_eq!(r.commit_text.as_deref(), Some(GOLD), "Space commits the whole sentence");
    assert!(r.preedit.is_empty());
}

/// Auto-commit on: matured prefixes commit during typing; every committed piece
/// leads the gold sentence, a tail is always left for the user, and Space
/// finishes the sentence so the reconstructed text equals the gold exactly.
#[test]
fn auto_commit_on_emits_mature_prefixes_and_finishes_on_space() {
    let Some(engine) = real_engine() else { return };
    let mut s = Session::sentence(engine, Config { auto_commit: true, ..Config::default() });

    let (concat, commits) = type_collect(&mut s, CODE);
    assert!(!commits.is_empty(), "the probabilistic early commit must trigger on a real sentence");
    assert!(GOLD.starts_with(&concat), "committed {concat:?} must be a leading prefix of {GOLD:?}");
    assert!(
        concat.chars().count() < GOLD.chars().count(),
        "early commit always leaves the tail (retain) for a manual commit"
    );

    let r = s.process_key(KEY_SPACE);
    let tail = r.commit_text.unwrap_or_default();
    let reconstructed = format!("{concat}{tail}");
    assert_eq!(reconstructed, GOLD, "auto-committed prefixes plus the final commit rebuild the sentence");
    assert!(r.preedit.is_empty(), "the composition ends on Space");
}

/// The empty-code commit is the other auto-commit mechanism: it triggers when a
/// new letter makes the tail uncoverable. Its output, like the early commit,
/// always leaves a non-empty tail, which `type_collect` enforces on every
/// commit. This code has auto-commit off as the control that the same input is
/// otherwise stable.
#[test]
fn auto_commit_control_code_stays_silent_when_off() {
    let Some(engine) = real_engine() else { return };
    // 时间就是金钱 — a code whose early-commit pieces are exercised by the
    // probe; here it only confirms the off-switch is silent for any code.
    let mut s = Session::sentence(engine, Config { auto_commit: false, ..Config::default() });
    let (_, commits) = type_collect(&mut s, "wozwnsjeluat");
    assert!(commits.is_empty(), "auto-commit off is silent for every code");
}
