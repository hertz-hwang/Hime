//! Typing statistics (SPEC §9).
//!
//! The engine records what its sessions actually send to the host: one
//! character count per committed character (punctuation, letters and digits
//! included), one key press per forwarded key, and whether each candidate
//! selection took the first entry. From those counters the preferences panel
//! renders the daily activity grid, the speed / keystroke / code-length
//! metrics, the first-choice rate and the per-scheme keyboard heat map.
//!
//! **What a run is.** A run is one uninterrupted typing stretch inside a
//! session: a gap longer than [`RUN_IDLE_GAP`] closes it, and so does a
//! session end (app switch, scheme switch, quit). Each inter-key interval is
//! capped at [`KEY_GAP_CAP`], so a pause to think never inflates the elapsed
//! time. Only a run longer than [`RUN_MIN_CHARS`] characters contributes to
//! the speed, keystroke and code-length averages — a two-character commit is
//! not a typing speed sample. Character counts themselves are recorded live
//! and count every run.
//!
//! **What counts as a character.** Every character the engine commits
//! (`commit_text`, whether a candidate, punctuation, a raw encoding or a
//! sentence fragment) plus every printable key it hands to the host (an idle
//! English letter, a released ASCII punctuation mark). A composition key such
//! as a letter that only extends the encoding is not a character.
//!
//! **Keyboard statistics are per scheme.** Each key press increments the
//! selected scheme's histogram; switching schemes starts filling another
//! histogram and leaves the previous one intact. Finger and hand splits are
//! derived from one canonical Mac keyboard map in this module, so the shell
//! draws counts and never decides which finger a key belongs to.
//!
//! Everything is persisted as one JSON file at
//! `<data-dir>/statistics/typing.json` (see [`crate::paths`]); saves are
//! throttled like the learned model's.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A run must carry more than this many characters to enter the speed,
/// keystroke and code-length averages.
pub const RUN_MIN_CHARS: u64 = 15;

/// Longest inter-key interval a run's elapsed time accounts for. Thinking
/// pauses are typing time only up to this bound.
pub const KEY_GAP_CAP: Duration = Duration::from_secs(2);

/// Idle time after which the next key starts a new run.
pub const RUN_IDLE_GAP: Duration = Duration::from_secs(30);

/// Days the activity grid covers: 53 week columns of 7 days.
pub const HEATMAP_DAYS: i64 = 371;

/// Minimum spacing between throttled saves while typing.
const SAVE_INTERVAL: Duration = Duration::from_secs(15);

/// Version of the JSON image; a file carrying another version is set aside
/// and a fresh one starts.
const IMAGE_VERSION: u32 = 1;

// ---- fingers ----

/// Where a key sits under the eight-finger touch-typing layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finger {
    LeftPinky,
    LeftRing,
    LeftMiddle,
    LeftIndex,
    Thumb,
    RightIndex,
    RightMiddle,
    RightRing,
    RightPinky,
}

impl Finger {
    /// Stable id used in JSON and by the shell's drawing code.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::LeftPinky => "l_pinky",
            Self::LeftRing => "l_ring",
            Self::LeftMiddle => "l_middle",
            Self::LeftIndex => "l_index",
            Self::Thumb => "thumb",
            Self::RightIndex => "r_index",
            Self::RightMiddle => "r_middle",
            Self::RightRing => "r_ring",
            Self::RightPinky => "r_pinky",
        }
    }

    /// Panel label.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::LeftPinky => "左小指",
            Self::LeftRing => "左无名指",
            Self::LeftMiddle => "左中指",
            Self::LeftIndex => "左食指",
            Self::Thumb => "拇指",
            Self::RightIndex => "右食指",
            Self::RightMiddle => "右中指",
            Self::RightRing => "右无名指",
            Self::RightPinky => "右小指",
        }
    }

    /// `left`, `right` or `thumb`.
    #[must_use]
    pub const fn hand(self) -> &'static str {
        match self {
            Self::LeftPinky | Self::LeftRing | Self::LeftMiddle | Self::LeftIndex => "left",
            Self::Thumb => "thumb",
            Self::RightIndex | Self::RightMiddle | Self::RightRing | Self::RightPinky => "right",
        }
    }

    /// All fingers in draw order.
    #[must_use]
    pub const fn all() -> [Self; 9] {
        [
            Self::LeftPinky,
            Self::LeftRing,
            Self::LeftMiddle,
            Self::LeftIndex,
            Self::Thumb,
            Self::RightIndex,
            Self::RightMiddle,
            Self::RightRing,
            Self::RightPinky,
        ]
    }

    /// The finger typing `key` on an ANSI Mac keyboard, by the canonical key
    /// name this module records. `None` for keys outside the main block
    /// (arrows, function keys) and for unrecorded codes.
    #[must_use]
    pub fn of(key: &str) -> Option<Self> {
        use Finger::*;
        let finger = match key {
            "Escape" | "Tab" | "`" | "~" | "1" | "!" | "q" | "Q" | "a" | "A" | "z" | "Z" => {
                LeftPinky
            }
            "2" | "@" | "w" | "W" | "s" | "S" | "x" | "X" => LeftRing,
            "3" | "#" | "e" | "E" | "d" | "D" | "c" | "C" => LeftMiddle,
            "4" | "$" | "5" | "%" | "r" | "R" | "t" | "T" | "f" | "F" | "g" | "G" | "v" | "V"
            | "b" | "B" => LeftIndex,
            "Space" => Thumb,
            "6" | "^" | "7" | "&" | "y" | "Y" | "u" | "U" | "h" | "H" | "j" | "J" | "n"
            | "N" | "m" | "M" => RightIndex,
            "8" | "*" | "i" | "I" | "k" | "K" | "," | "<" => RightMiddle,
            "9" | "(" | "o" | "O" | "l" | "L" | "." | ">" => RightRing,
            "0" | ")" | "-" | "_" | "=" | "+" | "p" | "P" | "[" | "{" | "]" | "}" | "\\"
            | "|" | ";" | ":" | "'" | "\"" | "/" | "?" | "Backspace" | "Enter" => RightPinky,
            _ => return None,
        };
        Some(finger)
    }
}

// ---- days ----

/// A local calendar date, the key of one entry of the daily counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year);
    let month = i64::from(month);
    let day = i64::from(day);
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day count since 1970-01-01.
fn civil_from_days(days: i64) -> Date {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    Date { year: year as i32, month, day }
}

impl Date {
    /// The date `offset` days away.
    #[must_use]
    pub fn add_days(self, offset: i64) -> Self {
        civil_from_days(days_from_civil(self.year, self.month, self.day) + offset)
    }

    /// `YYYY-MM-DD`, the key used in the JSON image.
    #[must_use]
    pub fn key(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    /// Days since the Unix epoch.
    #[must_use]
    pub fn epoch_day(self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    /// Day of week with Monday = 0 … Sunday = 6.
    #[must_use]
    pub fn weekday(self) -> i64 {
        (self.epoch_day() + 3).rem_euclid(7)
    }

    /// Parse the image's `YYYY-MM-DD` key.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        let mut parts = key.split('-');
        let year = parts.next()?.parse().ok()?;
        let month = parts.next()?.parse().ok()?;
        let day = parts.next()?.parse().ok()?;
        parts.next().is_none().then_some(Self { year, month, day })
    }

    /// The local date of a Unix timestamp. Falls back to UTC where no local
    /// time zone lookup exists.
    #[must_use]
    pub fn from_unix(secs: i64) -> Self {
        if let Some(parts) = local_parts(secs) {
            return parts;
        }
        civil_from_days(secs.div_euclid(86_400))
    }
}

#[cfg(unix)]
fn local_parts(secs: i64) -> Option<Date> {
    let time: libc::time_t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `time` and `tm` are valid for the call; localtime_r writes into
    // the caller's struct and does not retain either pointer.
    let result = unsafe { libc::localtime_r(&time, &mut tm) };
    if result.is_null() {
        return None;
    }
    Some(Date {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
    })
}

#[cfg(not(unix))]
fn local_parts(_secs: i64) -> Option<Date> {
    None
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Today's local date.
#[must_use]
pub fn today() -> Date {
    Date::from_unix(now_secs())
}

// ---- counters ----

/// One bucket of character, key, selection and run counters. The same shape
/// serves one day and the whole history.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Counters {
    /// Characters committed or handed to the host.
    pub chars: u64,
    /// Key presses forwarded to the engine.
    pub keys: u64,
    /// Candidate selections (Space, digits, 2nd/3rd-select, auto-commit).
    pub selections: u64,
    /// Selections that took the first candidate.
    pub preferred: u64,
    /// Runs longer than [`RUN_MIN_CHARS`] folded into this bucket.
    pub runs: u64,
    /// Characters, keys and capped milliseconds of those runs.
    pub run_chars: u64,
    pub run_keys: u64,
    pub run_millis: u64,
}

/// One day of the activity grid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DayChars {
    pub date: String,
    pub chars: u64,
}

/// Character totals for the summary rows.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CharTotals {
    pub total: u64,
    pub today: u64,
    pub week: u64,
    pub month: u64,
    pub year: u64,
}

/// Speed, keystroke and code-length averages over qualifying runs.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SpeedStats {
    pub runs: u64,
    pub chars: u64,
    pub keys: u64,
    pub millis: u64,
    /// Characters per minute over the measured runs.
    pub chars_per_min: f64,
    /// Keystrokes per second over the measured runs.
    pub keys_per_sec: f64,
    /// Keystrokes per character: the average code length.
    pub code_length: f64,
}

/// How often the first candidate was taken.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FirstChoiceStats {
    pub selections: u64,
    pub preferred: u64,
    /// `preferred / selections`, in percent.
    pub rate: f64,
}

/// One finger's share of the recorded keys.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FingerShare {
    pub id: String,
    pub name: String,
    pub hand: String,
    pub count: u64,
}

/// The keyboard heat map and its hand / finger splits for one scheme.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KeyboardStats {
    /// Scheme folder the histogram belongs to; empty before a scheme loads.
    pub scheme: String,
    /// Key presses attributed to a finger.
    pub total: u64,
    pub left: u64,
    pub right: u64,
    pub thumb: u64,
    /// `100 - |left - right| / (left + right) * 100`; thumbs excluded.
    pub balance: f64,
    /// Canonical key name → press count.
    pub keys: BTreeMap<String, u64>,
    /// Per-finger counts in draw order.
    pub fingers: Vec<FingerShare>,
}

/// Everything the preferences panel renders, in one pass.
#[derive(Debug, Clone, Serialize)]
pub struct TypingStatsInfo {
    pub path: String,
    pub exists: bool,
    pub file_bytes: u64,
    pub modified_secs: Option<i64>,
    pub active_scheme: String,
    /// Why the file was unreadable; absent after a clean load.
    pub error: Option<String>,
    /// Earliest day with any recorded character.
    pub first_day: Option<String>,
    pub chars: CharTotals,
    pub speed: SpeedStats,
    pub first_choice: FirstChoiceStats,
    /// Keyboard statistics of the selected scheme.
    pub keyboard: KeyboardStats,
    /// Keyboard statistics of every scheme with recorded presses, sorted by
    /// folder name.
    pub keyboards: Vec<KeyboardStats>,
    /// Character counts of the last [`HEATMAP_DAYS`] days, oldest first.
    pub days: Vec<DayChars>,
}

// ---- run meter ----

/// Timing and character counters of one uninterrupted typing stretch. Owned
/// by a session; folded into the statistics when the stretch ends.
#[derive(Debug, Clone, Copy)]
pub struct RunMeter {
    last: Instant,
    keys: u64,
    chars: u64,
    millis: u64,
}

impl RunMeter {
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self { last: now, keys: 0, chars: 0, millis: 0 }
    }

    /// Account for one key press: `keys` increments and the interval since
    /// the previous press, capped at [`KEY_GAP_CAP`], joins the elapsed time.
    pub fn press(&mut self, now: Instant) {
        let gap = now.saturating_duration_since(self.last).as_millis();
        self.millis += gap.min(KEY_GAP_CAP.as_millis()) as u64;
        self.last = now;
        self.keys += 1;
    }

    /// Account for characters committed or released since the last key.
    pub fn add_chars(&mut self, chars: u64) {
        self.chars += chars;
    }

    /// Whether a pause longer than [`RUN_IDLE_GAP`] separates `now` from the
    /// last press, i.e. the next key starts a new run.
    #[must_use]
    pub fn idle_for(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last) > RUN_IDLE_GAP
    }

    /// Whether the run carries enough characters to be a metrics sample.
    #[must_use]
    pub fn qualifies(&self) -> bool {
        self.chars > RUN_MIN_CHARS
    }

    #[must_use]
    pub fn chars(&self) -> u64 {
        self.chars
    }

    #[must_use]
    pub fn keys(&self) -> u64 {
        self.keys
    }

    #[must_use]
    pub fn millis(&self) -> u64 {
        self.millis
    }
}

// ---- persisted image ----

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct Image {
    version: u32,
    daily: BTreeMap<String, Counters>,
    overall: Counters,
    /// Scheme folder → canonical key name → presses.
    schemes: BTreeMap<String, BTreeMap<String, u64>>,
}

impl Default for Image {
    fn default() -> Self {
        Self {
            version: IMAGE_VERSION,
            daily: BTreeMap::new(),
            overall: Counters::default(),
            schemes: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    image: Image,
    error: Option<String>,
    dirty: bool,
    last_save: Option<Instant>,
}

/// The user's typing statistics: counters in memory, one JSON file behind
/// them. Shared like the learned model, with interior mutability; every
/// caller through the FFI is serialized by the engine lock anyway.
pub struct TypingStats {
    path: Option<PathBuf>,
    active_scheme: Mutex<String>,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for TypingStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        f.debug_struct("TypingStats")
            .field("path", &self.path)
            .field("days", &inner.image.daily.len())
            .field("schemes", &inner.image.schemes.len())
            .field("error", &inner.error)
            .finish()
    }
}

impl TypingStats {
    /// Statistics that persist to `path`; an empty model when the file does
    /// not exist yet.
    #[must_use]
    pub fn load(path: Option<PathBuf>) -> Self {
        let stats = Self {
            path,
            active_scheme: Mutex::new(String::new()),
            inner: Mutex::new(Inner::default()),
        };
        stats.read_from_disk();
        stats
    }

    /// In-memory statistics with no file behind them (tests, tools).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            active_scheme: Mutex::new(String::new()),
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn scheme(&self) -> MutexGuard<'_, String> {
        self.active_scheme.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The file behind these counters, when they persist.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    fn read_from_disk(&self) {
        let Some(path) = self.path.as_ref() else { return };
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                self.lock().error = Some(format!("io error: {e}"));
                return;
            }
        };
        match serde_json::from_slice::<Image>(&bytes) {
            Ok(image) if image.version == IMAGE_VERSION => {
                let mut inner = self.lock();
                inner.image = image;
                inner.error = None;
            }
            Ok(image) => {
                // A newer or older layout: keep the bytes for inspection and
                // start a fresh image rather than misreading them.
                let aside = path.with_extension(format!("json.v{}", image.version));
                let _ = fs::rename(path, &aside);
                self.lock().error =
                    Some(format!("文件版本 {} 不受支持，已改名保留", image.version));
            }
            Err(e) => {
                let aside = path.with_extension(format!("json.bad-{}", now_secs()));
                let _ = fs::rename(path, &aside);
                self.lock().error = Some(format!("统计数据无法解析（{e}），已改名保留"));
            }
        }
    }

    /// Name the scheme whose histogram receives the next key presses. Called
    /// when a scheme is selected.
    pub fn set_scheme(&self, scheme: &str) {
        let mut current = self.scheme();
        if *current != scheme {
            *current = scheme.to_string();
        }
    }

    /// The scheme receiving key presses right now.
    #[must_use]
    pub fn active_scheme(&self) -> String {
        self.scheme().clone()
    }

    /// Record one forwarded key press. Printable letters fold to lowercase so
    /// one physical key has one bucket.
    pub fn press_key(&self, key: u32) {
        let Some(name) = key_name(key) else { return };
        let scheme = self.active_scheme();
        let mut inner = self.lock();
        inner.image.overall.keys += 1;
        *inner
            .image
            .schemes
            .entry(scheme)
            .or_default()
            .entry(name)
            .or_insert(0) += 1;
        inner.dirty = true;
    }

    /// Record committed characters; `preferred` reports a candidate choice
    /// (`Some(true)` took the first entry, `None` commits text without a
    /// candidate).
    pub fn record_commit(&self, chars: u64, preferred: Option<bool>) {
        self.record_commit_on(today(), chars, preferred);
    }

    fn record_commit_on(&self, day: Date, chars: u64, preferred: Option<bool>) {
        if chars == 0 {
            return;
        }
        let mut inner = self.lock();
        let mut day_counters = std::mem::take(inner.image.daily.entry(day.key()).or_default());
        let mut overall = inner.image.overall.clone();
        for counters in [&mut day_counters, &mut overall] {
            counters.chars += chars;
            if let Some(preferred) = preferred {
                counters.selections += 1;
                if preferred {
                    counters.preferred += 1;
                }
            }
        }
        inner.image.daily.insert(day.key(), day_counters);
        inner.image.overall = overall;
        inner.dirty = true;
        drop(inner);
        self.save_if_due();
    }

    /// Fold a finished run into the statistics. Runs of [`RUN_MIN_CHARS`]
    /// characters or fewer are not metrics samples and are dropped.
    pub fn record_run(&self, run: RunMeter) {
        if !run.qualifies() {
            return;
        }
        self.record_sample_on(today(), run.chars, run.keys, run.millis);
    }

    fn record_sample_on(&self, day: Date, chars: u64, keys: u64, millis: u64) {
        if chars <= RUN_MIN_CHARS {
            return;
        }
        let mut inner = self.lock();
        let mut day_counters = std::mem::take(inner.image.daily.entry(day.key()).or_default());
        let mut overall = inner.image.overall.clone();
        for counters in [&mut day_counters, &mut overall] {
            counters.runs += 1;
            counters.run_chars += chars;
            counters.run_keys += keys;
            counters.run_millis += millis;
        }
        inner.image.daily.insert(day.key(), day_counters);
        inner.image.overall = overall;
        inner.dirty = true;
        drop(inner);
        self.save_if_due();
    }

    /// Save now. A missing path or an I/O failure is reported, not fatal:
    /// statistics must never break typing.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = self.path.as_ref() else { return Ok(()) };
        let mut inner = self.lock();
        if !inner.dirty {
            return Ok(());
        }
        let mut image = serde_json::to_vec_pretty(&inner.image)
            .map_err(|e| format!("serialize statistics: {e}"))?;
        image.push(b'\n');
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("create {dir:?}: {e}"))?;
        }
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, &image).map_err(|e| format!("write {temporary:?}: {e}"))?;
        fs::rename(&temporary, path).map_err(|e| format!("rename to {path:?}: {e}"))?;
        inner.dirty = false;
        inner.last_save = Some(Instant::now());
        Ok(())
    }

    /// Save when the last save is old enough; typing never writes a file per
    /// keystroke.
    pub fn save_if_due(&self) {
        let due = {
            let inner = self.lock();
            inner.dirty
                && inner
                    .last_save
                    .is_none_or(|last| last.elapsed() >= SAVE_INTERVAL)
        };
        if due {
            if let Err(e) = self.save() {
                eprintln!("hime-core: typing statistics save failed: {e}");
            }
        }
    }

    /// Save anything unsaved (session end, scheme switch, quit).
    pub fn flush(&self) {
        if self.lock().dirty {
            if let Err(e) = self.save() {
                eprintln!("hime-core: typing statistics save failed: {e}");
            }
        }
    }

    /// Everything the panel renders, relative to today's local date.
    #[must_use]
    pub fn info(&self) -> TypingStatsInfo {
        self.info_on(today())
    }

    fn info_on(&self, day: Date) -> TypingStatsInfo {
        let scheme = self.active_scheme();
        let inner = self.lock();
        let image = &inner.image;
        let daily = &image.daily;
        let chars_on = |date: Date| daily.get(&date.key()).map_or(0, |c| c.chars);

        let today_chars = chars_on(day);
        let week_start = day.add_days(-day.weekday());
        let mut week = 0;
        for offset in 0..=day.weekday() {
            week += chars_on(week_start.add_days(offset));
        }
        let mut month = 0;
        for (key, counters) in daily {
            if key.starts_with(&format!("{:04}-{:02}-", day.year, day.month)) {
                month += counters.chars;
            }
        }
        let mut year = 0;
        for (key, counters) in daily {
            if key.starts_with(&format!("{:04}-", day.year)) {
                year += counters.chars;
            }
        }

        let overall = &image.overall;
        let minutes = overall.run_millis as f64 / 60_000.0;
        let seconds = overall.run_millis as f64 / 1_000.0;
        let speed = SpeedStats {
            runs: overall.runs,
            chars: overall.run_chars,
            keys: overall.run_keys,
            millis: overall.run_millis,
            chars_per_min: round2(if minutes > 0.0 { overall.run_chars as f64 / minutes } else { 0.0 }),
            keys_per_sec: round2(if seconds > 0.0 { overall.run_keys as f64 / seconds } else { 0.0 }),
            code_length: round2(if overall.run_chars > 0 {
                overall.run_keys as f64 / overall.run_chars as f64
            } else {
                0.0
            }),
        };
        let first_choice = FirstChoiceStats {
            selections: overall.selections,
            preferred: overall.preferred,
            rate: round2(if overall.selections > 0 {
                overall.preferred as f64 / overall.selections as f64 * 100.0
            } else {
                0.0
            }),
        };

        let keyboards: Vec<KeyboardStats> = image
            .schemes
            .iter()
            .map(|(name, keys)| keyboard_stats(name, keys))
            .collect();
        let keyboard = image
            .schemes
            .get(&scheme)
            .map(|keys| keyboard_stats(&scheme, keys))
            .unwrap_or_else(|| keyboard_stats(&scheme, &BTreeMap::new()));

        let days = (-(HEATMAP_DAYS - 1)..=0)
            .map(|offset| {
                let date = day.add_days(offset);
                DayChars { date: date.key(), chars: chars_on(date) }
            })
            .collect();

        TypingStatsInfo {
            path: self.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            exists: self.path.as_ref().is_some_and(|p| p.is_file()),
            file_bytes: self.path.as_ref().and_then(|p| fs::metadata(p).ok()).map_or(0, |m| m.len()),
            modified_secs: self
                .path
                .as_ref()
                .and_then(|p| fs::metadata(p).ok())
                .and_then(|m| m.modified().ok())
                .and_then(|when| when.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64),
            active_scheme: scheme,
            error: inner.error.clone(),
            first_day: daily.iter().find(|(_, c)| c.chars > 0).map(|(key, _)| key.clone()),
            chars: CharTotals {
                total: overall.chars,
                today: today_chars,
                week,
                month,
                year,
            },
            speed,
            first_choice,
            keyboard,
            keyboards,
            days,
        }
    }
}

/// Build one scheme's keyboard statistics from its raw histogram. Keys the
/// finger map does not place (arrows, function keys) stay in `keys` for the
/// heat map but contribute to no hand or finger total.
fn keyboard_stats(scheme: &str, keys: &BTreeMap<String, u64>) -> KeyboardStats {
    let mut stats = KeyboardStats {
        scheme: scheme.to_string(),
        total: 0,
        left: 0,
        right: 0,
        thumb: 0,
        balance: 0.0,
        keys: keys.clone(),
        fingers: Vec::new(),
    };
    let mut per_finger = [0u64; 9];
    for (name, count) in keys {
        let Some(finger) = Finger::of(name) else { continue };
        let slot = Finger::all()
            .iter()
            .position(|f| *f == finger)
            .expect("every finger is in the table");
        per_finger[slot] += count;
        stats.total += count;
        match finger.hand() {
            "left" => stats.left += count,
            "right" => stats.right += count,
            _ => stats.thumb += count,
        }
    }
    stats.fingers = Finger::all()
        .iter()
        .zip(per_finger)
        .map(|(finger, count)| FingerShare {
            id: finger.id().to_string(),
            name: finger.name().to_string(),
            hand: finger.hand().to_string(),
            count,
        })
        .collect();
    let hands = stats.left + stats.right;
    stats.balance = round2(if hands > 0 {
        let delta = stats.left.abs_diff(stats.right);
        100.0 - delta as f64 / hands as f64 * 100.0
    } else {
        0.0
    });
    stats
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// The canonical name of one key code, i.e. the physical key it sits on:
/// printable keys by their unshifted character (letters folded to lowercase,
/// shifted symbols folded onto their base key), the control keys by name,
/// `None` for codes outside the recorded set. The heat map and the finger
/// split therefore count one bucket per physical key.
#[must_use]
pub fn key_name(key: u32) -> Option<String> {
    let name = match key {
        8 => "Backspace",
        13 => "Enter",
        27 => "Escape",
        32 => "Space",
        9 => "Tab",
        0x2190 => "Left",
        0x2191 => "Up",
        0x2192 => "Right",
        0x2193 => "Down",
        33..=126 => {
            let byte = u8::try_from(key).ok()?;
            return Some(base_key(byte).to_ascii_lowercase().to_string());
        }
        _ => return None,
    };
    Some(name.to_string())
}

/// The unshifted key of a printable ASCII code (US layout). Shifted symbols
/// fold onto the digit or punctuation key they are printed on.
fn base_key(byte: u8) -> char {
    match byte {
        b'!' => '1',
        b'@' => '2',
        b'#' => '3',
        b'$' => '4',
        b'%' => '5',
        b'^' => '6',
        b'&' => '7',
        b'*' => '8',
        b'(' => '9',
        b')' => '0',
        b'_' => '-',
        b'+' => '=',
        b'{' => '[',
        b'}' => ']',
        b'|' => '\\',
        b':' => ';',
        b'"' => '\'',
        b'<' => ',',
        b'>' => '.',
        b'?' => '/',
        b'~' => '`',
        _ => char::from(byte),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(year: i32, month: u32, day: u32) -> Date {
        Date { year, month, day }
    }

    #[test]
    fn civil_days_round_trip_and_order() {
        for (y, m, d, weekday) in [
            (1970, 1, 1, 3), // Thursday
            (2024, 2, 29, 3),
            (2026, 11, 27, 4),
            (1969, 12, 31, 2),
        ] {
            let date = day(y, m, d);
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), date);
            assert_eq!(date.weekday(), weekday, "{date:?}");
            assert_eq!(Date::parse(&date.key()), Some(date));
        }
        assert_eq!(day(2026, 1, 4).weekday(), 6, "Sunday");
        assert_eq!(day(2026, 1, 5).weekday(), 0, "Monday starts the week");
    }

    #[test]
    fn adding_days_crosses_month_and_year_bounds() {
        assert_eq!(day(2026, 12, 31).add_days(1), day(2027, 1, 1));
        assert_eq!(day(2026, 3, 1).add_days(-1), day(2026, 2, 28));
        assert_eq!(day(2024, 3, 1).add_days(-1), day(2024, 2, 29));
    }

    fn sample_stats() -> TypingStats {
        let stats = TypingStats::in_memory();
        stats.set_scheme("liuli_v1");
        stats.record_commit_on(day(2026, 11, 27), 120, Some(true));
        stats.record_commit_on(day(2026, 11, 26), 80, Some(false));
        stats.record_commit_on(day(2026, 10, 31), 40, None);
        stats.record_commit_on(day(2025, 12, 31), 10, Some(true));
        stats
    }

    #[test]
    fn totals_split_into_today_week_month_and_year() {
        let info = sample_stats().info_on(day(2026, 11, 27));
        assert_eq!(info.chars.total, 250);
        assert_eq!(info.chars.today, 120);
        assert_eq!(info.chars.week, 200, "the week starts on Monday");
        assert_eq!(info.chars.month, 200);
        assert_eq!(info.chars.year, 240);
        assert_eq!(info.first_day.as_deref(), Some("2025-12-31"));
        assert_eq!(info.days.len(), HEATMAP_DAYS as usize);
        assert_eq!(info.days.last().unwrap().chars, 120);
    }

    #[test]
    fn first_choice_rate_counts_selections_only() {
        let info = sample_stats().info_on(day(2026, 11, 27));
        assert_eq!(info.first_choice.selections, 3);
        assert_eq!(info.first_choice.preferred, 2);
        assert_eq!(info.first_choice.rate, 66.67);
    }

    #[test]
    fn runs_shorter_than_the_threshold_are_not_samples() {
        let stats = TypingStats::in_memory();
        stats.record_sample_on(day(2026, 11, 27), RUN_MIN_CHARS, 40, 4_000);
        assert_eq!(stats.info_on(day(2026, 11, 27)).speed.runs, 0);
        let mut short = RunMeter::new(Instant::now());
        short.add_chars(RUN_MIN_CHARS);
        stats.record_run(short);
        assert_eq!(stats.info_on(day(2026, 11, 27)).speed.runs, 0);
        stats.record_sample_on(day(2026, 11, 27), RUN_MIN_CHARS + 1, 40, 4_000);
        let info = stats.info_on(day(2026, 11, 27));
        assert_eq!(info.speed.runs, 1);
        assert_eq!(info.speed.chars_per_min, 240.0);
        assert_eq!(info.speed.keys_per_sec, 10.0);
        assert_eq!(info.speed.code_length, 2.5);
    }

    #[test]
    fn a_run_meter_caps_each_inter_key_gap() {
        let start = Instant::now();
        let mut run = RunMeter::new(start);
        run.press(start + Duration::from_millis(500));
        run.press(start + Duration::from_secs(30));
        assert_eq!(run.millis(), 500 + KEY_GAP_CAP.as_millis() as u64);
        assert_eq!(run.keys(), 2);
        run.add_chars(16);
        assert!(run.qualifies());
        assert!(run.idle_for(start + Duration::from_secs(31) + RUN_IDLE_GAP));
    }

    #[test]
    fn key_names_are_canonical_and_fold_shifted_symbols() {
        assert_eq!(key_name(u32::from(b'A')).as_deref(), Some("a"));
        assert_eq!(key_name(u32::from(b'a')).as_deref(), Some("a"));
        assert_eq!(key_name(32).as_deref(), Some("Space"));
        assert_eq!(key_name(8).as_deref(), Some("Backspace"));
        assert_eq!(key_name(0x2190).as_deref(), Some("Left"));
        assert_eq!(key_name(u32::from(b'!')).as_deref(), Some("1"));
        assert_eq!(key_name(u32::from(b':')).as_deref(), Some(";"));
        assert_eq!(key_name(u32::from(b'?')).as_deref(), Some("/"));
        assert_eq!(key_name(u32::from(b'~')).as_deref(), Some("`"));
        assert_eq!(key_name(0), None);
        assert_eq!(key_name(0x10FFFF), None);
    }

    #[test]
    fn finger_map_covers_the_mac_ansi_block() {
        assert_eq!(Finger::of("a"), Some(Finger::LeftPinky));
        assert_eq!(Finger::of("f"), Some(Finger::LeftIndex));
        assert_eq!(Finger::of("j"), Some(Finger::RightIndex));
        assert_eq!(Finger::of(";"), Some(Finger::RightPinky));
        assert_eq!(Finger::of("Space"), Some(Finger::Thumb));
        assert_eq!(Finger::of("Left"), None, "arrows are outside the block");
        let mapped: Vec<&str> = "`1234567890-=qwertyuiop[]\\asdfghjkl;'zxcvbnm,./".split("").collect();
        assert!(mapped.iter().all(|key| key.is_empty() || Finger::of(key).is_some()));
    }

    #[test]
    fn keyboard_splits_follow_the_histogram() {
        let stats = TypingStats::in_memory();
        stats.set_scheme("liuli_v1");
        // a (left pinky) x3, j (right index) x1, space x2, arrow x4.
        for key in [
            u32::from(b'a'),
            u32::from(b'a'),
            u32::from(b'a'),
            u32::from(b'j'),
            32,
            32,
            0x2190,
            0x2190,
            0x2190,
            0x2190,
        ] {
            stats.press_key(key);
        }
        let info = stats.info_on(day(2026, 11, 27));
        assert_eq!(info.keyboard.scheme, "liuli_v1");
        assert_eq!(info.keyboard.left, 3);
        assert_eq!(info.keyboard.right, 1);
        assert_eq!(info.keyboard.thumb, 2);
        assert_eq!(info.keyboard.total, 6);
        assert_eq!(info.keyboard.balance, 50.0);
        assert_eq!(info.keyboard.keys["a"], 3);
        assert_eq!(info.keyboard.keys.get("Left"), Some(&4));
        assert_eq!(info.speed.runs, 0);

        // A scheme switch starts filling another histogram and keeps this one.
        stats.set_scheme("pinyin");
        stats.press_key(u32::from(b'a'));
        let pinyin = stats.info_on(day(2026, 11, 27));
        assert_eq!(pinyin.keyboard.scheme, "pinyin");
        assert_eq!(pinyin.keyboard.keys["a"], 1);
        assert_eq!(pinyin.keyboard.left, 1);
        stats.set_scheme("liuli_v1");
        let back = stats.info_on(day(2026, 11, 27));
        assert_eq!(back.keyboard.keys["a"], 3);
        assert_eq!(back.keyboard.total, 6);
    }

    #[test]
    fn saving_and_loading_preserves_the_counters() {
        let root = std::env::temp_dir().join(format!(
            "hime-typing-test-{}-{}",
            std::process::id(),
            now_secs()
        ));
        let path = root.join("statistics").join("typing.json");
        let stats = TypingStats::load(Some(path.clone()));
        stats.set_scheme("liuli_v1");
        stats.record_commit_on(day(2026, 11, 27), 42, Some(true));
        stats.record_sample_on(day(2026, 11, 27), 16, 32, 12_000);
        stats.press_key(u32::from(b'g'));
        stats.save().unwrap();
        assert!(path.is_file());

        let loaded = TypingStats::load(Some(path.clone()));
        let info = loaded.info_on(day(2026, 11, 27));
        assert_eq!(loaded.active_scheme(), "");
        assert_eq!(info.chars.total, 42);
        assert_eq!(info.first_choice.preferred, 1);
        assert_eq!(info.speed.runs, 1);
        assert_eq!(info.speed.code_length, 2.0);
        assert_eq!(info.error, None);

        // A corrupt file is preserved beside a fresh start.
        fs::write(&path, b"{ not json").unwrap();
        let recovered = TypingStats::load(Some(path.clone()));
        let info = recovered.info_on(day(2026, 11, 27));
        assert!(info.error.is_some());
        assert_eq!(info.chars.total, 0);
        let aside: Vec<_> = fs::read_dir(root.join("statistics"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".bad-"))
            .collect();
        assert_eq!(aside.len(), 1, "the old bytes are kept");
        std::fs::remove_dir_all(root).unwrap();
    }
}
