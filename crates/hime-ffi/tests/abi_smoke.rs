//! End-to-end smoke test through the C ABI, exactly as a macOS shell would
//! call it. Kept to a single #[test] because the FFI engine is global state.

use std::ffi::{CStr, CString};

use hime_ffi::{
    hime_create_session, hime_destroy_session, hime_free_string, hime_get_config, hime_init,
    hime_process_key, hime_scan_schemes, hime_select_scheme, hime_set_config,
};

const KEY_A: u32 = 97;
const KEY_BACKSPACE: u32 = 8;
const KEY_SPACE: u32 = 32;
const KEY_ARROW_LEFT: u32 = 0x2190;
const KEY_ARROW_UP: u32 = 0x2191;
const KEY_ARROW_RIGHT: u32 = 0x2192;
const KEY_ARROW_DOWN: u32 = 0x2193;

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

fn schemes_root() -> CString {
    CString::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/schemes")).unwrap()
}

/// Call hime_scan_schemes and parse the JSON array it returns.
fn scan_schemes(dir: &CStr) -> Vec<serde_json::Value> {
    let ptr = unsafe { hime_scan_schemes(dir.as_ptr()) };
    assert!(!ptr.is_null(), "hime_scan_schemes returned null");
    let json = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { hime_free_string(ptr) };
    serde_json::from_str(&json).expect("scheme list is valid JSON")
}

/// Call hime_select_scheme with a folder path, returning the status.
fn select_scheme(path: &str) -> bool {
    let c = CString::new(path).unwrap();
    unsafe { hime_select_scheme(c.as_ptr()) }
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
    assert_eq!(r["code_line"], "a");
    assert_eq!(r["code_in_window"], false);
    assert_eq!((r["page"].as_u64(), r["page_count"].as_u64()), (Some(1), Some(1)));
    assert_eq!(r["focus"], 0);
    assert_eq!(r["candidates"][0]["text"], "工");
    assert_eq!(r["candidates"][0]["comment"], "");
    assert_eq!(r["candidates"][0]["spans"], serde_json::json!([]));

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

    // Arrow focus: horizontal candidates move with Left/Right, Space commits
    // the focused entry, and the vertical pair is swallowed.
    let arrows = hime_create_session();
    let r = process(arrows, KEY_A);
    assert_eq!(r["focus"], 0, "the first candidate starts focused");
    assert_eq!(process(arrows, KEY_ARROW_RIGHT)["focus"], 1);
    assert_eq!(process(arrows, KEY_ARROW_LEFT)["focus"], 0);
    let r = process(arrows, KEY_ARROW_DOWN);
    assert_eq!((r["handled"].as_bool(), r["focus"].as_u64()), (Some(true), Some(0)));
    assert_eq!(process(arrows, KEY_ARROW_RIGHT)["focus"], 1);
    let r = process(arrows, KEY_SPACE);
    assert_eq!(r["commit_text"], "戈", "Space commits the focused candidate");
    hime_destroy_session(arrows);

    // A vertical candidate column swaps the axis: Up/Down move, Left/Right
    // stay inert.
    assert!(set_config(r#"{"page_size":5,"max_code_len":4,"vertical_candidates":true}"#));
    assert_eq!(get_config()["vertical_candidates"], true);
    let vertical = hime_create_session();
    process(vertical, KEY_A);
    assert_eq!(process(vertical, KEY_ARROW_UP)["focus"], 0);
    assert_eq!(process(vertical, KEY_ARROW_DOWN)["focus"], 1);
    assert_eq!(process(vertical, KEY_ARROW_RIGHT)["focus"], 1);
    let r = process(vertical, KEY_SPACE);
    assert_eq!(r["commit_text"], "戈");
    hime_destroy_session(vertical);
    assert!(set_config(r#"{"page_size":5,"max_code_len":4,"vertical_candidates":false}"#));
    assert_eq!(get_config()["vertical_candidates"], false);

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
    for key in *b"abc" {
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

    // ascii_punct is a 0/1 flag; other numbers clamp to 中文标点.
    assert_eq!(get_config()["ascii_punct"], 0, "English punctuation by default");
    assert!(set_config(r#"{"ascii_punct":9}"#));
    assert_eq!(get_config()["ascii_punct"], 1, "unknown numbers clamp to the flag's top");
    assert!(set_config(r#"{"ascii_punct":0}"#));
    assert_eq!(get_config()["ascii_punct"], 0);

    // The two punctuation preference switches are plain bools, off by
    // default, and round-trip through JSON like every other config key.
    assert_eq!(get_config()["english_punct_after_alnum"], false);
    assert_eq!(get_config()["semicolon_quote_select"], false);
    assert_eq!(get_config()["show_code_in_window"], false, "the encoding is inline by default");
    assert_eq!(get_config()["vertical_candidates"], false, "candidates are horizontal by default");
    let both_on = r#"{"page_size":5,"max_code_len":4,"english_punct_after_alnum":true,"semicolon_quote_select":true,"show_code_in_window":true}"#;
    assert!(set_config(both_on));
    assert_eq!(get_config()["english_punct_after_alnum"], true);
    assert_eq!(get_config()["semicolon_quote_select"], true);
    assert_eq!(get_config()["show_code_in_window"], true);
    // A live session reports the placement on the next response.
    let live = hime_create_session();
    let r = process(live, KEY_A);
    assert_eq!(r["code_in_window"], true);
    assert_eq!(r["code_line"], "a");
    hime_destroy_session(live);
    let both_off = r#"{"page_size":5,"max_code_len":4,"english_punct_after_alnum":false,"semicolon_quote_select":false,"show_code_in_window":false}"#;
    assert!(set_config(both_off));
    assert_eq!(get_config()["english_punct_after_alnum"], false);
    assert_eq!(get_config()["semicolon_quote_select"], false);
    assert_eq!(get_config()["show_code_in_window"], false);

    // --- schemes: discovery (hime_scan_schemes) and selection ---

    let schemes = scan_schemes(&schemes_root());
    let liuli = schemes
        .iter()
        .find(|s| s["dir"] == "liuli_v1")
        .expect("the shipped scheme is discovered");
    assert_eq!(liuli["name"], "琉璃字词");
    assert_eq!(liuli["type"], 2);
    assert_eq!(liuli["max_code"], 4);
    assert_eq!(liuli["auto_commit"], false);
    assert_eq!(liuli["auto_commit_length"], 4);
    assert_eq!(liuli["commit_next_code"], 5);
    assert_eq!(liuli["ascii_punct"], 1, "琉璃v1 ships with Chinese punctuation");

    let unreadable = CString::new("/nonexistent/schemes-dir").unwrap();
    assert!(scan_schemes(&unreadable).is_empty(), "an unreadable dir lists nothing");

    // Switching while a live session is composing keeps the id, drops the
    // composition, and replaces the config with the scheme defaults.
    assert!(set_config(
        r#"{"page_size":5,"max_code_len":4,"show_code_in_window":true,"vertical_candidates":true}"#
    ));
    let live = hime_create_session();
    process(live, KEY_A);
    assert!(select_scheme(liuli["path"].as_str().unwrap()));

    let cfg = get_config();
    assert_eq!(cfg["page_size"], 5, "page size survives a scheme switch");
    assert_eq!(
        cfg["show_code_in_window"], true,
        "the preedit placement survives a scheme switch"
    );
    assert_eq!(
        cfg["vertical_candidates"], true,
        "the candidate layout survives a scheme switch"
    );
    assert_eq!(cfg["max_code_len"], 4);
    assert_eq!(cfg["code_type"], 2, "metadata type 2 = 形码");
    assert_eq!(cfg["auto_commit_length"], 4);
    assert_eq!(cfg["commit_next_code"], 5, "the metadata overflow rule is live");
    assert_eq!(cfg["ascii_punct"], 1, "the metadata punctuation default replaces the user value");

    // The session is idle (composition dropped) and answers from the 琉璃 table.
    let r = process(live, KEY_SPACE);
    assert_eq!(r["handled"], false, "the switch cleared the composition");
    let r = process(live, b'd' as u32);
    assert_eq!(r["candidates"][0]["text"], "的");

    // The scheme's ascii_punct = 1 is live: the comma commits the candidate
    // with the full-width form, and stays in the engine.
    let r = process(live, b',' as u32);
    assert_eq!(r["handled"], true);
    assert_eq!(r["commit_text"], "的，");
    assert_eq!(r["preedit"], "");
    let r = process(live, b',' as u32);
    assert_eq!(r["commit_text"], "，", "idle punctuation commits on its own");
    // An unmapped symbol still reaches the host application.
    let r = process(live, b'%' as u32);
    assert_eq!(r["handled"], false, "unmapped punctuation is released");

    // english_punct_after_alnum: the first punctuation after a digit reaches
    // the host as ASCII, and pressing that same key again answers a
    // correction the shell applies in place of it (replace_text = ":").
    // hime_set_config replaces the whole config, so toggle the one key on
    // top of the values the scheme gave the engine.
    let mut cfg = get_config();
    cfg["english_punct_after_alnum"] = serde_json::json!(true);
    assert!(set_config(&cfg.to_string()));
    let r = process(live, b'1' as u32);
    assert_eq!(r["handled"], false);
    let r = process(live, b':' as u32);
    assert_eq!(r["handled"], false, "the first colon stays half-width");
    assert!(r["replace_text"].is_null());
    let r = process(live, b':' as u32);
    assert_eq!(r["handled"], true, "the second press is the engine's");
    assert_eq!(r["commit_text"], "：");
    assert_eq!(r["replace_text"], ":", "it overwrites the ASCII colon on screen");
    let r = process(live, b':' as u32);
    assert_eq!(r["commit_text"], "：");
    assert!(r["replace_text"].is_null(), "the third press appends");
    cfg["english_punct_after_alnum"] = serde_json::json!(false);
    assert!(set_config(&cfg.to_string()));
    hime_destroy_session(live);

    // A folder that is not a scheme leaves the engine untouched.
    assert!(!select_scheme("/nonexistent/scheme-folder"));
    assert_eq!(get_config()["commit_next_code"], 5);

    // --- 全拼 (SPEC §8.2): a type-0 scheme serves pinyin words ---

    let pinyin = schemes
        .iter()
        .find(|s| s["dir"] == "pinyin")
        .expect("the shipped 全拼 scheme is discovered");
    assert_eq!(pinyin["name"], "全拼");
    assert_eq!(pinyin["type"], 0);
    assert!(pinyin["table"].as_str().unwrap().ends_with("py.hdict"), "{pinyin}");
    assert!(select_scheme(pinyin["path"].as_str().unwrap()));

    let cfg = get_config();
    assert_eq!(cfg["code_type"], 0, "metadata type 0 = 全拼");
    assert_eq!(cfg["ascii_punct"], 1, "the pinyin scheme types Chinese punctuation");

    let pinyin_session = hime_create_session();
    let mut r = process(pinyin_session, b'n' as u32);
    assert_eq!(r["preedit"], "n");
    assert!(!r["candidates"].as_array().unwrap().is_empty());
    for key in *b"ihao" {
        r = process(pinyin_session, u32::from(key));
    }
    assert_eq!(r["preedit"], "nihao", "a pinyin code outgrows the 形码 word cap");
    assert_eq!(r["code_line"], "ni hao", "the top candidate supplies the split");
    assert_eq!(r["candidates"][0]["text"], "你好");
    assert_eq!(r["candidates"][0]["code"], "ni hao");
    assert_eq!(r["candidates"][0]["comment"], "", "pinyin candidates carry no ~comment");
    let r = process(pinyin_session, KEY_SPACE);
    assert_eq!(r["commit_text"], "你好");
    assert_eq!(r["preedit"], "");
    hime_destroy_session(pinyin_session);

    // 全拼 owns no reverse lookup: the backtick stays the mapped punctuation
    // key and never opens a query.
    let pinyin_session = hime_create_session();
    let r = process(pinyin_session, 96);
    assert_eq!(r["handled"], true);
    assert_eq!(r["commit_text"], "`");
    assert_eq!(r["preedit"], "");
    hime_destroy_session(pinyin_session);

    // --- 反查 (SPEC §8.3): backtick + full pinyin lists scheme codes ---

    assert!(select_scheme(liuli["path"].as_str().unwrap()));
    let reverse = hime_create_session();
    let r = process(reverse, 96);
    assert_eq!(r["handled"], true);
    assert!(r["commit_text"].is_null());
    assert_eq!(r["preedit"], "", "the marker is session state, not encoding text");
    assert_eq!(r["code_line"], "`", "the display line keeps the marker");

    let mut r = r;
    for key in *b"nihao" {
        r = process(reverse, u32::from(key));
    }
    assert_eq!(r["preedit"], "nihao");
    assert_eq!(r["code_line"], "`ni hao");
    assert_eq!(r["candidates"][0]["text"], "你好");
    assert_eq!(r["candidates"][0]["comment"], "flol", "琉璃 hands over the full code");
    assert_eq!(r["candidates"][0]["code"], "ni hao");
    let r = process(reverse, KEY_SPACE);
    assert_eq!(r["commit_text"], "你好");
    hime_destroy_session(reverse);

    // Backtick then Space commits the literal backtick, so the key stays
    // typeable at all.
    let reverse = hime_create_session();
    process(reverse, 96);
    let r = process(reverse, KEY_SPACE);
    assert_eq!(r["commit_text"], "`");
    assert_eq!(r["preedit"], "");
    hime_destroy_session(reverse);

    // Esc closes the lookup without committing anything.
    let reverse = hime_create_session();
    process(reverse, 96);
    process(reverse, b'n' as u32);
    let r = process(reverse, 27);
    assert!(r["commit_text"].is_null());
    assert_eq!(r["preedit"], "");
    hime_destroy_session(reverse);
}
