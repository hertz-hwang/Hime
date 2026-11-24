//! ABI tests for the data-folder and n-gram-model functions (SPEC §6, §7.5):
//! `hime_schemes_dir`, `hime_ensure_data_dir`, `hime_set_model_path` and
//! `hime_model_status`. Kept to a single #[test] because the model override
//! and the engine are process globals shared by every test in this binary.

use std::env;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use hime_ffi::{
    hime_ensure_data_dir, hime_free_string, hime_model_status, hime_scan_schemes, hime_schemes_dir,
    hime_select_scheme, hime_set_model_path,
};

/// Take ownership of a returned JSON string, free it, parse it.
fn take_json(ptr: *mut c_char, what: &str) -> serde_json::Value {
    assert!(!ptr.is_null(), "{what} returned null");
    let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { hime_free_string(ptr) };
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{what} returned invalid JSON: {e}"))
}

fn model_status() -> serde_json::Value {
    take_json(hime_model_status(), "hime_model_status")
}

fn set_model_path(path: Option<&str>) -> bool {
    match path {
        None => unsafe { hime_set_model_path(std::ptr::null()) },
        Some(p) => {
            let c = CString::new(p).unwrap();
            unsafe { hime_set_model_path(c.as_ptr()) }
        }
    }
}

#[test]
fn model_status_override_and_data_dir() {
    // Pin the data folder to a scratch dir so the test never touches ~/.config.
    let root = env::temp_dir().join(format!("hime-abi-model-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    env::set_var("HIME_CONFIG_HOME", &root);

    // Data folder plumbing (SPEC §5.2). hime_schemes_dir is a plain path.
    let ptr = hime_schemes_dir();
    assert!(!ptr.is_null(), "hime_schemes_dir returned null");
    let dir = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    unsafe { hime_free_string(ptr) };
    assert_eq!(dir, root.join("schemes").to_string_lossy());
    assert!(hime_ensure_data_dir());
    assert!(root.join("schemes").is_dir(), "ensure_data_dir creates the schemes folder");

    // No engine yet: offline, but the document shape is complete.
    let status = model_status();
    assert_eq!(status["online"], false, "no engine, no online model");
    assert!(status["override_path"].is_null());
    assert_eq!(
        status["default_path"].as_str(),
        Some(root.join("sentence-ngram.hng").to_string_lossy().as_ref())
    );

    // An override is authoritative and its load failure is reported.
    let junk = root.join("junk.hng");
    std::fs::write(&junk, b"not a model").unwrap();
    assert!(set_model_path(Some(junk.to_str().unwrap())));
    let status = model_status();
    assert_eq!(status["override_path"].as_str(), Some(junk.to_str().unwrap()));
    assert_eq!(
        status["path"].as_str(),
        Some(junk.to_str().unwrap()),
        "the override wins over automatic discovery"
    );
    assert!(status["error"].as_str().unwrap().contains("HFNGMF"));
    assert!(status["info"].is_null());
    assert!(status["modified_secs"].is_i64());

    // A missing file is rejected and the override survives the bad call.
    assert!(!set_model_path(Some(root.join("absent.hng").to_str().unwrap())));
    assert_eq!(model_status()["override_path"].as_str(), Some(junk.to_str().unwrap()));

    // Clearing (null) restores automatic discovery.
    assert!(set_model_path(None));
    assert!(model_status()["override_path"].is_null());

    // With a real model, a 形码整句 scheme comes up online through the ABI.
    let Some(model) = hime_core::ngram::find_model_path() else {
        eprintln!("abi model: skipped the online leg (no .hng model present)");
        std::fs::remove_dir_all(&root).unwrap();
        return;
    };
    assert!(set_model_path(Some(model.to_str().unwrap())));
    let schemes_dir = CString::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/schemes")).unwrap();
    let schemes = take_json(unsafe { hime_scan_schemes(schemes_dir.as_ptr()) }, "hime_scan_schemes");
    let sentence = schemes
        .as_array()
        .expect("scheme list is an array")
        .iter()
        .find(|s| s["type"] == 3)
        .expect("the shipped 形码整句 scheme is listed");
    let sentence_path = CString::new(sentence["path"].as_str().unwrap()).unwrap();
    assert!(unsafe { hime_select_scheme(sentence_path.as_ptr()) });

    let status = model_status();
    assert_eq!(status["online"], true, "a selected 形码整句 scheme decodes with the model");
    // The online leg reports the engine's loaded file; the strings come from
    // the same display path as the override, so compare them to each other.
    assert_eq!(status["path"], status["override_path"]);
    assert!(status["error"].is_null());
    let info = &status["info"];
    assert_eq!(info["format"], "HFNGMF v1");
    assert_eq!(info["order"], 3);
    assert!(info["vocab"].as_u64().unwrap() > 0);
    assert!(!info["build_id"].as_str().unwrap().is_empty());

    assert!(set_model_path(None));
    let _ = std::fs::remove_dir_all(&root);
}
