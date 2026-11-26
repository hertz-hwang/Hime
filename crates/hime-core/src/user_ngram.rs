//! Per-user character n-gram model (SPEC §7.7).
//!
//! Schemes whose `metadata.toml` declares `user_ngram = true` learn the text
//! the user commits and re-rank candidates with the learned model beside the
//! default whole-sentence model. The learned data is one HFNGMF v1 (`.hng`)
//! file at `<data-dir>/userNgram/user.hng` (see [`crate::paths`]), so the same
//! container format, the same zero-copy reader shape and the same tooling
//! apply to both models.
//!
//! **The file stores counts, not probabilities.** Rows carry the raw
//! occurrence counts of their successors and the `λ` field carries the
//! context's total count; `meta` declares `"hime-kind":"user-ngram"` and
//! `"storage":"counts"`. The whole-sentence reader rejects that marker, and
//! [`UserNgram::load`] rejects a default model the same way, so the two files
//! can never be mistaken for each other.
//!
//! Scoring interpolates in probability space, ported from the reference
//! `UserCharNgramData.blendedLogp`:
//!
//! ```text
//! α            = min(αmax, T(prev1) / (T(prev1) + K))
//! p_uni(c)     = (c(c) + K_u · p_general(c)) / (N + K_u)
//! p_bi(c|w)    = (c(w,c) + D2 · p_uni(c)) / (T(w) + D2)
//! p_user(c|v,w) = (c(v,w,c) + D3 · p_bi(c|w)) / (T(v,w) + D3)
//! logp         = ln((1 − α)·e^base + α·p_user)
//! ```
//!
//! A context the user model never observed yields `None`: the caller keeps
//! the default model's score exactly, so an empty model leaves every ranking
//! bit-identical. Learning only ever contributes to candidate *ordering*; the
//! 形码整句 decoder strips it from `mass` so auto-commit confidence stays
//! calibrated to the default model.
//!
//! Counting rules: one chain per committed fragment, seeded with the two
//! characters committed earlier in the same session (BOS at the start), so a
//! session's end is never linked to the next session's beginning. Texts
//! without a Han character are not learned; characters outside the BMP are
//! skipped because the format's vocabulary is BMP-only.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::ngram::{f64_to_bf16, CP_BOS, CP_EOS};

/// Marker the user model's `meta` carries; the whole-sentence reader rejects
/// it and reading a user model checks it is present.
pub(crate) const USER_NGRAM_MARKER: &str = "\"hime-kind\":\"user-ngram\"";

/// Vocabulary ids 0 (UNK), 1 (BOS) and 2 (EOS) are reserved.
const RESERVED_IDS: u32 = 3;
/// `vocab_count` is a u16 field with `0xFFFF` reserved as "no id".
const MAX_VOCAB: usize = 0xFFFE;

/// Interpolation weight ceiling: a confident user model still cannot drown
/// the default one.
const ALPHA_MAX: f64 = 0.35;
/// α = T(ctx)/(T(ctx)+K): the more often a context was seen, the more the
/// user model is trusted for it.
const CONTEXT_K: f64 = 8.0;
/// Absolute discount of user trigram counts toward the user bigram.
const TRIGRAM_DISCOUNT: f64 = 4.0;
/// Absolute discount of user bigram counts toward the user unigram.
const BIGRAM_DISCOUNT: f64 = 2.0;
/// Mass the user unigram reserves for the default model's unigram.
const UNIGRAM_K: f64 = 4.0;

/// Capacity caps: the smallest counts are evicted first so the file (and the
/// memory behind it) stays bounded for a heavy typist.
const MAX_UNIGRAMS: usize = 20_000;
const MAX_BIGRAMS: usize = 200_000;
const MAX_TRIGRAMS: usize = 400_000;

/// Minimum spacing between throttled saves while typing.
const SAVE_INTERVAL: Duration = Duration::from_secs(15);

// ---- key packing ----

#[inline]
fn pack2(a: u32, b: u32) -> u64 {
    ((a as u64) << 32) | u64::from(b)
}

#[inline]
fn unpack2(key: u64) -> (u32, u32) {
    ((key >> 32) as u32, key as u32)
}

#[inline]
fn pack3(a: u32, b: u32, c: u32) -> u64 {
    ((a as u64) << 32) | ((b as u64) << 16) | u64::from(c)
}

#[inline]
fn unpack3(key: u64) -> (u32, u32, u32) {
    ((key >> 32) as u32, ((key >> 16) & 0xFFFF) as u32, (key & 0xFFFF) as u32)
}

/// A character the model can hold: in the BMP, printable, and neither of the
/// two control code points the format reserves.
#[inline]
fn learnable(ch: char) -> Option<u32> {
    let cp = ch as u32;
    (cp >= 0x20 && cp < 0x10000 && cp != CP_BOS && cp != CP_EOS).then_some(cp)
}

/// Whether a code point is a Han character (basic block, extension A or the
/// compatibility block); texts without one are not learned.
#[inline]
fn is_han(cp: u32) -> bool {
    (0x4E00..=0x9FFF).contains(&cp)
        || (0x3400..=0x4DBF).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
}

fn cp_string(cp: u32) -> String {
    if cp == CP_BOS {
        String::new() // the surface renders this as （句首）
    } else {
        char::from_u32(cp).map(String::from).unwrap_or_default()
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ---- scopes ----

/// Which order of the user model a query or edit addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserNgramScope {
    /// Single characters.
    Char,
    /// Character pairs (`prev` → `text`).
    Bigram,
    /// Character triples (`prev2`, `prev` → `text`).
    Trigram,
}

impl Default for UserNgramScope {
    fn default() -> Self {
        Self::Char
    }
}

/// One row of the user model, as the preferences manager shows it. `total` is
/// the context's total count (the unigram total for `char`), so a client can
/// render `count / total` without a second call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserNgramEntry {
    /// Which table this row belongs to.
    pub scope: UserNgramScope,
    /// Stable identity for a UI list: `char:<cp>` / `bigram:<cp>,<cp>` /
    /// `trigram:<cp>,<cp>,<cp>`.
    pub key: String,
    /// Older context character (trigram only; empty when the entry starts at
    /// the beginning of a sentence).
    pub prev2: String,
    /// Immediate context character (empty = beginning of a sentence).
    pub prev: String,
    /// The character this entry counts.
    pub text: String,
    /// The learned count.
    pub count: u64,
    /// Total count of the context.
    pub total: u64,
}

/// A query against the user model.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UserNgramQuery {
    /// Table to list.
    pub scope: UserNgramScope,
    /// Substring to keep (all entries when empty).
    pub query: String,
    /// Rows to return, at most.
    pub limit: usize,
    /// Rows to skip after sorting by count.
    pub offset: usize,
}

impl UserNgramQuery {
    fn limit(&self) -> usize {
        if self.limit == 0 {
            100
        } else {
            self.limit.min(1000)
        }
    }
}

/// The answer to a [`UserNgramQuery`], with the model's current statistics so
/// the panel can refresh both in one round trip.
#[derive(Debug, Clone, Serialize)]
pub struct UserNgramQueryResult {
    pub scope: UserNgramScope,
    /// Matching rows before the offset/limit window.
    pub total: usize,
    pub entries: Vec<UserNgramEntry>,
    pub info: UserNgramInfo,
}

/// What an edit does to one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum UserNgramAction {
    /// Increment the count (`count` 0 means 1).
    #[default]
    Add,
    /// Replace the count; 0 deletes.
    Set,
    /// Delete the entry.
    Delete,
    /// Wipe the whole model and allow writes again.
    Clear,
}

/// One edit request (the C ABI carries this as JSON).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UserNgramEdit {
    pub action: UserNgramAction,
    pub scope: UserNgramScope,
    /// Older context character (trigram only; empty = BOS).
    pub prev2: String,
    /// Immediate context character (empty = BOS).
    pub prev: String,
    /// The counted character (one character).
    pub text: String,
    /// Count for [`UserNgramAction::Add`] / [`UserNgramAction::Set`].
    pub count: u64,
}

/// Serializable statistics of one user model file.
#[derive(Debug, Clone, Serialize)]
pub struct UserNgramInfo {
    /// Container format the file was written in.
    pub format: String,
    /// What the row masses mean (`counts`).
    pub storage: String,
    /// Absolute file path.
    pub path: String,
    /// Whether the file exists right now.
    pub exists: bool,
    /// Declared file size (0 when missing).
    pub file_bytes: u64,
    /// File modification time in Unix seconds.
    pub modified_secs: Option<i64>,
    pub unigrams: usize,
    pub bigrams: usize,
    pub trigrams: usize,
    /// Vocabulary entries in the file (reserved ids included).
    pub vocab: usize,
    /// Total learned characters, the unigram denominator.
    pub unigram_total: u64,
    /// Build time the file carries.
    pub built_unixtime: u64,
    /// Why the file could not be read; writes stay blocked until a clear.
    pub error: Option<String>,
}

impl UserNgramInfo {
    /// One-line entry summary for logs.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} 字 / {} 二元 / {} 三元",
            self.unigrams, self.bigrams, self.trigrams
        )
    }
}

// ---- counts ----

#[derive(Debug, Default)]
struct Counts {
    unigrams: HashMap<u32, u64>,
    /// `pack2(prev1, target)` → count.
    bigrams: HashMap<u64, u64>,
    /// `pack3(prev2, prev1, target)` → count.
    trigrams: HashMap<u64, u64>,
    /// `prev1` → total.
    bigram_totals: HashMap<u32, u64>,
    /// `pack2(prev2, prev1)` → total.
    trigram_totals: HashMap<u64, u64>,
    unigram_total: u64,
    built_unixtime: u64,
    /// Set when an unreadable file was found: learning and saving stay off so
    /// the user's bytes are left for inspection until an explicit clear.
    write_blocked: bool,
    dirty: bool,
    last_save: Option<Instant>,
}

#[derive(Debug, Default)]
struct Inner {
    counts: Counts,
    load_error: Option<String>,
}

/// The per-user char n-gram model: count tables plus the file they came from.
///
/// Interior mutability (one mutex) keeps `Arc<UserNgram>` shareable between
/// the engine, the sessions and the preferences queries; the FFI serializes
/// every engine call anyway, so the lock is uncontended.
pub struct UserNgram {
    path: PathBuf,
    inner: Mutex<Inner>,
}

impl fmt::Debug for UserNgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.lock();
        f.debug_struct("UserNgram")
            .field("path", &self.path)
            .field("unigrams", &inner.counts.unigrams.len())
            .field("bigrams", &inner.counts.bigrams.len())
            .field("trigrams", &inner.counts.trigrams.len())
            .field("load_error", &inner.load_error)
            .finish()
    }
}

impl UserNgram {
    /// An empty model that will save to `path`.
    #[must_use]
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), inner: Mutex::new(Inner::default()) }
    }

    /// Read `path`, or start empty when it does not exist. An unreadable or
    /// foreign file yields an empty model with the reason recorded and writes
    /// blocked (see [`Self::load_error`]).
    #[must_use]
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let model = Self { path, inner: Mutex::new(Inner::default()) };
        let bytes = match fs::read(model.path()) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return model,
            Err(e) => {
                {
                    let mut inner = model.lock();
                    inner.load_error = Some(format!("io error: {e}"));
                    inner.counts.write_blocked = true;
                }
                return model;
            }
        };
        match read_image(&bytes) {
            Ok(parsed) => {
                {
                    let mut inner = model.lock();
                    inner.counts = parsed;
                    inner.load_error = None;
                }
            }
            Err(e) => {
                {
                    let mut inner = model.lock();
                    inner.load_error = Some(e);
                    inner.counts.write_blocked = true;
                }
            }
        }
        model
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The file this model saves to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Why the file could not be read, if it could not.
    #[must_use]
    pub fn load_error(&self) -> Option<String> {
        self.lock().load_error.clone()
    }

    /// Whether any count was learned (or loaded).
    #[must_use]
    pub fn has_data(&self) -> bool {
        let inner = self.lock();
        !inner.counts.unigrams.is_empty()
    }

    /// Whether writes are blocked because the file was unreadable.
    #[must_use]
    pub fn write_blocked(&self) -> bool {
        self.lock().counts.write_blocked
    }

    /// Current statistics and the file's on-disk state.
    #[must_use]
    pub fn info(&self) -> UserNgramInfo {
        let inner = self.lock();
        let counts = &inner.counts;
        let metadata = fs::metadata(&self.path).ok();
        UserNgramInfo {
            format: "HFNGMF v1".to_string(),
            storage: "counts".to_string(),
            path: self.path.display().to_string(),
            exists: metadata.is_some(),
            file_bytes: metadata.as_ref().map_or(0, fs::Metadata::len),
            modified_secs: metadata
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|when| when.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64),
            unigrams: counts.unigrams.len(),
            bigrams: counts.bigrams.len(),
            trigrams: counts.trigrams.len(),
            vocab: counts.unigrams.len() + RESERVED_IDS as usize,
            unigram_total: counts.unigram_total,
            built_unixtime: counts.built_unixtime,
            error: inner.load_error.clone(),
        }
    }

    // ---- learning ----

    /// Record one committed `text`, seeded with the last two characters
    /// committed earlier in the same session (`context`; empty at the start of
    /// a session). Returns whether anything was recorded.
    ///
    /// The text is skipped unless it carries a Han character. Characters the
    /// format cannot hold (out of the BMP, control code points) are skipped
    /// but keep their place in the chain.
    pub fn record(&self, text: &str, context: &str) -> bool {
        if !text.chars().any(|ch| is_han(ch as u32)) {
            return false;
        }
        let mut inner = self.lock();
        if inner.counts.write_blocked {
            return false;
        }
        let context: Vec<u32> = {
            let chars: Vec<char> = context.chars().collect();
            chars[chars.len().saturating_sub(2)..]
                .iter()
                .filter_map(|ch| learnable(*ch))
                .collect()
        };
        let mut prev2 = if context.len() == 2 { context[0] } else { CP_BOS };
        let mut prev1 = context.last().copied().unwrap_or(CP_BOS);
        let counts = &mut inner.counts;
        for target in text.chars().filter_map(learnable) {
            *counts.unigrams.entry(target).or_insert(0) += 1;
            counts.unigram_total += 1;
            *counts.bigrams.entry(pack2(prev1, target)).or_insert(0) += 1;
            *counts.bigram_totals.entry(prev1).or_insert(0) += 1;
            *counts.trigrams.entry(pack3(prev2, prev1, target)).or_insert(0) += 1;
            *counts.trigram_totals.entry(pack2(prev2, prev1)).or_insert(0) += 1;
            prev2 = prev1;
            prev1 = target;
        }
        counts.dirty = true;
        counts.enforce_capacity();
        true
    }

    /// The user model's log-probability for `target`, interpolated with the
    /// default model's `base` log-probability and unigram. `None` when the
    /// user model has nothing to say about this context, in which case the
    /// caller keeps `base` unchanged.
    #[must_use]
    pub fn blended_logp(
        &self,
        base: f64,
        prev2: u32,
        prev1: u32,
        target: u32,
        general_unigram: f64,
    ) -> Option<f64> {
        let inner = self.lock();
        let counts = &inner.counts;
        if counts.unigram_total == 0 {
            return None;
        }
        let total_bi = *counts.bigram_totals.get(&prev1)?;
        if total_bi == 0 {
            return None;
        }
        let alpha = ALPHA_MAX.min(total_bi as f64 / (total_bi as f64 + CONTEXT_K));
        if alpha <= 0.0 {
            return None;
        }
        let general = general_unigram.max(1e-300);
        let uni = *counts.unigrams.get(&target).unwrap_or(&0) as f64;
        let p_uni = (uni + UNIGRAM_K * general)
            / (counts.unigram_total as f64 + UNIGRAM_K);
        let bi = *counts.bigrams.get(&pack2(prev1, target)).unwrap_or(&0) as f64;
        let p_bi = (bi + BIGRAM_DISCOUNT * p_uni) / (total_bi as f64 + BIGRAM_DISCOUNT);
        let total_tri = *counts.trigram_totals.get(&pack2(prev2, prev1)).unwrap_or(&0) as f64;
        let tri = *counts.trigrams.get(&pack3(prev2, prev1, target)).unwrap_or(&0) as f64;
        let p_user = (tri + TRIGRAM_DISCOUNT * p_bi) / (total_tri + TRIGRAM_DISCOUNT);
        let mixed = (1.0 - alpha) * base.exp() + alpha * p_user;
        Some(mixed.max(1e-300).ln())
    }

    // ---- persistence ----

    /// Save the model now. A blocked model reports the reason instead of
    /// overwriting the unreadable file.
    pub fn save(&self) -> Result<(), String> {
        let mut inner = self.lock();
        if inner.counts.write_blocked {
            return Err(inner
                .load_error
                .clone()
                .unwrap_or_else(|| "the model file is unreadable; clear it to start over".into()));
        }
        let image = inner.counts.image();
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("create {dir:?}: {e}"))?;
        }
        let temporary = self.path.with_extension("hng.tmp");
        fs::write(&temporary, &image).map_err(|e| format!("write {temporary:?}: {e}"))?;
        fs::rename(&temporary, &self.path).map_err(|e| format!("rename to {:?}: {e}", self.path))?;
        inner.counts.dirty = false;
        inner.counts.last_save = Some(Instant::now());
        inner.counts.built_unixtime = now_secs();
        Ok(())
    }

    /// Save only when the last save is old enough, so typing does not write a
    /// file per commit.
    pub fn save_if_due(&self) {
        let due = {
            let inner = self.lock();
            inner.counts.dirty
                && inner
                    .counts
                    .last_save
                    .is_none_or(|last| last.elapsed() >= SAVE_INTERVAL)
        };
        if due {
            if let Err(e) = self.save() {
                eprintln!("hime-core: user model save failed: {e}");
            }
        }
    }

    /// Save when anything changed, without the throttle. Called when a session
    /// ends or the scheme changes so no learned text is left only in memory.
    pub fn flush(&self) {
        let dirty = self.lock().counts.dirty;
        if !dirty {
            return;
        }
        if let Err(e) = self.save() {
            eprintln!("hime-core: user model save failed: {e}");
        }
    }

    // ---- management ----

    /// List entries of one table, sorted by count (descending) then key.
    #[must_use]
    pub fn query(&self, request: &UserNgramQuery) -> UserNgramQueryResult {
        let inner = self.lock();
        let counts = &inner.counts;
        let needle = request.query.trim();
        let matches = |text: &str| needle.is_empty() || text.contains(needle);
        let mut entries: Vec<UserNgramEntry> = Vec::new();
        match request.scope {
            UserNgramScope::Char => {
                for (&cp, &count) in &counts.unigrams {
                    let text = cp_string(cp);
                    if !matches(&text) {
                        continue;
                    }
                    entries.push(UserNgramEntry {
                        scope: UserNgramScope::Char,
                        key: format!("char:{cp}"),
                        prev2: String::new(),
                        prev: String::new(),
                        text,
                        count,
                        total: counts.unigram_total,
                    });
                }
            }
            UserNgramScope::Bigram => {
                for (&key, &count) in &counts.bigrams {
                    let (prev, target) = unpack2(key);
                    let prev_text = cp_string(prev);
                    let text = cp_string(target);
                    if !matches(&format!("{prev_text}{text}")) {
                        continue;
                    }
                    entries.push(UserNgramEntry {
                        scope: UserNgramScope::Bigram,
                        key: format!("bigram:{prev},{target}"),
                        prev2: String::new(),
                        prev: prev_text,
                        text,
                        count,
                        total: *counts.bigram_totals.get(&prev).unwrap_or(&0),
                    });
                }
            }
            UserNgramScope::Trigram => {
                for (&key, &count) in &counts.trigrams {
                    let (older, prev, target) = unpack3(key);
                    let older_text = cp_string(older);
                    let prev_text = cp_string(prev);
                    let text = cp_string(target);
                    if !matches(&format!("{older_text}{prev_text}{text}")) {
                        continue;
                    }
                    entries.push(UserNgramEntry {
                        scope: UserNgramScope::Trigram,
                        key: format!("trigram:{older},{prev},{target}"),
                        prev2: older_text,
                        prev: prev_text,
                        text,
                        count,
                        total: *counts.trigram_totals.get(&key2(older, prev)).unwrap_or(&0),
                    });
                }
            }
        }
        entries.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
        let total = entries.len();
        let entries = entries
            .into_iter()
            .skip(request.offset)
            .take(request.limit())
            .collect();
        UserNgramQueryResult {
            scope: request.scope,
            total,
            entries,
            info: UserNgramInfo {
                format: "HFNGMF v1".to_string(),
                storage: "counts".to_string(),
                path: self.path.display().to_string(),
                exists: fs::metadata(&self.path).is_ok(),
                file_bytes: fs::metadata(&self.path).map_or(0, |m| m.len()),
                modified_secs: fs::metadata(&self.path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|when| when.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64),
                unigrams: counts.unigrams.len(),
                bigrams: counts.bigrams.len(),
                trigrams: counts.trigrams.len(),
                vocab: counts.unigrams.len() + RESERVED_IDS as usize,
                unigram_total: counts.unigram_total,
                built_unixtime: counts.built_unixtime,
                error: inner.load_error.clone(),
            },
        }
    }

    /// Apply one edit and persist immediately. Returns false when the request
    /// names nothing usable (an unknown scope is impossible; a bad character
    /// or a blocked model is not).
    pub fn edit(&self, edit: &UserNgramEdit) -> Result<(), String> {
        let mut inner = self.lock();
        if edit.action == UserNgramAction::Clear {
            inner.counts = Counts::default();
            inner.load_error = None;
            inner.counts.dirty = true;
            drop(inner);
            return self.save();
        }
        if inner.counts.write_blocked {
            return Err(inner
                .load_error
                .clone()
                .unwrap_or_else(|| "the model file is unreadable; clear it to start over".into()));
        }
        // Parse the request into a canonical key.
        let parse = |text: &str| -> Result<u32, String> {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => learnable(ch)
                    .ok_or_else(|| format!("character {ch:?} cannot be stored")),
                _ => Err(format!("{text:?} is not exactly one character")),
            }
        };
        let context = |text: &str| -> Result<u32, String> {
            if text.is_empty() {
                return Ok(CP_BOS);
            }
            parse(text)
        };
        let counts = &mut inner.counts;
        match edit.scope {
            UserNgramScope::Char => {
                let target = parse(&edit.text)?;
                apply_count(&mut counts.unigrams, target, &mut counts.unigram_total, edit);
            }
            UserNgramScope::Bigram => {
                let prev = context(&edit.prev)?;
                let target = parse(&edit.text)?;
                let key = pack2(prev, target);
                let delta = set_count(&mut counts.bigrams, key, edit);
                if delta != 0 {
                    adjust_u32(&mut counts.bigram_totals, prev, delta);
                }
            }
            UserNgramScope::Trigram => {
                let older = context(&edit.prev2)?;
                let prev = context(&edit.prev)?;
                let target = parse(&edit.text)?;
                let key = pack3(older, prev, target);
                let delta = set_count(&mut counts.trigrams, key, edit);
                if delta != 0 {
                    adjust(&mut counts.trigram_totals, key2(older, prev), delta);
                }
            }
        }
        counts.dirty = true;
        counts.enforce_capacity();
        drop(inner);
        self.save()
    }
}

/// `pack2` under a name that reads as a trigram context key.
#[inline]
fn key2(a: u32, b: u32) -> u64 {
    pack2(a, b)
}

/// Add `delta` to a total map entry, removing it when it reaches zero.
fn adjust(totals: &mut HashMap<u64, u64>, key: u64, delta: i64) {
    let next = totals.get(&key).copied().unwrap_or(0).saturating_add_signed(delta);
    if next == 0 {
        totals.remove(&key);
    } else {
        totals.insert(key, next);
    }
}

fn adjust_u32(totals: &mut HashMap<u32, u64>, key: u32, delta: i64) {
    let next = totals.get(&key).copied().unwrap_or(0).saturating_add_signed(delta);
    if next == 0 {
        totals.remove(&key);
    } else {
        totals.insert(key, next);
    }
}

/// Apply an edit to the unigram table and its total; returns whether anything
/// moved.
fn apply_count(
    table: &mut HashMap<u32, u64>,
    key: u32,
    total: &mut u64,
    edit: &UserNgramEdit,
) -> bool {
    let before = table.get(&key).copied().unwrap_or(0);
    let after = match edit.action {
        UserNgramAction::Add => before.saturating_add(edit.count.max(1)),
        UserNgramAction::Set => edit.count,
        UserNgramAction::Delete => 0,
        UserNgramAction::Clear => return false,
    };
    if after == before {
        return false;
    }
    if after == 0 {
        table.remove(&key);
    } else {
        table.insert(key, after);
    }
    *total = total.saturating_add_signed(after as i64 - before as i64);
    true
}

/// Apply an edit to a count table; returns the count delta.
fn set_count(table: &mut HashMap<u64, u64>, key: u64, edit: &UserNgramEdit) -> i64 {
    let before = table.get(&key).copied().unwrap_or(0);
    let after = match edit.action {
        UserNgramAction::Add => before.saturating_add(edit.count.max(1)),
        UserNgramAction::Set => edit.count,
        UserNgramAction::Delete => 0,
        UserNgramAction::Clear => return 0,
    };
    if after == before {
        return 0;
    }
    if after == 0 {
        table.remove(&key);
    } else {
        table.insert(key, after);
    }
    after as i64 - before as i64
}

impl Counts {
    /// Evict the lowest-count entries when a table outgrows its cap, keeping
    /// the context totals consistent.
    fn enforce_capacity(&mut self) {
        if self.unigrams.len() > MAX_UNIGRAMS {
            let excess = self.unigrams.len() - MAX_UNIGRAMS;
            let mut ranked: Vec<(u64, u32)> =
                self.unigrams.iter().map(|(&cp, &count)| (count, cp)).collect();
            ranked.sort_unstable();
            for (count, cp) in ranked.into_iter().take(excess) {
                self.unigrams.remove(&cp);
                self.unigram_total = self.unigram_total.saturating_sub(count);
            }
        }
        if self.bigrams.len() > MAX_BIGRAMS {
            let excess = self.bigrams.len() - MAX_BIGRAMS;
            let mut ranked: Vec<(u64, u64)> =
                self.bigrams.iter().map(|(&key, &count)| (count, key)).collect();
            ranked.sort_unstable();
            for (count, key) in ranked.into_iter().take(excess) {
                self.bigrams.remove(&key);
                let (prev, _) = unpack2(key);
                adjust_u32(&mut self.bigram_totals, prev, -(count as i64));
            }
        }
        if self.trigrams.len() > MAX_TRIGRAMS {
            let excess = self.trigrams.len() - MAX_TRIGRAMS;
            let mut ranked: Vec<(u64, u64)> =
                self.trigrams.iter().map(|(&key, &count)| (count, key)).collect();
            ranked.sort_unstable();
            for (count, key) in ranked.into_iter().take(excess) {
                self.trigrams.remove(&key);
                let (older, prev, _) = unpack3(key);
                adjust(&mut self.trigram_totals, key2(older, prev), -(count as i64));
            }
        }
    }

    /// Serialize the counts as an HFNGMF v1 image (SPEC §7.7).
    fn image(&mut self) -> Vec<u8> {
        self.built_unixtime = now_secs();
        let vocab = self.vocabulary();
        let ids: HashMap<u32, u16> = vocab
            .iter()
            .enumerate()
            .map(|(index, &cp)| (cp, index as u16))
            .collect();

        // Bigram rows grouped by prev1, successors ascending by target id.
        let mut bi_groups: HashMap<u16, Vec<(u16, u16)>> = HashMap::new();
        for (&key, &count) in &self.bigrams {
            let (prev, target) = unpack2(key);
            let (Some(&prev_id), Some(&target_id)) = (ids.get(&prev), ids.get(&target)) else {
                continue;
            };
            bi_groups
                .entry(prev_id)
                .or_default()
                .push((target_id, encode_count(count)));
        }
        // Trigram rows grouped by prev1, then prev2.
        let mut tri_groups: HashMap<u16, Vec<(u16, Vec<(u16, u16)>)>> = HashMap::new();
        for (&key, &count) in &self.trigrams {
            let (older, prev, target) = unpack3(key);
            let (Some(&older_id), Some(&prev_id), Some(&target_id)) =
                (ids.get(&older), ids.get(&prev), ids.get(&target))
            else {
                continue;
            };
            let bucket = tri_groups.entry(prev_id).or_default();
            let position = match bucket.iter().position(|(older, _)| *older == older_id) {
                Some(position) => position,
                None => {
                    bucket.push((older_id, Vec::new()));
                    bucket.len() - 1
                }
            };
            bucket[position].1.push((target_id, encode_count(count)));
        }

        // Row bodies: cold regions, 4-byte aligned rows.
        let mut bi_rows: Vec<u8> = Vec::new();
        let mut bi_index = vec![0u32; vocab.len()];
        let mut bi_row_count = 0u32;
        let mut bi_ids: Vec<u16> = bi_groups.keys().copied().collect();
        bi_ids.sort_unstable();
        for prev_id in bi_ids {
            let mut successors = bi_groups.remove(&prev_id).unwrap_or_default();
            if successors.is_empty() {
                continue;
            }
            successors.sort_unstable_by_key(|(target, _)| *target);
            successors.dedup_by_key(|(target, _)| *target);
            let total = *self.bigram_totals.get(&u32::from(prev_id)).unwrap_or(&0);
            let reference = write_row(&mut bi_rows, &successors, encode_count(total));
            bi_index[usize::from(prev_id)] = reference;
            bi_row_count += 1;
        }

        let mut tri_rows: Vec<u8> = Vec::new();
        let mut tri_keys: Vec<u16> = Vec::new();
        let mut tri_recs: Vec<u32> = Vec::new();
        let mut tri_dir = vec![0u32; vocab.len() + 1];
        let mut tri_row_count = 0u32;
        let mut prev1_ids: Vec<u16> = tri_groups.keys().copied().collect();
        prev1_ids.sort_unstable();
        for prev1 in prev1_ids {
            let mut bucket = tri_groups.remove(&prev1).unwrap_or_default();
            bucket.sort_unstable_by_key(|(older, _)| *older);
            for (older, successors) in bucket {
                let mut successors = successors;
                successors.sort_unstable_by_key(|(target, _)| *target);
                successors.dedup_by_key(|(target, _)| *target);
                let total = *self
                    .trigram_totals
                    .get(&key2(u32::from(older), u32::from(prev1)))
                    .unwrap_or(&0);
                let reference = write_row(&mut tri_rows, &successors, encode_count(total));
                tri_keys.push(older);
                tri_recs.push(reference);
                tri_row_count += 1;
            }
            tri_dir[usize::from(prev1) + 1] = tri_keys.len() as u32;
        }
        // Empty buckets inherit the previous row count so the directory stays
        // monotone for the reader.
        for index in 1..tri_dir.len() {
            tri_dir[index] = tri_dir[index].max(tri_dir[index - 1]);
        }

        // Section layout (SPEC §2): every fixed-length section is padded to
        // the 64-byte alignment, rows are packed back to back.
        const HOT: usize = 3;
        let cpmap_len = 0x10000 * 2;
        let uni_len = vocab.len() * 2;
        let bi_index_len = vocab.len() * 4;
        let tri_dir_len = (vocab.len() + 1) * 4;
        let hot_tri_len = HOT * HOT * 4;
        let tri_keys_len = tri_keys.len() * 2;
        let tri_recs_len = tri_recs.len() * 4;
        let meta = format!(
            "{{\"hime-kind\":\"user-ngram\",\"format\":\"HFNGMF v1\",\"storage\":\"counts\",\"built_unixtime\":{},\"unigrams\":{},\"bigrams\":{},\"trigrams\":{},\"vocab\":{},\"unigram_total\":{}}}",
            self.built_unixtime,
            self.unigrams.len(),
            self.bigrams.len(),
            self.trigrams.len(),
            vocab.len(),
            self.unigram_total,
        );
        let meta_len = meta.len();
        let meta_padded = align64(meta_len);
        let vocab_len = vocab.len() * 4;

        let mut sect = [0usize; 13];
        let mut cursor = crate::ngram::HEADER_SIZE;
        for (index, length) in [
            cpmap_len,
            uni_len,
            bi_index_len,
            tri_dir_len,
            hot_tri_len,
            0, // bi_rows_hot
            0, // tri_rows_hot
            bi_rows.len(),
            tri_keys_len,
            tri_recs_len,
            tri_rows.len(),
            meta_padded,
            vocab_len,
        ]
        .into_iter()
        .enumerate()
        {
            sect[index] = cursor;
            cursor = align64(cursor + length);
        }
        let file_size = cursor;

        let mut out = vec![0u8; file_size];
        out[0..8].copy_from_slice(crate::ngram::MAGIC);
        put_u32(&mut out, 8, 1);
        put_u32(&mut out, 12, crate::ngram::HEADER_SIZE as u32);
        put_u64(&mut out, 16, file_size as u64);
        put_u32(&mut out, 24, crate::ngram::ALIGN as u32);
        put_u32(&mut out, 28, 0);
        put_u32(&mut out, 32, vocab.len() as u32);
        put_u32(&mut out, 36, HOT as u32);
        put_u32(&mut out, 40, bi_row_count);
        put_u32(&mut out, 44, tri_row_count);
        out[48] = 3;
        out[49] = 16; // successor codes are BF16 counts
        out[50] = 0; // floor_octave
        out[51] = 0; // code_range_octave
        put_u16(&mut out, 52, 1);
        put_u16(&mut out, 54, 2);
        for (index, start) in sect.iter().enumerate() {
            put_u64(&mut out, 56 + 8 * index, *start as u64);
        }
        put_u64(&mut out, 160, sect[6] as u64); // hot_end: the hot row partition is empty
        put_u64(&mut out, 168, meta_len as u64);
        let bucket_max = tri_dir
            .windows(2)
            .map(|w| w[1].saturating_sub(w[0]))
            .max()
            .unwrap_or(0);
        put_u32(&mut out, 192, bucket_max);
        put_u64(&mut out, 208, self.unigram_total);
        put_u64(&mut out, 224, self.built_unixtime);
        let build_id = b"hime-user";
        out[232..232 + build_id.len()].copy_from_slice(build_id);

        // 1. cpmap
        for value in out[sect[0]..sect[0] + cpmap_len].chunks_exact_mut(2) {
            value.copy_from_slice(&0xFFFFu16.to_le_bytes());
        }
        for (index, &cp) in vocab.iter().enumerate().skip(1) {
            let offset = sect[0] + cp as usize * 2;
            put_u16(&mut out, offset, index as u16);
        }
        // 2. unigram counts (a context-only character keeps 0, so a reload
        // does not invent a usage it never had)
        for (index, &cp) in vocab.iter().enumerate() {
            let count = self.unigrams.get(&cp).copied().unwrap_or(0);
            let bits = if count == 0 { 0 } else { f64_to_bf16(count as f64) };
            put_u16(&mut out, sect[1] + index * 2, bits);
        }
        // 3. bigram row references
        for (index, reference) in bi_index.iter().enumerate() {
            put_u32(&mut out, sect[2] + index * 4, *reference);
        }
        // 4. trigram bucket directory
        for (index, start) in tri_dir.iter().enumerate() {
            put_u32(&mut out, sect[3] + index * 4, *start);
        }
        // 6./7. hot partitions stay empty; 8. bigram rows
        out[sect[7]..sect[7] + bi_rows.len()].copy_from_slice(&bi_rows);
        // 9./10. trigram bucket keys and references
        for (index, key) in tri_keys.iter().enumerate() {
            put_u16(&mut out, sect[8] + index * 2, *key);
        }
        for (index, reference) in tri_recs.iter().enumerate() {
            put_u32(&mut out, sect[9] + index * 4, *reference);
        }
        // 11. trigram rows
        out[sect[10]..sect[10] + tri_rows.len()].copy_from_slice(&tri_rows);
        // 12. meta
        out[sect[11]..sect[11] + meta_len].copy_from_slice(meta.as_bytes());
        // 13. id → code point
        for (index, &cp) in vocab.iter().enumerate() {
            put_u32(&mut out, sect[12] + index * 4, cp);
        }
        // The hot matrix is normally empty, but a trigram context whose two
        // preceding ids are both below HOT can only be reached through it.
        let hot_base = sect[4];
        let mut bucket = 0usize;
        let vocab_len = vocab.len();
        for (index, reference) in tri_recs.iter().enumerate() {
            while bucket + 1 < vocab_len && get_u32(&out, sect[3] + (bucket + 1) * 4) <= index as u32
            {
                bucket += 1;
            }
            if *reference == 0 {
                continue;
            }
            let older = tri_keys[index];
            let prev1 = bucket as u16;
            if usize::from(older) < HOT && usize::from(prev1) < HOT {
                let offset = hot_base + (usize::from(prev1) * HOT + usize::from(older)) * 4;
                put_u32(&mut out, offset, *reference);
            }
        }

        self.dirty = false;
        out
    }

    /// The id → code point table: ids 0/1/2 are UNK/BOS/EOS, the rest are the
    /// learned characters by descending count.
    fn vocabulary(&self) -> Vec<u32> {
        let mut chars: HashMap<u32, u64> = HashMap::new();
        for (&cp, &count) in &self.unigrams {
            chars.insert(cp, count);
        }
        // BOS/EOS appear as contexts, but they already own ids 1 and 2.
        let mut note = |cp: u32| {
            if cp != CP_BOS && cp != CP_EOS {
                chars.entry(cp).or_insert(0);
            }
        };
        for &key in self.bigrams.keys() {
            let (prev, target) = unpack2(key);
            note(prev);
            note(target);
        }
        for &key in self.trigrams.keys() {
            let (older, prev, target) = unpack3(key);
            note(older);
            note(prev);
            note(target);
        }
        let mut ranked: Vec<(u32, u64)> = chars.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        ranked.truncate(MAX_VOCAB - RESERVED_IDS as usize);
        let mut vocab = vec![0u32, CP_BOS, CP_EOS];
        vocab.extend(ranked.into_iter().map(|(cp, _)| cp));
        vocab
    }
}

/// Write one row and return its reference (1-based offset into the region).
fn write_row(rows: &mut Vec<u8>, successors: &[(u16, u16)], total: u16) -> u32 {
    let offset = rows.len();
    let count = successors.len();
    if count > 0x7F {
        rows.push(0x80);
    } else {
        rows.push(count as u8);
    }
    rows.push(0); // mode: RAW
    rows.extend_from_slice(&total.to_le_bytes());
    rows.extend_from_slice(&0u16.to_le_bytes()); // scale (16-bit codes ignore it)
    rows.extend_from_slice(&if count > 0x7F { count as u16 } else { 0 }.to_le_bytes());
    for (id, _) in successors {
        rows.extend_from_slice(&id.to_le_bytes());
    }
    for (_, code) in successors {
        rows.extend_from_slice(&code.to_le_bytes());
    }
    offset as u32 + 1
}

/// Counts are BF16; 0 is written as the smallest positive BF16 so a stored
/// successor never reads back as absent.
fn encode_count(count: u64) -> u16 {
    if count == 0 {
        return 1;
    }
    f64_to_bf16(count as f64)
}

fn align64(value: usize) -> usize {
    (value + crate::ngram::ALIGN - 1) / crate::ngram::ALIGN * crate::ngram::ALIGN
}

fn put_u16(out: &mut [u8], offset: usize, value: u16) {
    out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut [u8], offset: usize, value: u64) {
    out[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

/// BF16 bit pattern → count (rounded to the nearest integer).
fn decode_count(bits: u16) -> u64 {
    let value = f32::from_bits(u32::from(bits) << 16);
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    value.round() as u64
}

// ---- reading ----

/// Read an HFNGMF v1 user-model image back into counts.
fn read_image(bytes: &[u8]) -> Result<Counts, String> {
    if bytes.len() < crate::ngram::HEADER_SIZE {
        return Err("file is too short to be HFNGMF v1".into());
    }
    if &bytes[0..8] != crate::ngram::MAGIC {
        return Err("not an HFNGMF v1 (.hng) file".into());
    }
    if get_u32(bytes, 8) != 1 {
        return Err("HFNGMF version is not 1".into());
    }
    if get_u32(bytes, 12) as usize != crate::ngram::HEADER_SIZE {
        return Err("HFNGMF header size is not 256".into());
    }
    if get_u64(bytes, 16) as usize != bytes.len() {
        return Err("declared file size does not match the file".into());
    }
    if bytes[48] != 3 {
        return Err("HFNGMF order is not 3".into());
    }
    let vocab = get_u32(bytes, 32) as usize;
    if !(RESERVED_IDS as usize..=0xFFFF).contains(&vocab) {
        return Err(format!("invalid vocabulary size {vocab}"));
    }
    let mut sect = [0usize; 13];
    for (index, slot) in sect.iter_mut().enumerate() {
        *slot = get_u64(bytes, 56 + index * 8) as usize;
    }
    for index in 0..13 {
        let end = if index + 1 < 13 { sect[index + 1] } else { bytes.len() };
        if sect[index] < crate::ngram::HEADER_SIZE
            || sect[index] % crate::ngram::ALIGN != 0
            || sect[index] > end
            || end > bytes.len()
        {
            return Err(format!("section {index} is out of bounds"));
        }
    }
    let meta_len = get_u64(bytes, 168) as usize;
    if sect[11] + meta_len > bytes.len() {
        return Err("meta section is out of bounds".into());
    }
    let meta = String::from_utf8_lossy(&bytes[sect[11]..sect[11] + meta_len]);
    if !meta.contains(USER_NGRAM_MARKER) {
        return Err("not a Hime user n-gram model (missing its meta marker)".into());
    }

    let mut counts = Counts {
        built_unixtime: get_u64(bytes, 224),
        ..Counts::default()
    };
    let ids: Vec<u32> = (0..vocab).map(|id| get_u32(bytes, sect[12] + id * 4)).collect();

    // Unigrams (ids 1/2 are BOS/EOS, never targets).
    for (id, &cp) in ids.iter().enumerate().skip(RESERVED_IDS as usize) {
        let count = decode_count(get_u16(bytes, sect[1] + id * 2));
        if count > 0 && cp != 0 {
            counts.unigrams.insert(cp, count);
            counts.unigram_total += count;
        }
    }

    // Bigram rows: bi_index[id] → cold row.
    let bi_cold = sect[7];
    for (id, &cp) in ids.iter().enumerate() {
        let reference = get_u32(bytes, sect[2] + id * 4);
        if reference == 0 {
            continue;
        }
        let (successors, total) = read_row(bytes, bi_cold, reference)?;
        let mut sum = 0u64;
        for (target_id, count) in successors {
            let Some(&target) = ids.get(usize::from(target_id)) else { continue };
            if target == 0 {
                continue;
            }
            counts.bigrams.insert(pack2(cp, target), count);
            sum += count;
        }
        counts.bigram_totals.insert(cp, total.max(sum));
    }

    // Trigram rows: one bucket per prev1, `tri_ctx_count` rows in all.
    let tri_cold = sect[10];
    let tri_ctx = get_u32(bytes, 44) as usize;
    if sect[8] + tri_ctx * 2 > sect[9] || sect[9] + tri_ctx * 4 > sect[10] {
        return Err("trigram index sections are inconsistent".into());
    }
    for prev1_id in 0..vocab {
        let lo = get_u32(bytes, sect[3] + prev1_id * 4) as usize;
        let hi = get_u32(bytes, sect[3] + (prev1_id + 1) * 4) as usize;
        if lo > hi || hi > tri_ctx {
            return Err("trigram bucket directory is inconsistent".into());
        }
        let prev1 = ids[prev1_id];
        for position in lo..hi {
            let older_id = get_u16(bytes, sect[8] + position * 2) as usize;
            let reference = get_u32(bytes, sect[9] + position * 4);
            let Some(&older) = ids.get(older_id) else { continue };
            if reference == 0 {
                continue;
            }
            let (successors, total) = read_row(bytes, tri_cold, reference)?;
            let mut sum = 0u64;
            for (target_id, count) in successors {
                let Some(&target) = ids.get(usize::from(target_id)) else { continue };
                if target == 0 {
                    continue;
                }
                counts.trigrams.insert(pack3(older, prev1, target), count);
                sum += count;
            }
            counts.trigram_totals.insert(pack2(older, prev1), total.max(sum));
        }
    }

    counts.dirty = false;
    Ok(counts)
}

/// Read one row from a cold region: its `(successors, total)`.
fn read_row(bytes: &[u8], base: usize, reference: u32) -> Result<(Vec<(u16, u64)>, u64), String> {
    if reference & 0x8000_0000 != 0 {
        return Err("a hot row reference is invalid in a user model".into());
    }
    let start = base + (reference as usize - 1);
    if start + 8 > bytes.len() {
        return Err("row starts past the end of the file".into());
    }
    let head = bytes[start];
    let extended = head & 0x80 != 0;
    let count = if extended { get_u16(bytes, start + 6) as usize } else { (head & 0x7F) as usize };
    let total = decode_count(get_u16(bytes, start + 2));
    let ids_base = start + 8;
    let codes_base = ids_base + count * 2;
    if codes_base + count * 2 > bytes.len() {
        return Err("row runs past the end of the file".into());
    }
    let mut successors = Vec::with_capacity(count);
    for index in 0..count {
        let id = get_u16(bytes, ids_base + index * 2);
        let value = decode_count(get_u16(bytes, codes_base + index * 2));
        if value > 0 {
            successors.push((id, value));
        }
    }
    Ok((successors, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hime-user-ngram-{}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("user.hng")
    }

    /// The stored counts survive a save/load round trip, including the context
    /// totals the statistics display uses.
    #[test]
    fn counts_round_trip_through_the_hng_image() {
        let path = scratch("roundtrip");
        let model = UserNgram::empty(&path);
        assert!(model.record("今天天气", ""));
        assert!(model.record("明天", "天气"));
        model.save().expect("save");
        assert!(path.is_file());

        let loaded = UserNgram::load(&path);
        assert!(loaded.load_error().is_none(), "{:?}", loaded.load_error());
        let info = loaded.info();
        assert_eq!(info.unigrams, 4, "今 天 气 明");
        assert_eq!(info.unigram_total, 6, "今天天气 + 明天 is six characters");
        assert_eq!(info.bigrams, 6, "今→天 天→气 气→明 明→天 plus BOS→今 BOS→明");
        assert_eq!(info.trigrams, 6);
        assert_eq!(info.vocab, 7, "three reserved ids plus the four characters");

        // The cross-fragment chain: 气 (the last character of the first
        // fragment) is the context of 明 in the second.
        let result = loaded.query(&UserNgramQuery {
            scope: UserNgramScope::Bigram,
            query: "气明".into(),
            limit: 10,
            offset: 0,
        });
        assert_eq!(result.total, 1, "{:?}", result.entries);
        assert_eq!(result.entries[0].count, 1);
        assert_eq!(result.entries[0].total, 1, "气 only ever preceded 明");

        let result = loaded.query(&UserNgramQuery {
            scope: UserNgramScope::Trigram,
            query: "气明天".into(),
            limit: 10,
            offset: 0,
        });
        assert_eq!(result.total, 1, "{:?}", result.entries);
        assert_eq!(result.entries[0].prev2, "气");
        assert_eq!(result.entries[0].prev, "明");
        assert_eq!(result.entries[0].count, 1);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A file the loader rejects is never overwritten; a clear wipes it and
    /// unblocks writes.
    #[test]
    fn a_broken_file_blocks_writes_until_cleared() {
        let path = scratch("blocked");
        fs::write(&path, b"junk").unwrap();
        let model = UserNgram::load(&path);
        assert!(model.write_blocked());
        assert!(!model.record("今天", ""));
        assert!(model.save().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"junk", "the user's bytes stay untouched");

        model
            .edit(&UserNgramEdit { action: UserNgramAction::Clear, ..UserNgramEdit::default() })
            .expect("clear rebuilds the file");
        assert!(!model.write_blocked());
        assert!(model.record("今天", ""));
        model.save().expect("save after clear");
        let reloaded = UserNgram::load(&path);
        assert!(reloaded.load_error().is_none(), "{:?}", reloaded.load_error());
        assert_eq!(reloaded.info().unigrams, 2);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// A default whole-sentence model is not a user model, and the
    /// whole-sentence reader refuses a user model; the two formats cannot be
    /// mixed up.
    #[test]
    fn the_two_models_reject_each_other() {
        let path = scratch("foreign");
        let model = UserNgram::empty(&path);
        assert!(model.record("今天", ""));
        model.save().unwrap();

        let default_model = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/sentence-ngram.hng");
        if Path::new(default_model).is_file() {
            let foreign = UserNgram::load(default_model);
            assert!(foreign.load_error().is_some(), "a whole-sentence model is refused");
        }
        assert!(crate::ngram::NgramModel::load(&path).is_err(), "a user model is refused");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Blending pulls a target the user actually types ahead of one they do
    /// not, and reports nothing for an unseen context.
    #[test]
    fn blending_rewards_learned_transitions() {
        let path = scratch("blend");
        let model = UserNgram::empty(&path);
        for _ in 0..20 {
            model.record("今天", "");
        }
        let prev2 = CP_BOS;
        let prev1 = '今' as u32;
        let base = -8.0;
        let learned = model
            .blended_logp(base, prev2, prev1, '天' as u32, 1e-5)
            .expect("the context was seen");
        let unknown = model
            .blended_logp(base, prev2, prev1, '气' as u32, 1e-5)
            .expect("the context was seen");
        assert!(learned > base, "the learned successor beats the default: {learned}");
        assert!(learned > unknown, "{learned} vs {unknown}");
        assert!(
            model.blended_logp(base, CP_BOS, '你' as u32, '好' as u32, 1e-5).is_none(),
            "an unseen context keeps the default score"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Add / set / delete / clear go through the same map bookkeeping.
    #[test]
    fn edits_maintain_counts_and_totals() {
        let path = scratch("edits");
        let model = UserNgram::empty(&path);
        let edit = |action, scope, prev: &str, text: &str, count| UserNgramEdit {
            action,
            scope,
            prev2: String::new(),
            prev: prev.to_string(),
            text: text.to_string(),
            count,
        };
        model
            .edit(&edit(UserNgramAction::Add, UserNgramScope::Char, "", "今", 3))
            .unwrap();
        model
            .edit(&edit(UserNgramAction::Add, UserNgramScope::Bigram, "今", "天", 2))
            .unwrap();
        let result = model.query(&UserNgramQuery { scope: UserNgramScope::Char, ..Default::default() });
        assert_eq!(result.entries[0].text, "今");
        assert_eq!(result.entries[0].count, 3);
        assert_eq!(result.entries[0].total, 3);

        model
            .edit(&edit(UserNgramAction::Set, UserNgramScope::Bigram, "今", "天", 7))
            .unwrap();
        let result = model.query(&UserNgramQuery {
            scope: UserNgramScope::Bigram,
            ..Default::default()
        });
        assert_eq!(result.entries[0].count, 7);
        assert_eq!(result.entries[0].total, 7);

        model
            .edit(&edit(UserNgramAction::Delete, UserNgramScope::Bigram, "今", "天", 0))
            .unwrap();
        let result = model.query(&UserNgramQuery {
            scope: UserNgramScope::Bigram,
            ..Default::default()
        });
        assert_eq!(result.total, 0);

        model
            .edit(&UserNgramEdit { action: UserNgramAction::Clear, ..UserNgramEdit::default() })
            .unwrap();
        assert!(!model.has_data());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    /// Texts without a Han character are not learned, and the per-session
    /// context seeds the chain only when it is given.
    #[test]
    fn non_chinese_text_is_not_learned() {
        let path = scratch("gate");
        let model = UserNgram::empty(&path);
        assert!(!model.record("abc 123", ""));
        assert!(!model.record("，。", ""));
        assert!(model.record("今天", ""));
        assert!(model.has_data());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
