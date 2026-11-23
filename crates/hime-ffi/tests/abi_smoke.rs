//! End-to-end smoke test through the C ABI, exactly as a macOS shell would
//! call it. Kept to a single #[test] because the FFI engine is global state.

use std::ffi::{CStr, CString};

use hime_ffi::{
    hime_create_session, hime_destroy_session, hime_free_string, hime_init, hime_process_key,
};

const KEY_A: u32 = 97;
const KEY_BACKSPACE: u32 = 8;
const KEY_SPACE: u32 = 32;

fn sample_dict_path() -> CString {
    CString::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/xingma_sample.tsv"
    ))
    .unwrap()
}

/// Call hime_process_key, take ownership of the returned JSON, free it.
fn process(session: u64, key: u32) -> serde_json::Value {
    let ptr = hime_process_key(session, key, 0);
    assert!(!ptr.is_null(), "process_key({key}) returned null");
    let json = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { hime_free_string(ptr) };
    serde_json::from_str(&json).expect("response is valid JSON")
}

#[test]
fn full_flow_through_c_abi() {
    // Bad init arguments are rejected without poisoning the engine.
    assert!(!hime_init(std::ptr::null()));
    let missing = CString::new("/nonexistent/hime-dict.tsv").unwrap();
    assert!(!hime_init(missing.as_ptr()));

    let dict = sample_dict_path();
    assert!(hime_init(dict.as_ptr()));

    // Session 0 / unknown ids never resolve.
    let orphan = hime_process_key(0, KEY_A, 0);
    assert!(orphan.is_null());

    let session = hime_create_session();
    assert_ne!(session, 0);

    // a -> BS -> a -> Space commits "工", identical to the core tests.
    let r = process(session, KEY_A);
    assert_eq!(r["handled"], true);
    assert_eq!(r["preedit"], "a");
    assert_eq!(r["candidates"][0]["text"], "工");
    assert_eq!(r["candidates"][0]["comment"], "");

    let r = process(session, KEY_BACKSPACE);
    assert_eq!(r["handled"], true);
    assert_eq!(r["preedit"], "");
    assert_eq!(r["candidates"].as_array().unwrap().len(), 0);

    let r = process(session, KEY_A);
    assert_eq!(r["candidates"][0]["text"], "工");

    let r = process(session, KEY_SPACE);
    assert_eq!(r["handled"], true);
    assert_eq!(r["commit_text"], "工");
    assert_eq!(r["preedit"], "");

    // Destroying makes the id unknown again.
    hime_destroy_session(session);
    assert!(hime_process_key(session, KEY_A, 0).is_null());

    // Sessions stay independent: two live sessions, distinct buffers.
    let s1 = hime_create_session();
    let s2 = hime_create_session();
    assert_ne!(s1, s2);
    process(s1, KEY_A);
    let r = process(s2, KEY_BACKSPACE);
    assert_eq!(r["handled"], false, "idle session passes Backspace through");
    hime_destroy_session(s1);
    hime_destroy_session(s2);
}
