//! End-to-end smoke test through the C ABI, exactly as a macOS shell would
//! call it. Kept to a single #[test] because the FFI engine is global state.

use std::ffi::{CStr, CString};

use hime_ffi::{
    hime_create_session, hime_destroy_session, hime_free_string, hime_get_config, hime_init,
    hime_process_key, hime_set_config,
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

/// Call hime_get_config and parse the JSON it returns.
fn get_config() -> serde_json::Value {
    let ptr = hime_get_config();
    assert!(!ptr.is_null(), "hime_get_config returned null");
    let json = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { hime_free_string(ptr) };
    serde_json::from_str(&json).expect("config is valid JSON")
}

/// Call hime_set_config with a JSON object, returning the status.
fn set_config(json: &str) -> bool {
    let c = CString::new(json).unwrap();
    unsafe { hime_set_config(c.as_ptr()) }
}

#[test]
fn full_flow_through_c_abi() {
    // Bad init arguments are rejected without poisoning the engine.
    assert!(!unsafe { hime_init(std::ptr::null()) });
    let missing = CString::new("/nonexistent/hime-dict.tsv").unwrap();
    assert!(!unsafe { hime_init(missing.as_ptr()) });

    let dict = sample_dict_path();
    assert!(unsafe { hime_init(dict.as_ptr()) });

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

    // --- engine config ---
    assert_eq!(get_config()["page_size"], 5, "fresh engine reports the default config");
    assert_eq!(get_config()["max_code_len"], 4);
    assert!(!unsafe { hime_set_config(std::ptr::null()) });
    assert!(!set_config("not json"), "malformed JSON is rejected");

    let live = hime_create_session();
    assert_eq!(process(live, KEY_A)["candidates"].as_array().unwrap().len(), 5);

    // A live session adopts a smaller page immediately; digit keys follow.
    assert!(set_config(r#"{"page_size":2,"max_code_len":3}"#));
    let r = process(live, b'-' as u32); // swallowed; re-renders the page
    assert_eq!(r["candidates"].as_array().unwrap().len(), 2);
    let r = process(live, b'3' as u32); // beyond the 2-key page: swallowed
    assert!(r["commit_text"].is_null());
    assert_eq!(r["preedit"], "a");
    let r = process(live, b'2' as u32);
    assert_eq!(r["commit_text"], "戈", "digit 2 still selects within the smaller page");

    // The code-length cap follows the config: a 4th letter is swallowed.
    for key in [b'a', b'b', b'c'] {
        process(live, u32::from(key));
    }
    assert_eq!(process(live, b'd' as u32)["preedit"], "abc", "cap of 3 letters");
    hime_destroy_session(live);

    // Out-of-range values clamp instead of failing; defaults restore.
    assert!(set_config(r#"{"page_size":99,"max_code_len":0}"#));
    assert_eq!(get_config()["page_size"], 9);
    assert_eq!(get_config()["max_code_len"], 1);
    assert!(set_config(r#"{"page_size":5,"max_code_len":4}"#));
    assert_eq!(get_config()["page_size"], 5);
}
