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

use hime_core::ngram::{find_model_path, probe_model, set_model_override, NgramModel};
use hime_core::sentence::SentenceEngine;
use hime_core::pinyin::SharedPinyin;
use hime_core::typing::{TypingStats, TypingStatsInfo};
use hime_core::user_ngram::{UserNgram, UserNgramEdit, UserNgramQuery};
use hime_core::{
    ensure_config_layout, ensure_statistics_dir, ensure_user_ngram_dir, load_scheme,
    scan_schemes, schemes_dir, typing_stats_path, user_ngram_path, CodeType, Config, Dictionary,
    PinyinLexicon, ReverseLookup, Session,
};

/// Where candidates come from: a 形码 word table, the whole-sentence engine a
/// 形码整句 (type 3) scheme needs, or the pinyin lexicon a 全拼 (type 0)
/// scheme carries. This decides how every session is built.
enum EngineSource {
    Table(Arc<Dictionary>),
    Sentence(Arc<SentenceEngine>),
    /// A 全拼 (type 0) scheme. The model is present when an `.hng` was found:
    /// then the sessions decode whole sentences, not just table words.
    Pinyin { lexicon: Arc<PinyinLexicon>, model: Option<Arc<NgramModel>> },
}

/// Global engine state, present only after a successful `hime_init`.
struct Engine {
    source: EngineSource,
    config: Config,
    sessions: HashMap<u64, Session>,
    next_session_id: u64,
    /// The shared pinyin table: the candidate source of a 全拼 scheme and the
    /// query side of 反查 in every other scheme (SPEC §8). The table is read
    /// on first use, so a process that never needs pinyin pays nothing for
    /// it; `None` when no table was found beside the schemes, in which case
    /// 反查 stays unavailable.
    pinyin: Option<Arc<SharedPinyin>>,
    /// The `.hng` path a 形码整句 source loaded from (SPEC §7.5); `None`
    /// for a word table. Reported by `hime_model_status` so the panel can
    /// show the file the engine actually uses.
    model_path: Option<PathBuf>,
    /// The learned per-user model (SPEC §7.7), loaded when the selected
    /// scheme declares `user_ngram = true`. Every session of that scheme
    /// records into it, and the sources blend it into their score.
    user_ngram: Option<Arc<UserNgram>>,
    /// Daily character counts, first-choice counters and the per-scheme key
    /// histograms (SPEC §9). Loaded once per process and shared by every
    /// session; a scheme switch only redirects the histogram.
    typing: Arc<TypingStats>,
}

impl Engine {
    fn new(source: EngineSource, pinyin_path: Option<PathBuf>) -> Self {
        Self {
            source,
            config: Config::default(),
            sessions: HashMap::new(),
            next_session_id: 1,
            model_path: None,
            pinyin: pinyin_path.map(|path| Arc::new(SharedPinyin::new(path))),
            user_ngram: None,
            typing: Arc::new(TypingStats::load(typing_stats_path())),
        }
    }

    /// Build a fresh session for the current source and config. A 形码整句
    /// source builds a sentence session (long codes decode whole sentences);
    /// a table source builds the word-lookup session. Every session records
    /// into the shared typing statistics (SPEC §9).
    fn build_session(&self) -> Session {
        self.build_source_session().with_stats(Arc::clone(&self.typing))
    }

    fn build_source_session(&self) -> Session {
        // 反查 shares one pinyin table across schemes; a scheme whose table is
        // missing simply keeps `` ` `` as punctuation. A 全拼 session has no
        // lookup at all — that scheme *is* the pinyin side (SPEC §8.3).
        let session = match &self.source {
            EngineSource::Table(dict) => Session::with_config(Arc::clone(dict), self.config),
            EngineSource::Sentence(engine) => Session::sentence(Arc::clone(engine), self.config),
            EngineSource::Pinyin { lexicon, model: None } => {
                // Without the sentence model the word stage still serves the
                // window; the learned model keeps recording commits there.
                return Session::pinyin(Arc::clone(lexicon), self.config)
                    .with_user_ngram(self.user_ngram.clone());
            }
            EngineSource::Pinyin { lexicon, model: Some(model) } => {
                return Session::pinyin_sentence(
                    Arc::clone(lexicon),
                    Arc::clone(model),
                    self.user_ngram.clone(),
                    self.config,
                );
            }
        };
        match (&self.pinyin, &self.source) {
            (Some(pinyin), EngineSource::Table(dict)) => session.with_reverse(Arc::new(
                ReverseLookup::for_table(Arc::clone(pinyin), Arc::clone(dict)),
            )),
            (Some(pinyin), EngineSource::Sentence(engine)) => session.with_reverse(Arc::new(
                ReverseLookup::for_sentence(Arc::clone(pinyin), Arc::clone(engine)),
            )),
            _ => session,
        }
    }

    /// Rebuild every live session in place (ids kept) for the current source and
    /// config, dropping in-progress compositions. Used when a scheme switches
    /// the candidate source, including between word and whole-sentence modes,
    /// which a session cannot change by mutation.
    fn rebuild_sessions(&mut self) {
        let ids: Vec<u64> = self.sessions.keys().copied().collect();
        for id in ids {
            if let Some(previous) = self.sessions.get_mut(&id) {
                previous.end_run();
            }
            let session = self.build_session();
            self.sessions.insert(id, session);
        }
    }
}

/// Where the pinyin table lives: the `pinyin` scheme folder beside the one
/// being selected (both the bundled `data/schemes` and the user folder keep
/// that layout, SPEC §5.2/§8.1), or an explicit file given by
/// `hime_set_pinyin_path`. A 全拼 scheme carries its own table, so it wins.
fn pinyin_table_near(scheme_path: &Path) -> Option<PathBuf> {
    let schemes_dir = scheme_path.parent()?;
    if let Ok(info) = load_scheme(&schemes_dir.join("pinyin")) {
        let path = PathBuf::from(info.table);
        if path.is_file() {
            return Some(path);
        }
    }
    None
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
            let pinyin = pinyin_table_near(Path::new(path));
            // The legacy entry point has no scheme folder; attribute its key
            // presses to the table's own folder when there is one.
            let scheme = Path::new(path)
                .parent()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            with_engine("hime_init", |slot| {
                if let Some(previous) = slot.take() {
                    previous.typing.flush();
                    if let Some(user) = previous.user_ngram {
                        user.flush();
                    }
                }
                let engine = Engine::new(EngineSource::Table(dict), pinyin);
                engine.typing.set_scheme(&scheme);
                *slot = Some(engine);
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

/// Destroy a session. Unknown ids are ignored. The session's typing stretch
/// is closed into the statistics first, and the counters are flushed, so an
/// app switch or a quit does not lose the last run.
#[no_mangle]
pub extern "C" fn hime_destroy_session(session_id: u64) {
    with_engine("hime_destroy_session", |slot| {
        if let Some(engine) = slot.as_mut() {
            if let Some(mut session) = engine.sessions.remove(&session_id) {
                session.end_run();
            }
            engine.typing.flush();
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
/// punctuation right after English letters or digits is released too and
/// pressing that same key again answers a correction (`replace_text` holds
/// the ASCII character to overwrite with `commit_text`), and
/// with `semicolon_quote_select` a standing candidate turns `;` and `'`
/// into 2nd/3rd-selection keys (SPEC §3).
///
/// The response carries `preedit` (the raw encoding), `code_line` (the
/// display line, word-separated when the top candidate supplies a split),
/// `code_in_window` (render it in the candidate window), `focus` (the
/// zero-based focused candidate Space commits), one-based `page` /
/// `page_count`, and candidates whose `code` is the whole-sentence split and
/// whose `spans` are character-level diff runs against the first candidate
/// (empty for table matches).
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
/// "english_punct_after_alnum":<bool>,"semicolon_quote_select":<bool>,
/// "show_code_in_window":<bool>,"vertical_candidates":<bool>}` (see
/// [`hime_set_config`] and [`hime_core::Config`]).
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
/// reloads the scheme defaults. `page_size`, `english_punct_after_alnum`,
/// `semicolon_quote_select`, `show_code_in_window` and `vertical_candidates`
/// are not scheme-defined and survive. Live
/// sessions keep their ids, drop any in-progress composition,
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
    // table, a 全拼 (type 0) scheme looks pinyin words up, and every other
    // scheme is a word table. Building the engine here lets a bad model or
    // table fail the call without touching the live engine.
    let mut pinyin = pinyin_table_near(Path::new(path));
    // The learned user model is per user, not per scheme: the scheme only
    // decides whether it is used (SPEC §7.7).
    let user = if info.metadata.user_ngram { load_user_ngram() } else { None };
    let (source, loaded_model) = match info.metadata.code_type {
        CodeType::XingmaSentence => {
            let Some(model) = find_model_path() else {
                eprintln!(
                    "hime-ffi: scheme {path:?} is 形码整句 but no .hng model was found (set one in the preferences 模型 page, or via HIME_SENTENCE_MODEL, or place ~/.config/hime/sentence-ngram.hng)"
                );
                return false;
            };
            match SentenceEngine::load(&model, &info.table) {
                Ok(engine) => {
                    let engine = engine.with_user_ngram(user.clone());
                    (EngineSource::Sentence(Arc::new(engine)), Some(model))
                }
                Err(e) => {
                    eprintln!("hime-ffi: failed to load sentence engine for {path:?}: {e}");
                    return false;
                }
            }
        }
        CodeType::Quanpin => match PinyinLexicon::load(&info.table) {
            Ok(lexicon) => {
                // The scheme's own table is also the reverse source, so a
                // user pinyin scheme serves 反查 in the shape schemes.
                pinyin = Some(PathBuf::from(&info.table));
                // Whole-sentence decoding needs the n-gram model; without one
                // the scheme still types words and 简拼, so a missing model is
                // reported and not fatal here.
                let model = find_model_path().and_then(|path| match NgramModel::load(&path) {
                    Ok(model) => Some((Arc::new(model), path)),
                    Err(e) => {
                        eprintln!(
                            "hime-ffi: 全拼 sentence decoding is off, model {path:?} failed to load: {e}"
                        );
                        None
                    }
                });
                let model_path = model.as_ref().map(|(_, path)| path.clone());
                let source = EngineSource::Pinyin {
                    lexicon: Arc::new(lexicon),
                    model: model.map(|(model, _)| model),
                };
                (source, model_path)
            }
            Err(e) => {
                eprintln!("hime-ffi: failed to load pinyin table {:?}: {e}", info.table);
                return false;
            }
        },
        _ => match Dictionary::load(&info.table) {
            Ok(dict) => (EngineSource::Table(Arc::new(dict)), None),
            Err(e) => {
                eprintln!("hime-ffi: failed to load dictionary {:?}: {e}", info.table);
                return false;
            }
        },
    };
    // The key histogram follows the scheme the user typed with (SPEC §9);
    // presses recorded before any scheme loads stay unattributed.
    let scheme_dir = info.dir.clone();
    let switched = with_engine("hime_select_scheme", |slot| match slot.as_mut() {
        None => {
            let mut engine = Engine::new(source, pinyin);
            engine.config = info.metadata.defaults_for(engine.config);
            engine.model_path = loaded_model;
            engine.user_ngram = user;
            engine.typing.set_scheme(&scheme_dir);
            *slot = Some(engine);
            true
        }
        Some(engine) => {
            engine.config = info.metadata.defaults_for(engine.config);
            engine.source = source;
            engine.model_path = loaded_model;
            // Learned counts still in memory are written before the handle
            // they belong to is replaced.
            if let Some(previous) = engine.user_ngram.take() {
                previous.flush();
            }
            engine.user_ngram = user;
            // A different pinyin table (or a different folder) replaces the
            // shared one; an unchanged path keeps the loaded table and every
            // running session keeps reading it.
            let same_path = match (&engine.pinyin, &pinyin) {
                (Some(current), Some(path)) => current.path() == path,
                (None, None) => true,
                _ => false,
            };
            if !same_path {
                engine.pinyin = pinyin.map(|path| Arc::new(SharedPinyin::new(path)));
            }
            engine.typing.set_scheme(&scheme_dir);
            engine.rebuild_sessions();
            engine.typing.flush();
            true
        }
    });
    switched
}

/// Load the shared user model when the data folder resolves. A file the
/// loader rejects still yields a handle: it reports the error and blocks
/// writes, so the panel can offer a clear instead of silently overwriting.
fn load_user_ngram() -> Option<Arc<UserNgram>> {
    let path = user_ngram_path()?;
    Some(Arc::new(UserNgram::load(path)))
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
///   model: a 形码整句 scheme, or a 全拼 scheme whose `.hng` loaded (面板显示
///   onAir/offLine);
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
            (
                matches!(
                    engine.source,
                    EngineSource::Sentence(_) | EngineSource::Pinyin { model: Some(_), .. }
                ),
                engine.model_path.clone(),
            )
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

/// Describe the learned user n-gram model for the preferences 用户模型 page
/// (SPEC §7.7), as a NUL-terminated JSON string owned by the caller (release
/// it with [`hime_free_string`]). The shape is
/// `{"active":<bool>,"path":…,"folder":…,"info":{…}}`:
///
/// - `active` is true exactly while the selected scheme declares
///   `user_ngram = true`, i.e. while commits are learned and blended;
/// - `path` is `<data-dir>/userNgram/user.hng` (the folder is reported
///   beside it for the "open folder" action);
/// - `info` carries the model's statistics (`format`, `storage`, `exists`,
///   `file_bytes`, `modified_secs`, `unigrams`, `bigrams`, `trigrams`,
///   `vocab`, `unigram_total`, `built_unixtime`) and `error` when the file
///   could not be read. It is null only when no data folder resolves.
///
/// Works before [`hime_init`] (then `active` is false). Returns null only on
/// a serialization failure.
#[no_mangle]
pub extern "C" fn hime_user_ngram_status() -> *mut c_char {
    let (active, model) = with_engine("hime_user_ngram_status", |slot| match slot.as_ref() {
        Some(engine) => (
            engine.config.user_ngram && engine.user_ngram.is_some(),
            engine.user_ngram.clone(),
        ),
        None => (false, None),
    });
    let model = model.or_else(|| load_user_ngram());
    let (path, folder) = match user_ngram_path() {
        Some(path) => (
            path.display().to_string(),
            path.parent().map(|dir| dir.display().to_string()),
        ),
        None => (String::new(), None),
    };
    let value = serde_json::json!({
        "active": active,
        "path": path,
        "folder": folder,
        "info": model.as_deref().map(UserNgram::info),
    });
    match CString::new(value.to_string()) {
        Ok(s) => s.into_raw(),
        Err(_) => {
            eprintln!("hime-ffi: user model status contains an interior NUL");
            ptr::null_mut()
        }
    }
}

/// Query one table of the learned user model (SPEC §7.7). `json` is a
/// NUL-terminated object `{"scope":"char"|"bigram"|"trigram",
/// "query":<substring>,"limit":<n>,"offset":<n>}`; missing fields default to
/// the character table, no filter, 100 rows and no offset. Returns
/// `{"scope":…,"total":<matching rows>,"entries":[{"scope","key","prev2",
/// "prev","text","count","total"}],"info":{…}}` — `total` on each entry is
/// the context's own total, so a row can render `count / total`.
///
/// Returns null on a null/invalid pointer, malformed JSON or a serialization
/// failure; an unusable request yields `"total":0` instead.
///
/// # Safety
/// `json` must be null or point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn hime_user_ngram_query(json: *const c_char) -> *mut c_char {
    let Some(request) = (unsafe { parse_json_arg::<UserNgramQuery>("hime_user_ngram_query", json) })
    else {
        return ptr::null_mut();
    };
    let model = user_ngram_handle();
    let result = match model {
        Some(model) => model.query(&request),
        None => return ptr::null_mut(),
    };
    match serde_json::to_string(&result).ok().and_then(|json| CString::new(json).ok()) {
        Some(s) => s.into_raw(),
        None => {
            eprintln!("hime-ffi: failed to serialize the user model query");
            ptr::null_mut()
        }
    }
}

/// Apply one edit to the learned user model and save it immediately (SPEC
/// §7.7). `json` is a NUL-terminated object `{"action":"add"|"set"|"delete"|
/// "clear","scope":"char"|"bigram"|"trigram","prev2":<char>,"prev":<char>,
/// "text":<char>,"count":<n>}`. `add` increments by `count` (0 means 1),
/// `set` replaces the count (0 deletes), `delete` removes the entry, and
/// `clear` wipes the whole model, which also unblocks a file the loader
/// rejected. Returns false on a null/invalid pointer, malformed JSON, a text
/// that is not exactly one storable character, or a model whose file is
/// unreadable (clear it first); the running engine sees the change on the
/// next query.
///
/// # Safety
/// `json` must be null or point to a valid NUL-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn hime_user_ngram_edit(json: *const c_char) -> bool {
    let Some(edit) = (unsafe { parse_json_arg::<UserNgramEdit>("hime_user_ngram_edit", json) })
    else {
        return false;
    };
    let Some(model) = user_ngram_handle() else { return false };
    match model.edit(&edit) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("hime-ffi: hime_user_ngram_edit failed: {e}");
            false
        }
    }
}

/// Create the `userNgram` folder under the data directory (SPEC §7.7) and
/// return the model path as a NUL-terminated string owned by the caller
/// (release it with [`hime_free_string`]). The model file itself appears on
/// the first save. Returns null when no data folder resolves.
#[no_mangle]
pub extern "C" fn hime_user_ngram_ensure_dir() -> *mut c_char {
    match ensure_user_ngram_dir() {
        Ok(path) => match CString::new(path.as_os_str().to_string_lossy().into_owned()) {
            Ok(s) => s.into_raw(),
            Err(_) => ptr::null_mut(),
        },
        Err(e) => {
            eprintln!("hime-ffi: hime_user_ngram_ensure_dir failed: {e}");
            ptr::null_mut()
        }
    }
}

/// Save the learned user model now (throttled saves happen while typing; a
/// session end or an app quit should call this). Returns false when nothing
/// could be written because the file is unreadable.
#[no_mangle]
pub extern "C" fn hime_user_ngram_save() -> bool {
    match user_ngram_handle() {
        Some(model) => match model.save() {
            Ok(()) => true,
            Err(e) => {
                eprintln!("hime-ffi: hime_user_ngram_save failed: {e}");
                false
            }
        },
        None => false,
    }
}

/// The model the management calls act on: the engine's live handle when the
/// selected scheme learned anything, or a fresh load of the same file, so the
/// panel can read and edit before any scheme is selected.
fn user_ngram_handle() -> Option<Arc<UserNgram>> {
    let live = with_engine("user_ngram_handle", |slot| {
        slot.as_ref().and_then(|engine| engine.user_ngram.clone())
    });
    live.or_else(load_user_ngram)
}

/// Describe the typing statistics for the preferences 数据指标 and 键盘用指
/// pages (SPEC §9), as a NUL-terminated JSON string owned by the caller
/// (release it with [`hime_free_string`]). The shape is
/// `{"path":…,"exists":…,"file_bytes":…,"modified_secs":…,"active_scheme":…,
/// "error":…,"first_day":…,"chars":{"total","today","week","month","year"},
/// "speed":{"runs","chars","keys","millis","chars_per_min","keys_per_sec",
/// "code_length"},"first_choice":{"selections","preferred","rate"},
/// "keyboard":{…},"keyboards":[…],"days":[{"date":…,"chars":…}]}`.
///
/// - `chars` counts every committed or released character (punctuation,
///   letters and digits included); `today` / `week` / `month` / `year` follow
///   the local calendar;
/// - `speed` averages the runs that carried more than
///   [`hime_core::typing::RUN_MIN_CHARS`] characters: characters per minute,
///   keystrokes per second and keystrokes per character;
/// - `first_choice.rate` is the share of candidate selections that took the
///   first entry, in percent;
/// - `keyboard` is the selected scheme's heat map (`keys`: canonical key
///   name → presses) with its left/right/two-thumb totals, `balance`
///   (`100 - |left - right| / (left + right) * 100`) and per-finger shares;
///   `keyboards` carries one such block per scheme with recorded presses;
/// - `days` lists the last [`hime_core::typing::HEATMAP_DAYS`] days, oldest
///   first, for the activity grid.
///
/// Works before any scheme is selected and without a data folder (then the
/// counters are zero and `path` is empty). Reads the live engine's counters,
/// so it reflects typing that has not been saved yet. Returns null only on a
/// serialization failure.
#[no_mangle]
pub extern "C" fn hime_typing_stats() -> *mut c_char {
    let info: Option<TypingStatsInfo> =
        with_engine("hime_typing_stats", |slot| match slot.as_ref() {
            Some(engine) => Some(engine.typing.info()),
            None => Some(TypingStats::load(typing_stats_path()).info()),
        });
    let Some(info) = info else {
        eprintln!("hime-ffi: hime_typing_stats panicked while collecting");
        return ptr::null_mut();
    };
    match serde_json::to_string(&info).ok().and_then(|json| CString::new(json).ok()) {
        Some(s) => s.into_raw(),
        None => {
            eprintln!("hime-ffi: failed to serialize the typing statistics");
            ptr::null_mut()
        }
    }
}

/// Create the `statistics` folder under the data directory and return the
/// statistics file path as a NUL-terminated string owned by the caller
/// (release it with [`hime_free_string`]). The file itself appears on the
/// first save. Returns null when no data folder resolves.
#[no_mangle]
pub extern "C" fn hime_typing_ensure_dir() -> *mut c_char {
    match ensure_statistics_dir() {
        Ok(path) => match CString::new(path.as_os_str().to_string_lossy().into_owned()) {
            Ok(s) => s.into_raw(),
            Err(_) => ptr::null_mut(),
        },
        Err(e) => {
            eprintln!("hime-ffi: hime_typing_ensure_dir failed: {e}");
            ptr::null_mut()
        }
    }
}

/// Save the typing statistics now. Typing saves them on a throttle; call
/// this when the preferences window closes or the app quits. Returns true
/// when everything is on disk (a missing data folder is nothing to save).
#[no_mangle]
pub extern "C" fn hime_typing_save() -> bool {
    let live = with_engine("hime_typing_save", |slot| {
        slot.as_ref().map(|engine| Arc::clone(&engine.typing))
    });
    let stats = match live {
        Some(stats) => stats,
        None => Arc::new(TypingStats::load(typing_stats_path())),
    };
    match stats.save() {
        Ok(()) => true,
        Err(e) => {
            eprintln!("hime-ffi: hime_typing_save failed: {e}");
            false
        }
    }
}

/// Parse a JSON argument for a management call, reporting the reason.
unsafe fn parse_json_arg<T: serde::de::DeserializeOwned>(
    what: &str,
    json: *const c_char,
) -> Option<T> {
    if json.is_null() {
        eprintln!("hime-ffi: {what} called with a null json");
        return None;
    }
    let text = match unsafe { CStr::from_ptr(json) }.to_str() {
        Ok(text) => text,
        Err(_) => {
            eprintln!("hime-ffi: {what} json is not valid UTF-8");
            return None;
        }
    };
    match serde_json::from_str::<T>(text) {
        Ok(value) => Some(value),
        Err(e) => {
            eprintln!("hime-ffi: failed to parse {what} json {text:?}: {e}");
            None
        }
    }
}

/// Free a string returned by [`hime_process_key`], [`hime_get_config`],
/// [`hime_scan_schemes`], [`hime_schemes_dir`], [`hime_model_status`],
/// [`hime_user_ngram_status`], [`hime_user_ngram_query`],
/// [`hime_user_ngram_ensure_dir`], [`hime_typing_stats`] or
/// [`hime_typing_ensure_dir`]. Null is a no-op.
///
/// # Safety
/// `ptr` must be null or a value returned by one of those functions in this
/// process and not yet freed.
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
