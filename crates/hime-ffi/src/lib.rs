//! C-ABI boundary for the Hime engine (SPEC §6).
//!
//! Only flat C functions cross this boundary; every structured value is
//! serialized to a JSON string first. The global engine owns the dictionary
//! and all sessions (`Mutex<Option<Engine>>`), so any thread holding the
//! `hime.h` contract may call in. Every export catches panics so Rust
//! unwinding never runs through C frames.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};

use hime_core::ngram::{find_model_path, probe_model, set_model_override};
use hime_core::sentence::SentenceEngine;
use hime_core::{
    ensure_config_layout, load_scheme, scan_schemes, schemes_dir, CodeType, Config, Dictionary,
    Session,
};

/// Where candidates come from: a word table, or the whole-sentence engine a
/// 形码整句 (type 3) scheme needs. This decides how every session is built.
enum EngineSource {
    Table(Arc<Dictionary>),
    Sentence(Arc<SentenceEngine>),
}

/// Global engine state, present only after a successful `hime_init`.
struct Engine {
    source: EngineSource,
    config: Config,
    sessions: HashMap<u64, Session>,
    next_session_id: u64,
    /// The `.hng` path a 形码整句 source loaded from (SPEC §7.5); `None`
    /// for a word table. Reported by `hime_model_status` so the panel can
    /// show the file the engine actually uses.
    model_path: Option<PathBuf>,
}

impl Engine {
    fn new(source: EngineSource) -> Self {
        Self {
            source,
            config: Config::default(),
            sessions: HashMap::new(),
            next_session_id: 1,
            model_path: None,
        }
    }

    /// Build a fresh session for the current source and config. A 形码整句
    /// source builds a sentence session (long codes decode whole sentences);
    /// a table source builds the word-lookup session.
    fn build_session(&self) -> Session {
        match &self.source {
            EngineSource::Table(dict) => Session::with_config(Arc::clone(dict), self.config),
            EngineSource::Sentence(engine) => Session::sentence(Arc::clone(engine), self.config),
        }
    }

    /// Rebuild every live session in place (ids kept) for the current source and
    /// config, dropping in-progress compositions. Used when a scheme switches
    /// the candidate source, including between word and whole-sentence modes,
    /// which a session cannot change by mutation.
    fn rebuild_sessions(&mut self) {
        let ids: Vec<u64> = self.sessions.keys().copied().collect();
        for id in ids {
            let session = self.build_session();
            self.sessions.insert(id, session);
        }
    }
}

static ENGINE: OnceLock<Mutex<Option<Engine>>> = OnceLock::new();

fn engine_mutex() -> &'static Mutex<Option<Engine>> {
    ENGINE.get_or_init(|| Mutex::new(None))
}

/// Run `f` against the global engine slot. A poisoned lock is recovered (no
/// invariant depends on it), and panics are caught at this boundary.
fn with_engine<T: Default>(what: &str, f: impl FnOnce(&mut Option<Engine>) -> T) -> T {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let mut guard = engine_mutex()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut guard)
    }));
    match outcome {
        Ok(value) => value,
        Err(_) => {
            eprintln!("hime-ffi: panic while executing {what}");
            T::default()
        }
    }
}

/// Load the lexicon and (re)initialize the global engine.
///
/// `dict_path` is a NUL-terminated path to a lexicon file: a scheme table
/// (`text<TAB>code`, the default — e.g. 琉璃v1) or a weighted TSV
/// (`code<TAB>text<TAB>weight`). Returns `false` on a null/invalid path, an
/// unreadable file, or a parse error. Calling this again re-initializes the
/// engine and drops all live sessions.
///
/// # Safety
/// `dict_path` must be null or point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn hime_init(dict_path: *const c_char) -> bool {
    if dict_path.is_null() {
        eprintln!("hime-ffi: hime_init called with a null dict_path");
        return false;
    }
    let path = match unsafe { CStr::from_ptr(dict_path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            eprintln!("hime-ffi: dict_path is not valid UTF-8");
            return false;
        }
    };
    match Dictionary::load(path) {
        Ok(dict) => {
            let dict = Arc::new(dict);
            with_engine("hime_init", |slot| {
                *slot = Some(Engine::new(EngineSource::Table(dict)));
            });
            true
        }
        Err(e) => {
            eprintln!("hime-ffi: failed to load dictionary {path:?}: {e}");
            false
        }
    }
}

/// Create a session. Returns the session id, or `0` when no engine is
/// initialized (session ids from [`hime_create_session`] always start at 1).
#[no_mangle]
pub extern "C" fn hime_create_session() -> u64 {
    with_engine("hime_create_session", |slot| match slot.as_mut() {
        None => {
            eprintln!("hime-ffi: hime_create_session before hime_init");
            0
        }
        Some(engine) => {
            let id = engine.next_session_id;
            engine.next_session_id += 1;
            let session = engine.build_session();
            engine.sessions.insert(id, session);
            id
        }
    })
}

/// Destroy a session. Unknown ids are ignored.
#[no_mangle]
pub extern "C" fn hime_destroy_session(session_id: u64) {
    with_engine("hime_destroy_session", |slot| {
        if let Some(engine) = slot.as_mut() {
            engine.sessions.remove(&session_id);
        }
    });
}

/// Process one keystroke and return the `EngineResponse` as a
/// NUL-terminated JSON string owned by the caller (release it with
/// [`hime_free_string`]). `modifiers` is reserved and ignored.
/// Printable keys pass their ASCII code, punctuation included: in the
/// `ascii_punct = 1` mode the engine commits full-width punctuation for
/// mapped keys, and answers `handled: false` for the rest so the host
/// inserts them itself. With `english_punct_after_alnum` the first mapped
/// punctuation right after English letters or digits is released too, and
/// with `semicolon_quote_select` a standing candidate turns `;` and `'`
/// into 2nd/3rd-selection keys (SPEC §3).
///
/// Returns null if the engine is uninitialized, the session id is unknown,
/// or serialization fails.
#[no_mangle]
pub extern "C" fn hime_process_key(
    session_id: u64,
    key_code: u32,
    _modifiers: u32,
) -> *mut c_char {
    with_engine("hime_process_key", |slot| {
        let response = match slot
            .as_mut()
            .and_then(|engine| engine.sessions.get_mut(&session_id))
        {
            Some(session) => session.process_key(key_code),
            None => {
                eprintln!("hime-ffi: unknown session id {session_id}");
                return ptr::null_mut();
            }
        };
        match serde_json::to_string(&response)
            .ok()
            .and_then(|json| CString::new(json).ok())
        {
            Some(s) => s.into_raw(),
            None => {
                eprintln!("hime-ffi: failed to serialize EngineResponse");
                ptr::null_mut()
            }
        }
    })
}

/// Read the current engine configuration as a NUL-terminated JSON string
/// owned by the caller (release it with [`hime_free_string`]). The shape is
/// `{"page_size":<n>,"max_code_len":<n>,"code_type":<n>,"auto_commit":<bool>,
/// "auto_commit_length":<n>,"commit_next_code":<n>,"ascii_punct":<n>,
/// "english_punct_after_alnum":<bool>,"semicolon_quote_select":<bool>}`
/// (see [`hime_set_config`] and [`hime_core::Config`]).
///
/// Before `hime_init` the default configuration is reported. Returns null
/// only if serialization fails.
#[no_mangle]
pub extern "C" fn hime_get_config() -> *mut c_char {
    let config = with_engine("hime_get_config", |slot| {
        slot.as_ref().map(|engine| engine.config).unwrap_or_default()
    });
    match serde_json::to_string(&config).ok().and_then(|json| CString::new(json).ok()) {
        Some(s) => s.into_raw(),
        None => {
            eprintln!("hime-ffi: failed to serialize Config");
            ptr::null_mut()
        }
    }
}

/// Apply a configuration to every live session and to all sessions created
/// afterwards. `json` is a NUL-terminated JSON object; missing fields keep
/// their defaults and out-of-range values are clamped (see
/// [`hime_core::Config::sanitized`]).
///
/// Returns `false` on a null pointer, invalid UTF-8, malformed JSON, or an
/// uninitialized engine; the stored configuration survives a failed call
/// unchanged.
///
/// # Safety
/// `json` must be null or point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn hime_set_config(json: *const c_char) -> bool {
    if json.is_null() {
        eprintln!("hime-ffi: hime_set_config called with a null json");
        return false;
    }
    let text = match unsafe { CStr::from_ptr(json) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            eprintln!("hime-ffi: hime_set_config json is not valid UTF-8");
            return false;
        }
    };
    let config = match serde_json::from_str::<Config>(text) {
        Ok(config) => config.sanitized(),
        Err(e) => {
            eprintln!("hime-ffi: failed to parse config {text:?}: {e}");
            return false;
        }
    };
    with_engine("hime_set_config", |slot| match slot.as_mut() {
        None => {
            eprintln!("hime-ffi: hime_set_config before hime_init");
            false
        }
        Some(engine) => {
            engine.config = config;
            for session in engine.sessions.values_mut() {
                session.set_config(config);
            }
            true
        }
    })
}

/// List the schemes under `dir` (the `data/schemes/` layout: one folder per
/// scheme, each with a `metadata.toml` and a `*.txt` table) as a
/// NUL-terminated JSON array owned by the caller (release it with
/// [`hime_free_string`]). This is a plain filesystem read: it works at any
/// time, including before [`hime_init`]. Entries are sorted by folder name
/// and shaped like
/// `{"dir":…,"path":…,"table":…,"name":…,"describe":…,"author":…,"type":…,"max_code":…,"auto_commit":…,"auto_commit_length":…,"commit_next_code":…,"ascii_punct":…}`;
/// `dir` is the stable id, `path` is what to hand to [`hime_select_scheme`].
///
/// An unreadable directory yields `"[]"`; folders that fail to load are
/// skipped. Returns null on a null/invalid path or a serialization failure.
///
/// # Safety
/// `dir` must be null or point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn hime_scan_schemes(dir: *const c_char) -> *mut c_char {
    if dir.is_null() {
        eprintln!("hime-ffi: hime_scan_schemes called with a null dir");
        return ptr::null_mut();
    }
    let path = match unsafe { CStr::from_ptr(dir) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            eprintln!("hime-ffi: hime_scan_schemes dir is not valid UTF-8");
            return ptr::null_mut();
        }
    };
    let schemes = scan_schemes(Path::new(path));
    match serde_json::to_string(&schemes).ok().and_then(|json| CString::new(json).ok()) {
        Some(s) => s.into_raw(),
        None => {
            eprintln!("hime-ffi: failed to serialize the scheme list");
            ptr::null_mut()
        }
    }
}

/// Load the scheme folder at `scheme_dir` (as reported in the
/// [`hime_scan_schemes`] `path` field): its table becomes the engine
/// dictionary and its `metadata.toml` values become the engine
/// configuration, replacing user changes — that is how selecting a scheme
/// reloads the scheme defaults. `page_size` is not scheme-defined and
/// survives. Live sessions keep their ids, drop any in-progress composition,
/// and are rebuilt for the new source, so a scheme can be switched while
/// typing (including between a 形码 and a 形码整句 scheme). The engine is
/// initialized on demand.
///
/// A 形码整句 scheme (`type = 3` in its metadata) decodes whole sentences and
/// additionally needs the HFNGMF n-gram model; it is located as everywhere
/// else (see `find_model_path`: the [`hime_set_model_path`] override if set,
/// then `HIME_SENTENCE_MODEL`, then the user folder
/// `~/.config/hime/sentence-ngram.hng`, then the bundled
/// `data/sentence-ngram.hng`). If that model is absent, selecting such a
/// scheme fails and leaves the engine unchanged.
///
/// Returns `false` on a null/invalid path or when the folder's metadata, table
/// or (for 形码整句) model fails to load; the engine stays unchanged.
///
/// # Safety
/// `scheme_dir` must be null or point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn hime_select_scheme(scheme_dir: *const c_char) -> bool {
    if scheme_dir.is_null() {
        eprintln!("hime-ffi: hime_select_scheme called with a null scheme_dir");
        return false;
    }
    let path = match unsafe { CStr::from_ptr(scheme_dir) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            eprintln!("hime-ffi: hime_select_scheme scheme_dir is not valid UTF-8");
            return false;
        }
    };
    let info = match load_scheme(Path::new(path)) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("hime-ffi: failed to load scheme {path:?}: {e}");
            return false;
        }
    };
    // A 形码整句 (type 3) scheme decodes with the n-gram model over a sentence
    // table; every other scheme is a word table. Building the engine here lets
    // a bad model fail the call without touching the live engine.
    let (source, loaded_model) = match info.metadata.code_type {
        CodeType::XingmaSentence => {
            let Some(model) = find_model_path() else {
                eprintln!(
                    "hime-ffi: scheme {path:?} is 形码整句 but no .hng model was found (set one in the preferences 模型 page, or via HIME_SENTENCE_MODEL, or place ~/.config/hime/sentence-ngram.hng)"
                );
                return false;
            };
            match SentenceEngine::load(&model, &info.table) {
                Ok(engine) => (EngineSource::Sentence(Arc::new(engine)), Some(model)),
                Err(e) => {
                    eprintln!("hime-ffi: failed to load sentence engine for {path:?}: {e}");
                    return false;
                }
            }
        }
        _ => match Dictionary::load(&info.table) {
            Ok(dict) => (EngineSource::Table(Arc::new(dict)), None),
            Err(e) => {
                eprintln!("hime-ffi: failed to load dictionary {:?}: {e}", info.table);
                return false;
            }
        },
    };
    with_engine("hime_select_scheme", |slot| match slot.as_mut() {
        None => {
            let mut engine = Engine::new(source);
            engine.config = info.metadata.defaults_for(engine.config);
            engine.model_path = loaded_model;
            *slot = Some(engine);
            true
        }
        Some(engine) => {
            engine.config = info.metadata.defaults_for(engine.config);
            engine.source = source;
            engine.model_path = loaded_model;
            engine.rebuild_sessions();
            true
        }
    })
}

/// Path of the user's scheme folder `<data-dir>/schemes` (SPEC §5.2), where
/// user schemes sit beside the bundled `data/schemes/`, as a NUL-terminated
/// string owned by the caller (release it with [`hime_free_string`]). A
/// plain path query that works at any time, including before [`hime_init`].
/// Returns null when no data folder can be resolved.
#[no_mangle]
pub extern "C" fn hime_schemes_dir() -> *mut c_char {
    let Some(dir) = schemes_dir() else { return ptr::null_mut() };
    match CString::new(dir.as_os_str().to_string_lossy().into_owned()) {
        Ok(s) => s.into_raw(),
        Err(_) => {
            eprintln!("hime-ffi: schemes dir {dir:?} is not representable as a C string");
            ptr::null_mut()
        }
    }
}

/// Create the user data folder and its `schemes` subfolder when absent
/// (SPEC §5.2); a model file is never created or touched. Call it before
/// pointing the user at those folders. Returns false when no location can
/// be resolved or the filesystem fails.
#[no_mangle]
pub extern "C" fn hime_ensure_data_dir() -> bool {
    match ensure_config_layout() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("hime-ffi: hime_ensure_data_dir failed: {e}");
            false
        }
    }
}

/// Point the whole-sentence engine at one explicit `.hng` model file (the
/// preferences 模型 page, SPEC §7.5). `path` is a NUL-terminated path to an
/// existing readable file; null or an empty string clears the override and
/// restores automatic discovery (see [`hime_core::ngram::find_model_path`]).
/// The override is authoritative while set. It takes effect on the next
/// scheme load; the running engine keeps its already-mapped model, so a
/// shell applies a change by selecting the scheme again.
///
/// Returns false on invalid UTF-8 or when `path` names no existing file; a
/// failed call leaves the stored override unchanged.
///
/// # Safety
/// `path` must be null or point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn hime_set_model_path(path: *const c_char) -> bool {
    let text = if path.is_null() {
        ""
    } else {
        match unsafe { CStr::from_ptr(path) }.to_str() {
            Ok(s) => s,
            Err(_) => {
                eprintln!("hime-ffi: hime_set_model_path path is not valid UTF-8");
                return false;
            }
        }
    };
    if text.is_empty() {
        set_model_override(None);
        return true;
    }
    let candidate = Path::new(text);
    if !candidate.is_file() {
        eprintln!("hime-ffi: hime_set_model_path: no readable file at {text}");
        return false;
    }
    set_model_override(Some(candidate.to_path_buf()));
    true
}

/// Describe the whole-sentence n-gram model for the preferences 模型 page
/// (SPEC §7.5), as a NUL-terminated JSON string owned by the caller (release
/// it with [`hime_free_string`]). The shape is
/// `{"online":<bool>,"override_path":…,"default_path":…,"path":…,"error":…,
/// "modified_secs":…,"info":{…}|null}`:
///
/// - `online` is true exactly while the loaded engine decodes with the
///   model, i.e. a 形码整句 scheme is selected (面板显示 onAir/offLine);
/// - `override_path` / `default_path` report the [`hime_set_model_path`]
///   override and `<data-dir>/sentence-ngram.hng` when set/resolvable;
/// - `path`, `error`, `modified_secs` and `info` describe the file the
///   engine loaded, or (while offline) the file discovery resolves today;
///   `info` carries the header fields (`format`, `header_version`, `order`,
///   `vocab`, `hot_limit`, `bi_rows`, `tri_rows`, `code_bits`,
///   `code_range_oct`, `file_bytes`, `build_id`, `meta`) and is null when no
///   file loaded, with the reason in `error`.
///
/// Works before [`hime_init`] (then `online` is false). Returns null only
/// on a serialization failure.
#[no_mangle]
pub extern "C" fn hime_model_status() -> *mut c_char {
    let (online, loaded) = with_engine("hime_model_status", |slot| match slot.as_ref() {
        Some(engine) => {
            (matches!(engine.source, EngineSource::Sentence(_)), engine.model_path.clone())
        }
        None => (false, None),
    });
    let probe = probe_model(loaded.as_deref());
    let mut value = match serde_json::to_value(&probe) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("hime-ffi: failed to serialize the model status");
            return ptr::null_mut();
        }
    };
    value["online"] = serde_json::Value::Bool(online);
    match CString::new(value.to_string()) {
        Ok(s) => s.into_raw(),
        Err(_) => {
            eprintln!("hime-ffi: model status contains an interior NUL");
            ptr::null_mut()
        }
    }
}

/// Free a string returned by [`hime_process_key`], [`hime_get_config`],
/// [`hime_scan_schemes`], [`hime_schemes_dir`] or [`hime_model_status`].
/// Null is a no-op.
///
/// # Safety
/// `ptr` must be null or a value returned by [`hime_process_key`],
/// [`hime_get_config`], [`hime_scan_schemes`], [`hime_schemes_dir`] or
/// [`hime_model_status`] in this process and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn hime_free_string(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    let outcome = catch_unwind(AssertUnwindSafe(|| drop(CString::from_raw(ptr))));
    if outcome.is_err() {
        eprintln!("hime-ffi: panic while freeing string (leaked)");
    }
}
