//! HFNGMF v1 (`.hng`) whole-sentence n-gram model reader (SPEC §7).
//!
//! Port of the reference implementation's `NgramModel.swift` / `HngReader` and its executable reference
//! `verify_hng.py`. The format spec is `docs/hfngmf-v1.md` in the reference project;
//! the scoring math is the three-level interpolation it documents:
//!
//! ```text
//! P1(c)         = uni[id(c)]
//! P2(c | w)     = s2(w,c)   + λ2(w)     · P1(c)
//! P3(c | w1,w2) = s3(w1,w2,c) + λ3(w1,w2) · P2(c | w2)
//! logp          = ln(max(P3, 1e-300))
//! ```
//!
//! Loading is `open` + `mmap` + a header check: the unigram table, bigram row
//! offsets, trigram bucket directory and hot×hot dense matrix are raw in-file
//! images, so there is no per-process deserialization copy. Lookups are direct:
//! code point → id (`cpmap`), id → bigram row (O(1)), trigram contexts via the
//! hot matrix or a per-`prev1` bucket with interpolation search.
//!
//! The arithmetic runs in `f64` over `bf16`-decoded operands, matching
//! `verify_hng.py` so the two readers agree bit-for-bit at the reported
//! precision. `Mmap` is `Send + Sync`, so an `NgramModel` behind an `Arc`
//! serves every session thread (AGENTS.md §1).

use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use memmap2::Mmap;
use serde::Serialize;

/// HFNGMF v1 magic (ASCII `HFNGMF01`).
const MAGIC: &[u8; 8] = b"HFNGMF01";
/// Header is exactly 256 bytes (4 cache lines).
const HEADER_SIZE: usize = 256;
/// Every section start is a multiple of this.
const ALIGN: usize = 64;
/// Reserved id meaning "no id" in `cpmap`.
const INVALID_ID: u16 = 0xFFFF;
/// Reserved ids and code points (spec §4).
const ID_UNK: u16 = 0;
const ID_BOS: u16 = 1;
/// 0x0002 / 0x0003 — the BOS / EOS control code points the decoder feeds `logp`.
pub const CP_BOS: u32 = 0x0002;
pub const CP_EOS: u32 = 0x0003;

/// Number of sections, in the fixed order of spec §2.
const SECT_COUNT: usize = 13;
// Section indices.
const S_CPMAP: usize = 0;
const S_UNI: usize = 1;
const S_BI_INDEX: usize = 2;
const S_TRI_DIR: usize = 3;
const S_HOT_TRI: usize = 4;
const S_BI_ROWS_HOT: usize = 5;
const S_TRI_ROWS_HOT: usize = 6;
const S_BI_ROWS_COLD: usize = 7;
const S_TRI_KEYS: usize = 8;
const S_TRI_RECS: usize = 9;
const S_TRI_ROWS_COLD: usize = 10;
const S_META: usize = 11;

/// Why a file was rejected at load time.
#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    /// Too short to be a model, or the magic does not match — the file is not
    /// HFNGMF v1 at all (e.g. an old `TCSKNM02` export).
    NotHng,
    /// A header field contradicted the spec or the real file length.
    BadHeader(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "io error: {e}"),
            LoadError::NotHng => write!(f, "not an HFNGMF v1 (.hng) model"),
            LoadError::BadHeader(why) => write!(f, "invalid HFNGMF header: {why}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<std::io::Error> for LoadError {
    fn from(e: std::io::Error) -> Self {
        LoadError::Io(e)
    }
}

/// bf16 (16-bit) bit pattern → f64, keeping the exact f32 value the pattern
/// encodes. This matches the Python oracle, which widens every bf16-decoded
/// operand to double before any arithmetic.
#[inline]
fn bf16_to_f64(h: u16) -> f64 {
    f32::from_bits((h as u32) << 16) as f64
}

/// A loaded `.hng` model. Owns the mapping and the decoded quantization table.
#[derive(Debug)]
pub struct NgramModel {
    data: Mmap,
    /// Header `version` field (the format is gated by [`MAGIC`], so it is 1).
    header_version: u32,
    /// Header `order` byte (always 3, validated at load).
    order: u8,
    vocab: usize,
    hot: usize,
    code_bits: u8,
    /// log2 probability range in octaves; only meaningful when `code_bits == 8`.
    range_oct: u8,
    /// 8-bit code → 2^(-q·range/255); only used when `code_bits == 8`.
    pow2: Vec<f64>,
    /// Absolute section start offsets into `data`.
    sect: [usize; SECT_COUNT],
    /// Physical end of the hot region (tri_rows_hot partition), for validation.
    hot_end: usize,
    /// Counts and build metadata, for diagnostics.
    bi_ctx: usize,
    tri_ctx: usize,
    file_bytes: u64,
    vocab_count: u32,
    meta: String,
    build_id: String,
}

impl NgramModel {
    /// Open and map a `.hng` file, then validate the header (O(1) checks).
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let file = File::open(path.as_ref())?;
        // SAFETY: the caller keeps `file` alive only long enough to map; the
        // mapping stays valid even if the file is later edited or removed on
        // macOS, and the model is read-only. This matches the reference reader, which
        // assumes the file is not mutated while loaded.
        let data = unsafe { Mmap::map(&file) }.map_err(LoadError::Io)?;
        Self::from_mmap(data)
    }

    /// Validate and index an already-mapped buffer (split out so a test can map
    /// a synthetic image).
    fn from_mmap(data: Mmap) -> Result<Self, LoadError> {
        let file_size = data.len();
        if file_size < HEADER_SIZE {
            return Err(LoadError::NotHng);
        }
        if &data[0..8] != MAGIC {
            return Err(LoadError::NotHng);
        }

        let u16_at = |off: usize| -> u16 { u16::from_le_bytes(data[off..off + 2].try_into().unwrap()) };
        let u32_at = |off: usize| -> u32 { u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) };
        let u64_at = |off: usize| -> u64 { u64::from_le_bytes(data[off..off + 8].try_into().unwrap()) };
        let bad = |why: String| Err(LoadError::BadHeader(why));

        let header_version = u32_at(8);
        if header_version != 1 {
            return bad("version".into());
        }
        if u32_at(12) as usize != HEADER_SIZE {
            return bad("header_size".into());
        }
        let declared_size = u64_at(16);
        if declared_size as usize != file_size {
            return bad(format!("file_size declared {declared_size} != actual {file_size}"));
        }
        if u32_at(24) != ALIGN as u32 {
            return bad("align".into());
        }
        let order = data[48];
        if order != 3 {
            return bad("order".into());
        }
        let code_bits = data[49];
        if code_bits != 8 && code_bits != 16 {
            return bad(format!("code_bits {code_bits}"));
        }
        let range_oct = data[51] as usize;
        if code_bits == 8 && !(1..=32).contains(&range_oct) {
            return bad(format!("code_range {range_oct}"));
        }
        let range_oct = data[51];
        let vocab = u32_at(32) as usize;
        let hot = u32_at(36) as usize;
        if !(3..=INVALID_ID as usize).contains(&vocab) {
            return bad(format!("vocab {vocab}"));
        }
        if !(3..=vocab.min(4096)).contains(&hot) {
            return bad(format!("hot_limit {hot}"));
        }
        if u16_at(52) != ID_BOS || u16_at(54) != 2 {
            return bad("bos/eos id".into());
        }

        let mut sect = [0usize; SECT_COUNT];
        for (i, slot) in sect.iter_mut().enumerate() {
            *slot = u64_at(56 + 8 * i) as usize;
        }
        // Section starts: ordered, ≥ header, aligned, within file.
        let mut end = [0usize; SECT_COUNT];
        for i in 0..SECT_COUNT {
            end[i] = if i + 1 < SECT_COUNT { sect[i + 1] } else { file_size };
            if sect[i] < HEADER_SIZE || sect[i] % ALIGN != 0 || sect[i] > end[i] || end[i] > file_size {
                return bad(format!("section {i} start {} end {}", sect[i], end[i]));
            }
        }
        // Fixed-length sections must match the header counts (within a 64-byte pad).
        let bi_ctx = u32_at(40) as usize;
        let tri_ctx = u32_at(44) as usize;
        let meta_len = u64_at(168) as usize;
        let fixed: [(usize, usize); 9] = [
            (S_CPMAP, 0x10000 * 2),
            (S_UNI, vocab * 2),
            (S_BI_INDEX, vocab * 4),
            (S_TRI_DIR, (vocab + 1) * 4),
            (S_HOT_TRI, hot * hot * 4),
            (S_TRI_KEYS, tri_ctx * 2),
            (S_TRI_RECS, tri_ctx * 4),
            (S_META, meta_len),
            (12, vocab * 4),
        ];
        for (s, want) in fixed {
            let got = end[s] - sect[s];
            if !(want..want + ALIGN).contains(&got) {
                return bad(format!("section {s} length {got} inconsistent (need {want})"));
            }
        }
        let hot_end = u64_at(160) as usize;
        if !(sect[S_TRI_ROWS_HOT] <= hot_end && hot_end <= end[S_TRI_ROWS_HOT]) {
            return bad(format!("hot_end {hot_end}"));
        }

        // 8-bit quantization table, computed in f64 like the Python oracle.
        let pow2 = if code_bits == 8 {
            (0..256).map(|q| 2.0f64.powf(-q as f64 * range_oct as f64 / 255.0)).collect()
        } else {
            Vec::new()
        };

        let meta = if end[S_META] > sect[S_META] {
            String::from_utf8_lossy(&data[sect[S_META]..end[S_META]]).trim_end().to_string()
        } else {
            String::new()
        };
        let build_id_end = data[232..248]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(16);
        let build_id = String::from_utf8_lossy(&data[232..232 + build_id_end]).to_string();

        Ok(NgramModel {
            data,
            header_version,
            order,
            vocab,
            hot,
            code_bits,
            range_oct,
            pow2,
            sect,
            hot_end,
            bi_ctx,
            tri_ctx,
            file_bytes: declared_size,
            vocab_count: vocab as u32,
            meta,
            build_id,
        })
    }

    // ---- small-endian readers over the mapping ----

    #[inline]
    fn u16(&self, off: usize) -> u16 {
        u16::from_le_bytes(self.data[off..off + 2].try_into().unwrap())
    }
    #[inline]
    fn u32(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.data[off..off + 4].try_into().unwrap())
    }

    // ---- id mapping ----

    /// Code point → vocabulary id; `None` for out-of-BMP or unmapped (`INVALID`).
    #[inline]
    fn id_of(&self, cp: u32) -> Option<u16> {
        if cp >= 0x10000 {
            return None;
        }
        let v = self.u16(self.sect[S_CPMAP] + (cp as usize) * 2);
        if v == INVALID_ID {
            None
        } else {
            Some(v)
        }
    }

    /// Code point → id, falling back to UNK (id 0) for unmapped targets.
    #[inline]
    fn id_or_unk(&self, cp: u32) -> u16 {
        self.id_of(cp).unwrap_or(ID_UNK)
    }

    /// Row regions for bigram vs trigram rows (spec §2): hot/cold base offsets.
    #[inline]
    fn bi_bases(&self) -> (usize, usize) {
        (self.sect[S_BI_ROWS_HOT], self.sect[S_BI_ROWS_COLD])
    }
    #[inline]
    fn tri_bases(&self) -> (usize, usize) {
        (self.sect[S_TRI_ROWS_HOT], self.sect[S_TRI_ROWS_COLD])
    }

    /// Resolve a row reference to its absolute start and successor count. A
    /// reference of `0` or a zero-successor row resolves to `None` ("no row":
    /// the caller reads λ = 1, i.e. a full backoff).
    #[inline]
    fn row_of(&self, refs: u32, hot_base: usize, cold_base: usize) -> Option<(usize, usize)> {
        if refs == 0 {
            return None;
        }
        let off = (refs & 0x7FFF_FFFF) - 1;
        let base = if refs & 0x8000_0000 != 0 { hot_base } else { cold_base } + off as usize;
        let b0 = self.data[base] as usize;
        let n = if b0 & 0x80 != 0 { self.u16(base + 6) as usize } else { b0 & 0x7F };
        if n == 0 {
            None
        } else {
            Some((base, n))
        }
    }

    /// Find `target` among a row's ascending successor ids, returning its index.
    /// Short rows (the common case) use an early-exit linear scan; long rows
    /// (high-frequency bigram heads, up to ~1500 successors) binary search.
    fn succ_find(&self, ids_base: usize, n: usize, target: u16) -> Option<usize> {
        if n <= 16 {
            for i in 0..n {
                let v = self.u16(ids_base + i * 2);
                if v == target {
                    return Some(i);
                }
                if v > target {
                    return None;
                }
            }
            None
        } else {
            let mut low = 0usize;
            let mut high = n;
            while low < high {
                let mid = (low + high) / 2;
                let v = self.u16(ids_base + mid * 2);
                if v < target {
                    low = mid + 1;
                } else {
                    high = mid;
                }
            }
            if low < n && self.u16(ids_base + low * 2) == target {
                Some(low)
            } else {
                None
            }
        }
    }

    /// A row's `(λ, decoded-mass-of-target)`; `(0, 1)` when there is no row.
    /// A present row without the target yields `(0, λ)` so the backoff keeps λ.
    #[inline]
    fn row_lambda_mass(&self, refs: u32, hot_base: usize, cold_base: usize, target: u16) -> (f64, f64) {
        let Some((base, n)) = self.row_of(refs, hot_base, cold_base) else {
            return (1.0, 0.0);
        };
        let lambda = bf16_to_f64(self.u16(base + 2));
        let ids_base = base + 8;
        let Some(idx) = self.succ_find(ids_base, n, target) else {
            return (lambda, 0.0);
        };
        let codes_base = base + 8 + 2 * n;
        let mass = if self.code_bits == 8 {
            let scale = bf16_to_f64(self.u16(base + 4));
            let code = self.data[codes_base + idx] as usize;
            scale * self.pow2[code]
        } else {
            bf16_to_f64(self.u16(codes_base + idx * 2))
        };
        (lambda, mass)
    }

    /// P1(target): the unigram, falling back to the UNK bucket.
    #[inline]
    pub fn unigram(&self, target_cp: u32) -> f64 {
        bf16_to_f64(self.u16(self.sect[S_UNI] + (self.id_or_unk(target_cp) as usize) * 2))
    }

    fn unigram_id(&self, id: u16) -> f64 {
        bf16_to_f64(self.u16(self.sect[S_UNI] + (id as usize) * 2))
    }

    /// P2(target | prev) and P3(target | prev2, prev1) together, mirroring
    /// `verify_hng.py::probabilities`. Returns `(p1, p2, p3)`.
    fn probabilities(&self, prev2_cp: u32, prev1_cp: u32, target_cp: u32) -> (f64, f64, f64) {
        let target = self.id_or_unk(target_cp);
        let p1 = self.unigram_id(target);
        let i2 = self.id_of(prev1_cp);
        let i1 = self.id_of(prev2_cp);

        // Bigram: a BOS or unmapped prev has no stored row → λ = 1 → p2 = p1.
        let (lam2, s2) = match i2 {
            None | Some(ID_BOS) => (1.0, 0.0),
            Some(pid) => {
                let refs = self.u32(self.sect[S_BI_INDEX] + (pid as usize) * 4);
                let (hot, cold) = self.bi_bases();
                self.row_lambda_mass(refs, hot, cold, target)
            }
        };
        let p2 = s2 + lam2 * p1;

        // Trigram: unmapped context → no row → p3 = p2.
        let (lam3, s3) = match (i1, i2) {
            (Some(a), Some(b)) => {
                let refs = self.trigram_refs(a, b);
                let (hot, cold) = self.tri_bases();
                self.row_lambda_mass(refs, hot, cold, target)
            }
            _ => (1.0, 0.0),
        };
        let p3 = s3 + lam3 * p2;

        (p1, p2, p3)
    }

    /// Locate the row reference for a trigram context `prev2 = a, prev1 = b`.
    /// Hot×hot uses the dense matrix; otherwise the per-`prev1` bucket with
    /// interpolation search. `0` means the context was never observed.
    fn trigram_refs(&self, a: u16, b: u16) -> u32 {
        let (a, b) = (a as usize, b as usize);
        if a < self.hot && b < self.hot {
            return self.u32(self.sect[S_HOT_TRI] + (b * self.hot + a) * 4);
        }
        let lo = self.u32(self.sect[S_TRI_DIR] + b * 4) as usize;
        let hi = self.u32(self.sect[S_TRI_DIR] + (b + 1) * 4) as usize;
        if lo == hi {
            return 0; // empty bucket: the context cannot exist
        }
        match self.bucket_search(lo, hi, a as u16) {
            Some(pos) => self.u32(self.sect[S_TRI_RECS] + pos * 4),
            None => 0,
        }
    }

    /// Search `tri_keys[lo..hi]` (strictly ascending ids) for `key`. Returns the
    /// index into `tri_keys`/`tri_recs`, or `None`. Mirrors `verify_hng.py`:
    /// one interpolation probe, then a binary step when the probe undershoots.
    fn bucket_search(&self, lo: usize, hi: usize, key: u16) -> Option<usize> {
        let keys = self.sect[S_TRI_KEYS];
        let want = key as usize;
        let klow = self.u16(keys + lo * 2) as usize;
        let khigh = self.u16(keys + (hi - 1) * 2) as usize;
        if want < klow || want > khigh {
            return None;
        }
        let at = |i: usize| self.u16(keys + i * 2) as usize;
        let mut pos = lo;
        if khigh != klow {
            let span = (hi - lo - 1) * (want - klow);
            pos = lo + span / (khigh - klow);
            if pos >= hi {
                pos = hi - 1;
            }
        }
        while pos < hi && at(pos) < want {
            pos += 1;
            if pos < hi && at(pos) < want {
                // The probe undershot badly: switch to a binary search over
                // [pos, hi) rather than scan the long tail linearly.
                let (mut a, mut b) = (pos, hi);
                while a < b {
                    let m = (a + b) / 2;
                    if at(m) < want {
                        a = m + 1;
                    } else {
                        b = m;
                    }
                }
                pos = a;
                break;
            }
        }
        if pos >= hi || at(pos) != want {
            return None;
        }
        Some(pos)
    }

    /// log P(target | prev2, prev1) with the three-level interpolation; 0.0 when
    /// the model is absent (the engine treats it as a missing signal). The
    /// probability is floored at 1e-300 before taking the log.
    pub fn logp(&self, prev2_cp: u32, prev1_cp: u32, target_cp: u32) -> f64 {
        let (_, _, p3) = self.probabilities(prev2_cp, prev1_cp, target_cp);
        p3.max(1e-300).ln()
    }

    /// Whether the model observed `prev → target` as a bigram (isolated-rare
    /// signal reserved by the reference implementation; not used by the migrated scoring path today).
    pub fn has_observed_bigram(&self, prev_cp: u32, target_cp: u32) -> bool {
        let Some(pid) = self.id_of(prev_cp) else {
            return false;
        };
        if pid == ID_BOS {
            return false;
        }
        let refs = self.u32(self.sect[S_BI_INDEX] + (pid as usize) * 4);
        let (hot, cold) = self.bi_bases();
        let Some((base, n)) = self.row_of(refs, hot, cold) else {
            return false;
        };
        self.succ_find(base + 8, n, self.id_or_unk(target_cp)).is_some()
    }

    // ---- diagnostics ----

    /// Vocabulary size.
    pub fn vocab(&self) -> usize {
        self.vocab
    }
    /// Hot-region id bound.
    pub fn hot_limit(&self) -> usize {
        self.hot
    }
    /// Successor code width in bits (8 or 16).
    pub fn code_bits(&self) -> u8 {
        self.code_bits
    }
    /// Bigram context rows.
    pub fn bi_rows(&self) -> usize {
        self.bi_ctx
    }
    /// Trigram context rows.
    pub fn tri_rows(&self) -> usize {
        self.tri_ctx
    }
    /// Declared / actual file size in bytes.
    pub fn file_bytes(&self) -> u64 {
        self.file_bytes
    }
    /// Reported `vocab_count` header field.
    pub fn vocab_count(&self) -> u32 {
        self.vocab_count
    }
    /// The build metadata JSON the model carries (spec §9).
    pub fn meta(&self) -> &str {
        &self.meta
    }
    /// The build identifier.
    pub fn build_id(&self) -> &str {
        &self.build_id
    }
    /// Physical end of the hot region in bytes.
    pub fn hot_end(&self) -> usize {
        self.hot_end
    }

    /// Header and build summary for shell display ([`probe_model`],
    /// `hime_model_status` in SPEC §6).
    #[must_use]
    pub fn info(&self) -> ModelInfo {
        ModelInfo {
            format: "HFNGMF v1".to_string(),
            header_version: self.header_version,
            order: self.order,
            vocab: self.vocab,
            hot_limit: self.hot,
            bi_rows: self.bi_ctx,
            tri_rows: self.tri_ctx,
            code_bits: self.code_bits,
            code_range_oct: self.range_oct,
            file_bytes: self.file_bytes,
            build_id: self.build_id.clone(),
            meta: self.meta.clone(),
        }
    }
}

/// Serializable snapshot of a loaded model's header and build fields, for
/// the preferences 模型 page (SPEC §6 `hime_model_status`).
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    /// Format name the loader accepted ("HFNGMF v1").
    pub format: String,
    /// Header `version` field.
    pub header_version: u32,
    /// Model order (n); 3 = trigram.
    pub order: u8,
    /// Vocabulary size (ids incl. the reserved ones).
    pub vocab: usize,
    /// Hot-region id bound (dense trigram matrix size is this squared).
    pub hot_limit: usize,
    /// Bigram context rows.
    pub bi_rows: usize,
    /// Trigram context rows.
    pub tri_rows: usize,
    /// Successor code width in bits (8 = quantized, 16 = bf16).
    pub code_bits: u8,
    /// log2 probability range in octaves (8-bit quantization only).
    pub code_range_oct: u8,
    /// File size in bytes (declared and actual, validated equal at load).
    pub file_bytes: u64,
    /// The build identifier the header carries.
    pub build_id: String,
    /// The build metadata JSON the model carries (spec §9).
    pub meta: String,
}

/// Model path explicitly specified by the user (the preferences 模型 page,
/// through `hime_set_model_path`, SPEC §7.5). Process-global.
static MODEL_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

fn model_override() -> Option<PathBuf> {
    MODEL_OVERRIDE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

/// Set (`Some`) or clear (`None`) the user-specified model path. The
/// override is authoritative: while it is set, [`find_model_path`] offers no
/// fallback, so a moved or deleted file fails loudly instead of silently
/// selecting another model.
pub fn set_model_override(path: Option<PathBuf>) {
    *MODEL_OVERRIDE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

/// Locate the whole-sentence model file. Shared by the engine, the CLI and the
/// tests so they agree on where the model lives. Search order:
///
/// 1. the panel override [`set_model_override`] — authoritative: returned
///    when readable, otherwise nothing is found (no fallback candidates);
/// 2. `HIME_SENTENCE_MODEL` — the explicit override for tests and for a shell
///    that ships its own model;
/// 3. the user data folder `~/.config/hime/sentence-ngram.hng` (macOS; see
///    [`crate::paths`]) — where an installed model is meant to live, since the
///    large model is neither committed nor bundled;
/// 4. the repository `data/sentence-ngram.hng` (a development checkout);
/// 5. `data/sentence-ngram.hng` relative to the current directory.
#[must_use]
pub fn find_model_path() -> Option<PathBuf> {
    if let Some(path) = model_override() {
        return path.is_file().then_some(path);
    }
    if let Some(path) = std::env::var_os("HIME_SENTENCE_MODEL") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(path) = crate::paths::model_path() {
        if path.is_file() {
            return Some(path);
        }
    }
    let repo = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../data/sentence-ngram.hng"
    ));
    if repo.is_file() {
        return Some(repo);
    }
    let relative = PathBuf::from("data/sentence-ngram.hng");
    relative.is_file().then_some(relative)
}

/// Everything the preferences 模型 page shows about the model file
/// (`hime_model_status`, SPEC §6). Purely observational: it resolves and
/// validates a file without installing an engine.
#[derive(Debug, Serialize)]
pub struct ModelProbe {
    /// The panel override in effect ([`set_model_override`]), if set.
    pub override_path: Option<String>,
    /// `<data>/sentence-ngram.hng`, when the data folder resolves
    /// ([`crate::paths::model_path`]).
    pub default_path: Option<String>,
    /// The file that was validated: the engine's loaded file when given,
    /// otherwise whatever [`find_model_path`] resolves today.
    pub path: Option<String>,
    /// Why no model is usable: the load failure, or "nothing found".
    pub error: Option<String>,
    /// File modification time as Unix seconds, when the file is stat-able.
    pub modified_secs: Option<i64>,
    /// Header diagnostics; present exactly when the file loaded.
    pub info: Option<ModelInfo>,
}

/// Lexical path normalization for display: `.` segments drop and `..`
/// cancels the preceding named segment. Symlinks stay untouched (unlike
/// `canonicalize`), so an override through a symlink keeps showing the
/// path the user chose.
fn normalized_display(path: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::path::Component;
    let mut parts: Vec<OsString> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(last)
                    if last != ".."
                        && Path::new(&last).components().next().is_some_and(|c| matches!(c, Component::Normal(_))) =>
                {
                    parts.pop();
                }
                _ => parts.push(OsString::from("..")),
            },
            other => parts.push(other.as_os_str().to_os_string()),
        }
    }
    parts.iter().collect()
}

/// Display form of a model path (normalized, lossy UTF-8).
fn path_string(path: &Path) -> String {
    normalized_display(path).to_string_lossy().into_owned()
}

fn modified_secs_of(path: &Path) -> Option<i64> {
    let when = std::fs::metadata(path).ok().and_then(|m| m.modified().ok())?;
    match when.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => Some(d.as_secs() as i64),
        Err(e) => Some(-(e.duration().as_secs() as i64)),
    }
}

/// Resolve (or re-validate `loaded_path`) and load the model for display.
/// A failed load keeps the `path` and reports the reason in `error`; with
/// no file at all only the search paths are reported.
#[must_use]
pub fn probe_model(loaded_path: Option<&Path>) -> ModelProbe {
    let override_path = model_override();
    let (path, error, info, modified_secs) = match loaded_path.map(PathBuf::from).or_else(find_model_path) {
        None => {
            let why = match override_path.as_deref() {
                Some(bad) => format!("override model path {} is not a readable file", path_string(bad)),
                None => "no .hng model file found (searched the override path, \
                         HIME_SENTENCE_MODEL, the user data folder and the bundled data)"
                    .to_string(),
            };
            (None, Some(why), None, None)
        }
        Some(path) => {
            let modified = modified_secs_of(&path);
            let shown = path_string(&path);
            match NgramModel::load(&path) {
                Ok(model) => (Some(shown), None, Some(model.info()), modified),
                Err(e) => (Some(shown), Some(e.to_string()), None, modified),
            }
        }
    };
    ModelProbe {
        override_path: override_path.as_deref().map(path_string),
        default_path: crate::paths::model_path().map(|p| path_string(&p)),
        path,
        error,
        modified_secs,
        info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Probe every prediction of a sentence the way `verify_hng.py` does:
    /// `BOS BOS … EOS`, returning one logP per predicted character (EOS last).
    fn probe(model: &NgramModel, sentence: &str) -> Vec<f64> {
        let mut out = Vec::new();
        let mut prev2 = CP_BOS;
        let mut prev1 = CP_BOS;
        for ch in sentence.chars().chain(std::iter::once('\u{0003}')) {
            let target = if ch == '\u{0003}' { CP_EOS } else { ch as u32 };
            out.push(model.logp(prev2, prev1, target));
            prev2 = prev1;
            prev1 = target;
        }
        out
    }

    fn load_real_model() -> Option<NgramModel> {
        // Hold the override lock so a concurrent override test cannot make
        // this resolution point at its scratch file.
        let _override_guard = OVERRIDE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path = find_model_path()?;
        match NgramModel::load(&path) {
            Ok(model) => Some(model),
            Err(e) => {
                eprintln!("ngram test: {path:?} failed to load: {e}");
                None
            }
        }
        .or_else(|| {
            eprintln!("ngram test: skipped (no model at {path:?})");
            None
        })
    }

    #[test]
    fn header_fields_match_the_shipped_model() {
        let Some(model) = load_real_model() else { return };
        assert_eq!(model.vocab(), 17401);
        assert_eq!(model.hot_limit(), 1024);
        assert_eq!(model.code_bits(), 8);
        assert_eq!(model.bi_rows(), 14413);
        assert_eq!(model.tri_rows(), 3893697);
        assert_eq!(model.file_bytes(), 200554816);
        assert_eq!(model.build_id(), "hng_train");
        assert!(model.meta().contains("HFNGMF v1"));
    }

    /// The `logp` for every trigram must match the reference `verify_hng.py --probe`
    /// to the reported 4 decimals; the `p1` unigrams likewise. This is the
    /// faithfulness gate for the whole reader against the real `.hng`.
    #[test]
    fn trigram_logp_matches_the_python_oracle() {
        let Some(model) = load_real_model() else { return };
        let cases: [(&str, &[f64]); 3] = [
            ("今天天气很好", &[
                -6.0281, -1.0756, -5.8508, -3.3820, -4.3029, -1.8999, -0.7346,
            ]),
            ("中华人民共和国", &[
                -4.7217, -3.0470, -1.2847, -0.4671, -2.5793, -0.0640, -0.1069, -3.2194,
            ]),
            ("我们的", &[-4.0057, -1.3423, -2.0809, -3.6275]),
        ];
        for (sentence, expected) in cases {
            let got = probe(&model, sentence);
            assert_eq!(got.len(), expected.len(), "prediction count for {sentence}");
            for (i, (&want, &have)) in expected.iter().zip(&got).enumerate() {
                assert!(
                    (want - have).abs() < 2e-3,
                    "{sentence} prediction {i}: oracle {want}, rust {have}"
                );
            }
        }
    }

    /// bf16 storage plus the oracle's 4-significant-figure display bound the
    /// agreement to roughly a part per hundred, so compare relatively.
    fn close_rel(got: f64, want: f64) -> bool {
        (got - want).abs() <= want.abs() * 0.02 + 1e-12
    }

    #[test]
    fn unigram_matches_the_python_oracle() {
        let Some(model) = load_real_model() else { return };
        // 今 → 6.638e-04, EOS → 1.104e-01 (verify_hng probe p1 column).
        assert!(close_rel(model.unigram('今' as u32), 6.638e-4));
        assert!(close_rel(model.unigram(CP_EOS), 1.104e-1));
        // An out-of-vocabulary code point falls back to the UNK bucket, which
        // is exactly `uni[0]` — the '—' probe's p1 of 7.385e-10.
        assert!(close_rel(model.unigram('—' as u32), 7.385e-10));
        // Any unmapped code point (in-BMP unmapped, or out-of-BMP) returns the
        // identical UNK probability, exactly.
        assert_eq!(model.unigram('\u{2014}' as u32), model.unigram(0x10_FFFF));
    }

    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn load_bytes(bytes: &[u8]) -> Result<NgramModel, LoadError> {
        use std::io::Write;
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("hime-hng-test-{}-{}.hng", std::process::id(), id));
        {
            let mut f = std::fs::File::create(&path).expect("temp file");
            f.write_all(bytes).expect("write temp");
        }
        let result = NgramModel::load(&path);
        let _ = std::fs::remove_file(&path);
        result
    }

    #[test]
    fn rejects_short_or_foreign_files() {
        assert!(matches!(load_bytes(b"tiny").unwrap_err(), LoadError::NotHng));
        // Right length, wrong magic — e.g. an old TCSKNM02 export.
        let foreign = vec![0u8; HEADER_SIZE];
        assert!(matches!(load_bytes(&foreign).unwrap_err(), LoadError::NotHng));
    }

    #[test]
    fn rejects_size_mismatch_after_a_valid_magic() {
        let mut buf = vec![0u8; HEADER_SIZE];
        buf[0..8].copy_from_slice(MAGIC);
        buf[8..12].copy_from_slice(&1u32.to_le_bytes()); // version
        buf[12..16].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes()); // header_size
        buf[16..24].copy_from_slice(&999_999u64.to_le_bytes()); // file_size (wrong)
        assert!(matches!(load_bytes(&buf).unwrap_err(), LoadError::BadHeader(_)));
    }

    /// Serializes read-modify-restore of the process-global model override.
    static OVERRIDE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set the model override for the duration of `body`, then restore it.
    fn with_override<T>(path: Option<PathBuf>, body: impl FnOnce() -> T) -> T {
        let guard = OVERRIDE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = model_override();
        set_model_override(path);
        let result = body();
        set_model_override(saved);
        drop(guard);
        result
    }

    fn scratch_file(tag: &str, bytes: &[u8]) -> PathBuf {
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "hime-ngram-override-{}-{}-{}.hng",
            std::process::id(),
            tag,
            id
        ));
        std::fs::write(&path, bytes).expect("write scratch model");
        path
    }

    #[test]
    fn model_override_is_authoritative_over_discovery() {
        let present = scratch_file("present", b"not a model, a path target");
        let absent = present.with_file_name("absent-override.hng");
        with_override(Some(present.clone()), || {
            assert_eq!(
                find_model_path().as_deref(),
                Some(present.as_path()),
                "the override is returned verbatim, whatever else exists"
            );
        });
        with_override(Some(absent), || {
            assert!(
                find_model_path().is_none(),
                "a missing override blocks the fallback candidates instead of silently falling back"
            );
        });
        with_override(None, || {
            let _ = std::fs::remove_file(&present);
        });
    }

    #[test]
    fn probe_reports_a_foreign_file_as_a_load_error() {
        let foreign = scratch_file("probe", b"tiny junk file");
        let probe = with_override(None, || probe_model(Some(&foreign)));
        assert_eq!(probe.path.as_deref(), Some(foreign.to_string_lossy().as_ref()));
        assert!(probe.error.as_deref().is_some_and(|e| e.contains("HFNGMF")), "got {:?}", probe.error);
        assert!(probe.info.is_none(), "a rejected file carries no header info");
        assert!(probe.modified_secs.is_some(), "a stat-able file reports its mtime");
        let _ = std::fs::remove_file(&foreign);
    }

    #[test]
    fn probe_of_the_shipped_model_reports_the_header_fields() {
        let _guard = OVERRIDE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let Some(resolved) = find_model_path() else {
            eprintln!("probe test: skipped (no model to probe)");
            return;
        };
        let probe = probe_model(None);
        assert!(probe.error.is_none(), "the shipped model must probe clean: {:?}", probe.error);
        assert_eq!(
            probe.path.as_deref(),
            Some(normalized_display(&resolved).to_string_lossy().as_ref())
        );
        assert!(probe.modified_secs.is_some());
        let info = probe.info.clone().expect("a clean probe carries header info");
        assert_eq!(info.format, "HFNGMF v1");
        assert_eq!(info.header_version, 1);
        assert_eq!(info.order, 3);
        assert_eq!(info.vocab, 17401);
        assert_eq!(info.hot_limit, 1024);
        assert_eq!(info.code_bits, 8);
        assert_eq!(info.file_bytes, 200_554_816);
        assert_eq!(info.build_id, "hng_train");
        // The probe is what hime-ffi serializes; it must stay JSON-shaped.
        let json = serde_json::to_string(&probe).expect("probe serializes");
        assert!(json.contains("\"tri_rows\""));
    }
}

