//! C-ABI boundary for the Hime engine (SPEC_MVP §6).
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
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};

use hime_core::{load_scheme, scan_schemes, Config, Dictionary, Session};

/// Global engine state, present only after a successful `hime_init`.
struct Engine {
    dict: Arc<Dictionary>,
    config: Config,
    sessions: HashMap<u64, Session>,
    next_session_id: u64,
}

impl Engine {
    fn new(dict: Arc<Dictionary>) -> Self {
        Self { dict, config: Config::default(), sessions: HashMap::new(), next_session_id: 1 }
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
                *slot = Some(Engine::new(dict));
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
            let session = Session::with_config(Arc::clone(&engine.dict), engine.config);
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
/// [`hime_free_string`]). `modifiers` is reserved and ignored in the MVP.
/// Printable keys pass their ASCII code, punctuation included: in the
/// `ascii_punct = 1` mode the engine commits full-width punctuation for
/// mapped keys, and answers `handled: false` for the rest so the host
/// inserts them itself.
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
/// "auto_commit_length":<n>,"commit_next_code":<n>,"ascii_punct":<n>}`
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
/// survives. Live sessions keep their ids, adopt the new dictionary and
/// config, and drop any in-progress composition; a scheme can therefore be
/// switched while typing. The engine is initialized on demand.
///
/// Returns `false` on a null/invalid path or when the folder's metadata or
/// table fails to load; the engine stays unchanged in that case.
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
    let dict = match Dictionary::load(&info.table) {
        Ok(dict) => Arc::new(dict),
        Err(e) => {
            eprintln!("hime-ffi: failed to load dictionary {:?}: {e}", info.table);
            return false;
        }
    };
    with_engine("hime_select_scheme", |slot| match slot.as_mut() {
        None => {
            let mut engine = Engine::new(dict);
            engine.config = info.metadata.defaults_for(engine.config);
            *slot = Some(engine);
            true
        }
        Some(engine) => {
            engine.dict = Arc::clone(&dict);
            engine.config = info.metadata.defaults_for(engine.config);
            for session in engine.sessions.values_mut() {
                session.set_dict(Arc::clone(&dict));
                session.set_config(engine.config);
            }
            true
        }
    })
}

/// Free a string returned by [`hime_process_key`], [`hime_get_config`] or
/// [`hime_scan_schemes`]. Null is a no-op.
///
/// # Safety
/// `ptr` must be null or a value returned by [`hime_process_key`],
/// [`hime_get_config`] or [`hime_scan_schemes`] in this process and not yet
/// freed.
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
