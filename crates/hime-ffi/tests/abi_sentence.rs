//! Whole-sentence decoding through the C ABI: selecting the shipped 形码整句
//! scheme (`type = 3`) switches the engine to sentence mode, and a long key
//! stream past the word-code cap decodes to whole-sentence candidates that
//! Space commits — exactly what a thin shell renders.
//!
//! This runs in its own process, so its global FFI engine is independent of the
//! `abi_smoke` test. It needs the `.hng` model and the liuli sentence table;
//! without them it skips clean.

use std::ffi::{CStr, CString};

use hime_core::ngram::find_model_path;
use hime_ffi::{
    hime_create_session, hime_free_string, hime_get_config, hime_process_key, hime_select_scheme,
};

const KEY_SPACE: u32 = 32;

fn sentence_scheme_dir() -> CString {
    CString::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/schemes/liuli_v1_sentence"
    ))
    .unwrap()
}

fn process(session: u64, key: u32) -> serde_json::Value {
    let ptr = hime_process_key(session, key, 0);
    assert!(!ptr.is_null(), "process_key({key}) returned null");
    let json = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { hime_free_string(ptr) };
    serde_json::from_str(&json).expect("response is valid JSON")
}

fn get_config() -> serde_json::Value {
    let ptr = hime_get_config();
    assert!(!ptr.is_null());
    let json = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { hime_free_string(ptr) };
    serde_json::from_str(&json).unwrap()
}

#[test]
fn sentence_scheme_decodes_whole_sentences_through_the_abi() {
    let Some(model) = find_model_path() else {
        eprintln!("skip: no .hng model (set HIME_SENTENCE_MODEL or add data/sentence-ngram.hng)");
        return;
    };

    // Selecting a scheme flushes typing statistics (SPEC §9); keep them in a
    // scratch folder instead of the real user data. The model was resolved
    // before the data folder moved, so pin it explicitly.
    let root = std::env::temp_dir().join(format!("hime-abi-sentence-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::env::set_var("HIME_CONFIG_HOME", &root);
    std::env::set_var("HIME_SENTENCE_MODEL", &model);

    let dir = sentence_scheme_dir();
    assert!(
        unsafe { hime_select_scheme(dir.as_ptr()) },
        "selecting the shipped 形码整句 scheme must succeed when the model is present"
    );
    assert_eq!(get_config()["code_type"], 3, "metadata type 3 = 形码整句");
    assert_eq!(get_config()["auto_commit"], false);

    let session = hime_create_session();
    assert_ne!(session, 0);

    // A 12-key encoding (far past the 4-letter word cap) accumulates whole.
    let mut last = serde_json::Value::Null;
    for &byte in b"gdakakdmtiol" {
        last = process(session, u32::from(byte));
        assert_eq!(last["handled"], true);
    }
    assert_eq!(last["preedit"], "gdakakdmtiol", "the long code is not capped");
    assert_eq!(
        last["code_line"], "gd ak ak dm ti ol",
        "the display line is the top candidate's automatic code split"
    );
    assert_eq!(last["candidates"][0]["text"], "今天天气很好", "gold sentence first");
    assert_eq!(last["candidates"][0]["code"], "gd ak ak dm ti ol");
    assert_eq!((last["page"].as_u64(), last["page_count"].as_u64()), (Some(1), Some(2)));
    let spans = last["candidates"][1]["spans"].as_array().expect("alternative carries spans");
    assert!(
        spans.iter().any(|span| span["kind"] != "equal"),
        "the alternative differs from the top candidate: {spans:?}"
    );

    let r = process(session, KEY_SPACE);
    assert_eq!(r["commit_text"], "今天天气很好", "Space commits the whole sentence");
    assert_eq!(r["preedit"], "");
}
