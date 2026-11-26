//! End-to-end ABI test for scheme-driven user learning (SPEC §7.7): selecting
//! a 全拼 scheme with `user_ngram = true` makes its sessions learn, and the
//! recorded text shows up in `hime_user_ngram_status`. A single #[test]
//! because the engine and the data folder are process globals.

use std::env;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use hime_ffi::{
    hime_create_session, hime_free_string, hime_process_key, hime_select_scheme, hime_set_config,
    hime_user_ngram_status,
};

const KEY_SPACE: u32 = 32;

fn take_json(ptr: *mut c_char, what: &str) -> serde_json::Value {
    assert!(!ptr.is_null(), "{what} returned null");
    let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { hime_free_string(ptr) };
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{what} returned invalid JSON: {e}"))
}

fn status() -> serde_json::Value {
    take_json(hime_user_ngram_status(), "hime_user_ngram_status")
}

fn process(session: u64, key: u32) -> serde_json::Value {
    let ptr = hime_process_key(session, key, 0);
    assert!(!ptr.is_null(), "process_key({key}) returned null");
    let json = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { hime_free_string(ptr) };
    serde_json::from_str(&json).expect("response is valid JSON")
}

fn select(dir: &str) -> bool {
    let path = CString::new(dir).unwrap();
    unsafe { hime_select_scheme(path.as_ptr()) }
}

fn set_config(json: &str) -> bool {
    let c = CString::new(json).unwrap();
    unsafe { hime_set_config(c.as_ptr()) }
}

#[test]
fn a_learning_scheme_records_commits_through_the_abi() {
    let root = env::temp_dir().join(format!("hime-abi-learn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    env::set_var("HIME_CONFIG_HOME", &root);

    let schemes = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/schemes");
    // The shape scheme turns the user model off: selecting it clears `active`
    // and commits teach nothing.
    assert!(select(&format!("{schemes}/liuli_v1")));
    assert_eq!(status()["active"], false, "琉璃v1 ranks with the default model alone");

    // The 全拼 scheme enables it.
    assert!(select(&format!("{schemes}/pinyin")));
    let s = status();
    assert_eq!(s["active"], true, "{s}");
    assert_eq!(s["info"]["exists"], false, "nothing committed yet");
    assert!(set_config(
        r#"{"page_size":5,"code_type":0,"user_ngram":true,"ascii_punct":0}"#
    ));

    // Type `ni` and commit the focused candidate with Space.
    let session = hime_create_session();
    assert_ne!(session, 0);
    let mut candidates = Vec::new();
    for byte in "ni".bytes() {
        let r = process(session, u32::from(byte));
        candidates = r["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["text"].as_str().unwrap_or_default().to_owned())
            .collect();
    }
    assert!(!candidates.is_empty(), "ni must produce candidates");
    let r = process(session, KEY_SPACE);
    let committed = r["commit_text"].as_str().unwrap_or_default().to_owned();
    assert!(!committed.is_empty(), "{r}");

    let s = status();
    assert_eq!(s["active"], true);
    assert_eq!(s["info"]["unigrams"].as_u64().unwrap() > 0, true, "the commit was learned: {s}");
    assert!(s["info"]["info"].is_null() || s["info"]["error"].is_null(), "{s}");

    // The chosen scheme's `active` follows the scheme, not the file.
    assert!(select(&format!("{schemes}/liuli_v1")));
    assert_eq!(status()["active"], false, "a switch back turns learning off");
    let learned = status()["info"]["unigrams"].as_u64().unwrap();
    assert!(learned > 0, "the learned data survives the switch: {learned}");

    let _ = std::fs::remove_dir_all(&root);
}
