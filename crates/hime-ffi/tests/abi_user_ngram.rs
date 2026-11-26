//! ABI tests for the learned user n-gram model (SPEC §6, §7.7):
//! `hime_user_ngram_status`, `hime_user_ngram_query`, `hime_user_ngram_edit`,
//! `hime_user_ngram_ensure_dir` and `hime_user_ngram_save`. Kept to a single
//! #[test] because the engine and the data folder are process globals.

use std::env;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;

use hime_ffi::{
    hime_free_string, hime_user_ngram_edit, hime_user_ngram_ensure_dir, hime_user_ngram_query,
    hime_user_ngram_save, hime_user_ngram_status,
};

fn take(ptr: *mut c_char, what: &str) -> String {
    assert!(!ptr.is_null(), "{what} returned null");
    let text = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    unsafe { hime_free_string(ptr) };
    text
}

fn take_json(ptr: *mut c_char, what: &str) -> serde_json::Value {
    let text = take(ptr, what);
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{what} returned invalid JSON: {e}"))
}

fn call(path: extern "C" fn() -> *mut c_char, what: &str) -> serde_json::Value {
    take_json(path(), what)
}

fn edit(json: &str) -> bool {
    let c = CString::new(json).unwrap();
    unsafe { hime_user_ngram_edit(c.as_ptr()) }
}

fn query(json: &str) -> serde_json::Value {
    let c = CString::new(json).unwrap();
    take_json(unsafe { hime_user_ngram_query(c.as_ptr()) }, "hime_user_ngram_query")
}

#[test]
fn user_model_status_management_round_trip() {
    // Pin the data folder to a scratch directory.
    let root = env::temp_dir().join(format!("hime-abi-user-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    env::set_var("HIME_CONFIG_HOME", &root);

    let expected: PathBuf = root.join("userNgram").join("user.hng");
    let status = call(hime_user_ngram_status, "hime_user_ngram_status");
    assert_eq!(status["active"], false, "no scheme selected, nothing learns");
    assert_eq!(status["path"].as_str(), Some(expected.to_string_lossy().as_ref()));
    assert_eq!(
        status["folder"].as_str(),
        Some(root.join("userNgram").to_string_lossy().as_ref())
    );
    assert_eq!(status["info"]["exists"], false, "the file appears on the first save");

    // The folder is created on demand and the model path comes back.
    assert_eq!(
        take(hime_user_ngram_ensure_dir(), "hime_user_ngram_ensure_dir"),
        expected.to_string_lossy()
    );
    assert!(root.join("userNgram").is_dir());
    assert!(!expected.exists(), "create never writes a model");

    // Add / query / set / delete through the C ABI.
    assert!(edit(r#"{"action":"add","scope":"char","text":"今","count":3}"#));
    assert!(edit(r#"{"action":"add","scope":"bigram","prev":"今","text":"天"}"#));
    assert!(expected.is_file(), "an edit saves immediately");
    let chars = query(r#"{"scope":"char"}"#);
    assert_eq!(chars["total"], 1, "{chars}");
    assert_eq!(chars["entries"][0]["text"], "今");
    assert_eq!(chars["entries"][0]["count"], 3);
    assert_eq!(chars["entries"][0]["total"], 3);

    let bigrams = query(r#"{"scope":"bigram","query":"今"}"#);
    assert_eq!(bigrams["total"], 1, "{bigrams}");
    assert_eq!(bigrams["entries"][0]["prev"], "今");
    assert_eq!(bigrams["entries"][0]["text"], "天");
    assert_eq!(bigrams["entries"][0]["count"], 1, "add without a count means 1");

    assert!(edit(r#"{"action":"set","scope":"bigram","prev":"今","text":"天","count":7}"#));
    let bigrams = query(r#"{"scope":"bigram"}"#);
    assert_eq!(bigrams["entries"][0]["count"], 7);
    assert_eq!(bigrams["entries"][0]["total"], 7);

    assert!(hime_user_ngram_save(), "an explicit save succeeds");
    assert!(edit(r#"{"action":"delete","scope":"bigram","prev":"今","text":"天"}"#));
    assert_eq!(query(r#"{"scope":"bigram"}"#)["total"], 0);

    // Status reports the saved file.
    let status = call(hime_user_ngram_status, "hime_user_ngram_status");
    assert_eq!(status["info"]["exists"], true);
    assert_eq!(status["info"]["unigrams"], 1);
    assert_eq!(status["info"]["format"], "HFNGMF v1");
    assert!(status["info"]["error"].is_null(), "{status}");

    // Clear wipes everything and leaves a readable file behind.
    assert!(edit(r#"{"action":"clear"}"#));
    let chars = query(r#"{"scope":"char"}"#);
    assert_eq!(chars["total"], 0);
    assert_eq!(chars["info"]["unigrams"], 0);
    let status = call(hime_user_ngram_status, "hime_user_ngram_status");
    assert_eq!(status["info"]["exists"], true, "the cleared model was saved");

    // Bad input is rejected without touching the model.
    assert!(!edit(r#"{"action":"add","scope":"char","text":"今天"}"#), "two characters");
    assert!(!edit("not json"));
    assert!(!edit(r#"{"action":"add","scope":"char","text":""}"#), "empty text");

    // A file the loader rejects is reported, and a clear unblocks it.
    std::fs::write(&expected, b"junk").unwrap();
    let status = call(hime_user_ngram_status, "hime_user_ngram_status");
    assert!(status["info"]["error"].is_string(), "{status}");
    assert!(!hime_user_ngram_save(), "saving over the user's bytes is refused");
    assert!(!edit(r#"{"action":"add","scope":"char","text":"今"}"#));
    assert!(edit(r#"{"action":"clear"}"#), "clear rebuilds the file");
    assert!(edit(r#"{"action":"add","scope":"char","text":"今"}"#));

    let _ = std::fs::remove_dir_all(&root);
}
