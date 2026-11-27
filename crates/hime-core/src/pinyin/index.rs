//! `.hidx`: the prebuilt 全拼 index container (SPEC §8.1).
//!
//! A pinyin table is 11 MB of compressed rows whose syllable index needs a full
//! sort of every key — seconds of work that must not happen inside a keystroke
//! or a scheme switch. The sort therefore runs offline (`hime-cli build-index`)
//! and its result is written as the flat arrays the query path wants anyway: a
//! text blob, fixed-stride row records, fixed-stride key records and a slot
//! array. Loading is `open` + `mmap` + a header check, the same shape as the
//! `.hng` model reader (SPEC §7), so pages fault in as queries touch them and a
//! process that never opens 反查 pays nothing.
//!
//! ```text
//! header (256 B) | blob | rows | keys | slots     (every section 64-byte aligned)
//! ```
//!
//! A row record is `text (offset, len)`, `code (offset, len)`, `weight`, `rank`,
//! `syllables` and the packed `reading`; a key record is `packed`, `start` and
//! `count`; the slot array holds row ids. All of it is little-endian.
//!
//! The header names the table the index was built from — [`hdict::Fingerprint`],
//! read from that table's own header — so an index left behind by an older table
//! is ignored rather than trusted, and the reader falls back to parsing the
//! table. Validation is O(1) (header CRC, strides, counts against section
//! lengths) and row payloads are read defensively: a truncated or edited file
//! cannot turn a keystroke into a panic.

use std::fs;
use std::io::{BufWriter, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use crate::hdict;

/// `.hidx` magic (ASCII `HIMEIDX1`).
pub const MAGIC: &[u8; 8] = b"HIMEIDX1";
/// Header is exactly 256 bytes (4 cache lines), like the `.hng` header.
pub const HEADER_SIZE: usize = 256;
/// Every section start is a multiple of this.
pub const ALIGN: usize = 64;
/// Bytes per row record (see [`Store::row`]).
pub const ROW_STRIDE: usize = 48;
/// Bytes per key record (see [`Store::key`]).
pub const KEY_STRIDE: usize = 24;
/// Bytes per slot entry (a row id).
pub const SLOT_STRIDE: usize = 4;
/// The only format version this reader and writer agree on.
const VERSION: u32 = 1;
/// Zero run used to pad sections to [`ALIGN`].
const ZERO_PAD: [u8; ALIGN] = [0; ALIGN];

// Header field offsets.
const OFF_VERSION: usize = 8;
const OFF_HEADER_SIZE: usize = 12;
const OFF_FILE_SIZE: usize = 16;
const OFF_ALIGN: usize = 24;
const OFF_ROW_STRIDE: usize = 28;
const OFF_KEY_STRIDE: usize = 30;
const OFF_ROW_COUNT: usize = 32;
const OFF_KEY_COUNT: usize = 36;
const OFF_SLOT_COUNT: usize = 40;
const OFF_SOURCE_BYTES: usize = 48;
const OFF_SOURCE_CRC: usize = 56;
const OFF_HEADER_CRC: usize = 60;
const OFF_SECTIONS: usize = 64;

/// Section order in the file; the header stores one start per entry.
const SECTION_COUNT: usize = 4;
const SECTION_BLOB: usize = 0;
const SECTION_ROWS: usize = 1;
const SECTION_KEYS: usize = 2;
const SECTION_SLOTS: usize = 3;

/// The `.hidx` path that belongs to a table file: beside it, same stem
/// (`py.hdict` → `py.hidx`).
#[must_use]
pub fn index_path(table: &Path) -> PathBuf {
    table.with_extension("hidx")
}

/// The image's bytes: a `.hidx` mapping, or the buffer
/// [`super::PinyinLexicon::parse`] built. Both are read-only and `Send + Sync`,
/// so one lexicon behind an `Arc` serves every session thread (AGENTS.md §1).
#[derive(Debug)]
pub(crate) enum Image {
    /// Built in memory: the table was parsed by this process.
    Owned(Vec<u8>),
    /// A `.hidx` file, mapped rather than copied.
    Mapped(Mmap),
}

impl Image {
    fn as_slice(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Mapped(map) => map,
        }
    }
}

impl Default for Image {
    fn default() -> Self {
        Self::Owned(Vec::new())
    }
}

/// One section of the image.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Section {
    start: usize,
    len: usize,
}

impl Section {
    fn new(start: usize, len: usize) -> Self {
        Self { start, len }
    }

    fn window(&self) -> Range<usize> {
        self.start..self.start + self.len
    }
}

/// One table row, read out of the image.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Row<'a> {
    /// The word, as the table wrote it (UTF-8 bytes).
    pub text: &'a [u8],
    /// The concatenated code, already normalized (`lve` / `nve`).
    pub code: &'a [u8],
    /// Table frequency; higher comes first.
    pub weight: i64,
    /// 1-based table order (the tie-breaker behind weight).
    pub rank: u32,
    /// Fewest syllables `code` can be read as; 0 when it cannot be read at all.
    pub syllables: usize,
    /// The code's fewest-syllable reading, packed. This is what the candidate
    /// bar displays, and what a finer hit is kept behind.
    pub reading: u128,
}

/// One distinct syllable key and its word slots.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KeySpan {
    /// Syllable ids packed most significant first.
    pub packed: u128,
    /// First slot in the slot array.
    pub start: u32,
    /// Number of slots (distinct texts under this key).
    pub count: u32,
}

/// The flat tables a [`super::PinyinLexicon`] queries: one buffer plus where its
/// four sections start. [`Builder`] assembles it while a table is parsed;
/// [`open_for`] maps a `.hidx`. An all-zero value is a valid empty table.
#[derive(Debug, Default)]
pub(crate) struct Store {
    data: Image,
    blob: Section,
    rows: Section,
    keys: Section,
    slots: Section,
    row_count: usize,
    key_count: usize,
    slot_count: usize,
}

impl Store {
    /// Rows (one per table entry).
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// Distinct syllable keys, ascending by packed value.
    #[must_use]
    pub fn key_count(&self) -> usize {
        self.key_count
    }

    /// Word slots (one per `(key, text)`).
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slot_count
    }

    fn bytes(&self) -> &[u8] {
        self.data.as_slice()
    }

    /// Row `index`, or `None` when a damaged record points outside the image.
    #[must_use]
    pub fn row(&self, index: u32) -> Option<Row<'_>> {
        let at = self.rows.start + (index as usize).checked_mul(ROW_STRIDE)?;
        let record = self.bytes().get(at..at + ROW_STRIDE)?;
        let blob = &self.blob;
        Some(Row {
            text: self.span(record, 0, blob)?,
            code: self.span(record, 8, blob)?,
            weight: i64::from_le_bytes(record[16..24].try_into().ok()?),
            rank: u32::from_le_bytes(record[24..28].try_into().ok()?),
            syllables: u32::from_le_bytes(record[28..32].try_into().ok()?) as usize,
            reading: u128::from_le_bytes(record[32..48].try_into().ok()?),
        })
    }

    /// A `(offset, len)` pair inside the blob; `None` when it runs past the
    /// image.
    fn span<'a>(&'a self, record: &[u8], at: usize, blob: &Section) -> Option<&'a [u8]> {
        let start = u32::from_le_bytes(record[at..at + 4].try_into().ok()?) as usize;
        let len = u32::from_le_bytes(record[at + 4..at + 8].try_into().ok()?) as usize;
        self.bytes().get(blob.start + start..blob.start + start + len)
    }

    /// The `index`th key. [`validate`] bound the key count against the section
    /// length, so this cannot run past the image.
    #[must_use]
    pub fn key(&self, index: usize) -> KeySpan {
        let at = self.keys.start + index * KEY_STRIDE;
        let record = &self.bytes()[at..at + KEY_STRIDE];
        KeySpan {
            packed: u128::from_le_bytes(record[0..16].try_into().unwrap_or([0; 16])),
            start: u32::from_le_bytes(record[16..20].try_into().unwrap_or([0; 4])),
            count: u32::from_le_bytes(record[20..24].try_into().unwrap_or([0; 4])),
        }
    }

    /// The row ids under one key, best word first: `count` little-endian `u32`s.
    /// `None` when a damaged key span runs past the slot array.
    #[must_use]
    pub fn slots(&self, key: &KeySpan) -> Option<&[u8]> {
        let start = self.slots.start + (key.start as usize).checked_mul(SLOT_STRIDE)?;
        let len = (key.count as usize).checked_mul(SLOT_STRIDE)?;
        self.bytes().get(start..start + len)
    }

    /// The row one slot stands for.
    #[must_use]
    pub fn slot_row(&self, slot: &[u8]) -> Option<Row<'_>> {
        self.row(u32::from_le_bytes(slot.try_into().ok()?))
    }

    /// Write the tables as a `.hidx` at `index_path`, declaring `table` as the
    /// file they were built from (SPEC §8.1). The reader refuses an index whose
    /// source no longer matches, so a replaced table stops being served from a
    /// stale index without either caller having to pass the two paths around.
    pub(crate) fn write_index(
        &self,
        index_path: &Path,
        table: &Path,
    ) -> Result<(), hdict::HDictError> {
        let source = hdict::fingerprint(table)?;
        let windows = [
            self.blob.window(),
            self.rows.window(),
            self.keys.window(),
            self.slots.window(),
        ];
        let mut header = [0u8; HEADER_SIZE];
        header[..MAGIC.len()].copy_from_slice(MAGIC);
        write_u32(&mut header, OFF_VERSION, VERSION);
        write_u32(&mut header, OFF_HEADER_SIZE, HEADER_SIZE as u32);
        write_u32(&mut header, OFF_ALIGN, ALIGN as u32);
        write_u16(&mut header, OFF_ROW_STRIDE, ROW_STRIDE as u16);
        write_u16(&mut header, OFF_KEY_STRIDE, KEY_STRIDE as u16);
        write_u32(&mut header, OFF_ROW_COUNT, self.row_count as u32);
        write_u32(&mut header, OFF_KEY_COUNT, self.key_count as u32);
        write_u32(&mut header, OFF_SLOT_COUNT, self.slot_count as u32);
        write_u64(&mut header, OFF_SOURCE_BYTES, source.bytes);
        write_u32(&mut header, OFF_SOURCE_CRC, source.payload_crc);

        let mut starts = [0usize; SECTION_COUNT];
        let mut offset = HEADER_SIZE;
        for (start, window) in starts.iter_mut().zip(&windows) {
            offset = align_up(offset);
            *start = offset;
            offset += window.len();
        }
        for (index, start) in starts.iter().enumerate() {
            write_u64(&mut header, OFF_SECTIONS + 8 * index, *start as u64);
        }
        write_u64(&mut header, OFF_FILE_SIZE, offset as u64);
        let crc = hdict::crc32(&header);
        write_u32(&mut header, OFF_HEADER_CRC, crc);

        let mut file = BufWriter::new(fs::File::create(index_path)?);
        file.write_all(&header)?;
        let mut written = HEADER_SIZE;
        for (start, window) in starts.iter().zip(&windows) {
            write_pad(&mut file, *start - written)?;
            file.write_all(&self.bytes()[window.clone()])?;
            written = *start + window.len();
        }
        file.flush()?;
        Ok(())
    }

    /// The image size in bytes (diagnostics).
    #[must_use]
    pub fn image_bytes(&self) -> usize {
        self.data.as_slice().len()
    }
}

/// Round up to the next section boundary.
fn align_up(offset: usize) -> usize {
    offset.div_ceil(ALIGN) * ALIGN
}

/// Pad to the next section, chunked so the file is never asked for a huge
/// single write.
fn write_pad(file: &mut impl Write, gap: usize) -> Result<(), hdict::HDictError> {
    let mut left = gap;
    while left > 0 {
        let chunk = left.min(ZERO_PAD.len());
        file.write_all(&ZERO_PAD[..chunk])?;
        left -= chunk;
    }
    Ok(())
}

fn write_u16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

/// Assembles the four sections while a table is parsed, then hands them over as
/// one image laid out exactly like the file (SPEC §8.1).
#[derive(Debug, Default)]
pub(crate) struct Builder {
    blob: Vec<u8>,
    rows: Vec<u8>,
    keys: Vec<u8>,
    slots: Vec<u8>,
}

impl Builder {
    /// Append a row with its text and code bytes; returns the row id.
    pub fn push_row(
        &mut self,
        text: &[u8],
        code: &[u8],
        weight: i64,
        rank: u32,
        syllables: u32,
        reading: u128,
    ) -> u32 {
        let text_start = self.blob.len() as u32;
        self.blob.extend_from_slice(text);
        let code_start = self.blob.len() as u32;
        self.blob.extend_from_slice(code);
        debug_assert!(
            self.blob.len() <= u32::MAX as usize,
            "a pinyin table blob stays far below 4 GB"
        );
        self.rows.extend_from_slice(&text_start.to_le_bytes());
        self.rows.extend_from_slice(&(text.len() as u32).to_le_bytes());
        self.rows.extend_from_slice(&code_start.to_le_bytes());
        self.rows.extend_from_slice(&(code.len() as u32).to_le_bytes());
        self.rows.extend_from_slice(&weight.to_le_bytes());
        self.rows.extend_from_slice(&rank.to_le_bytes());
        self.rows.extend_from_slice(&syllables.to_le_bytes());
        self.rows.extend_from_slice(&reading.to_le_bytes());
        debug_assert_eq!(self.rows.len() % ROW_STRIDE, 0);
        (self.rows.len() / ROW_STRIDE - 1) as u32
    }

    /// Append a key span over `count` slots starting at slot `start`.
    pub fn push_key(&mut self, packed: u128, start: u32, count: u32) {
        self.keys.extend_from_slice(&packed.to_le_bytes());
        self.keys.extend_from_slice(&start.to_le_bytes());
        self.keys.extend_from_slice(&count.to_le_bytes());
        debug_assert_eq!(self.keys.len() % KEY_STRIDE, 0);
    }

    /// Append a row id to the slot array.
    pub fn push_slot(&mut self, row: u32) {
        self.slots.extend_from_slice(&row.to_le_bytes());
    }

    /// Concatenate the sections into one image, aligned as the file has them.
    pub fn into_store(self) -> Store {
        let Self { blob, rows, keys, slots } = self;
        let counts = (
            rows.len() / ROW_STRIDE,
            keys.len() / KEY_STRIDE,
            slots.len() / SLOT_STRIDE,
        );
        let parts = [blob, rows, keys, slots];
        let mut data =
            Vec::with_capacity(parts.iter().map(Vec::len).sum::<usize>() + ALIGN * SECTION_COUNT);
        let mut sections = [Section::default(); SECTION_COUNT];
        for (index, part) in parts.iter().enumerate() {
            while data.len() % ALIGN != 0 {
                data.push(0);
            }
            sections[index] = Section::new(data.len(), part.len());
            data.extend_from_slice(part);
        }
        Store {
            data: Image::Owned(data),
            blob: sections[SECTION_BLOB],
            rows: sections[SECTION_ROWS],
            keys: sections[SECTION_KEYS],
            slots: sections[SECTION_SLOTS],
            row_count: counts.0,
            key_count: counts.1,
            slot_count: counts.2,
        }
    }
}

/// The index beside `table`, when one exists, is well formed and says it was
/// built from that very table. `None` means: parse the table instead. An index
/// that is simply absent is not worth reporting; one that is present and
/// unusable is, because the load that follows is then the slow one.
pub(crate) fn open_for(table: &Path) -> Option<Store> {
    let path = index_path(table);
    if !path.is_file() {
        return None;
    }
    let source = hdict::fingerprint(table).ok()?;
    match open(&path, source) {
        Ok(store) => Some(store),
        Err(why) => {
            eprintln!("hime-core: ignoring the pinyin index {}: {why}", path.display());
            None
        }
    }
}

/// Map and validate one `.hidx` file.
fn open(path: &Path, source: hdict::Fingerprint) -> Result<Store, hdict::HDictError> {
    let file = fs::File::open(path)?;
    // SAFETY: the mapping stays valid once `file` is dropped (the OS keeps the
    // inode alive), the file holds read-only data the user owns, and nothing
    // here writes to it. Same assumption the `.hng` reader documents (SPEC §7).
    let data = unsafe { Mmap::map(&file) }.map_err(hdict::HDictError::Io)?;
    let layout = validate(&data, source)?;
    let size = data.len();
    let mut sections = [Section::default(); SECTION_COUNT];
    for (index, section) in sections.iter_mut().enumerate() {
        let end = if index + 1 < SECTION_COUNT {
            layout.starts[index + 1]
        } else {
            size
        };
        *section = Section::new(layout.starts[index], end - layout.starts[index]);
    }
    Ok(Store {
        data: Image::Mapped(data),
        blob: sections[SECTION_BLOB],
        rows: sections[SECTION_ROWS],
        keys: sections[SECTION_KEYS],
        slots: sections[SECTION_SLOTS],
        row_count: layout.row_count,
        key_count: layout.key_count,
        slot_count: layout.slot_count,
    })
}

/// The file layout a header claims: where each section starts and how many
/// records it says it holds. Checked as a whole before anything reads it.
struct Layout {
    starts: [usize; SECTION_COUNT],
    row_count: usize,
    key_count: usize,
    slot_count: usize,
}

/// O(1) header checks: magic, version, strides, the header CRC, the source the
/// index claims, and every count against its section length.
fn validate(data: &[u8], source: hdict::Fingerprint) -> Result<Layout, hdict::HDictError> {
    let size = data.len();
    if size < HEADER_SIZE || &data[..MAGIC.len()] != MAGIC {
        return Err(hdict::HDictError::Format("not a .hidx index".into()));
    }
    let u16_at = |at: usize| u16::from_le_bytes(data[at..at + 2].try_into().unwrap());
    let u32_at = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
    let u64_at = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
    let bad = |why: String| hdict::HDictError::Format(why);

    if u32_at(OFF_VERSION) != VERSION {
        return Err(bad(format!("unsupported index version {}", u32_at(OFF_VERSION))));
    }
    if u32_at(OFF_HEADER_SIZE) as usize != HEADER_SIZE {
        return Err(bad("header size".into()));
    }
    if u64_at(OFF_FILE_SIZE) as usize != size {
        return Err(bad(format!("declared {} bytes, file is {size}", u64_at(OFF_FILE_SIZE))));
    }
    if u32_at(OFF_ALIGN) != ALIGN as u32 {
        return Err(bad("alignment".into()));
    }
    if u16_at(OFF_ROW_STRIDE) != ROW_STRIDE as u16 || u16_at(OFF_KEY_STRIDE) != KEY_STRIDE as u16
    {
        return Err(bad("record strides".into()));
    }
    let mut header = [0u8; HEADER_SIZE];
    header.copy_from_slice(&data[..HEADER_SIZE]);
    let stored = u32_at(OFF_HEADER_CRC);
    header[OFF_HEADER_CRC..OFF_HEADER_CRC + 4].fill(0);
    let computed = hdict::crc32(&header);
    if computed != stored {
        return Err(bad(format!(
            "header CRC mismatch (stored {stored:#010x}, computed {computed:#010x})"
        )));
    }
    if u64_at(OFF_SOURCE_BYTES) != source.bytes
        || u32_at(OFF_SOURCE_CRC) != source.payload_crc
    {
        return Err(bad(format!(
            "built from a different table (index saw {}/{}, the table is now {}/{})",
            u64_at(OFF_SOURCE_BYTES),
            source.bytes,
            u32_at(OFF_SOURCE_CRC),
            source.payload_crc
        )));
    }

    let row_count = u32_at(OFF_ROW_COUNT) as usize;
    let key_count = u32_at(OFF_KEY_COUNT) as usize;
    let slot_count = u32_at(OFF_SLOT_COUNT) as usize;
    if row_count == 0 || key_count == 0 || slot_count < key_count {
        return Err(bad(format!(
            "{row_count} rows, {key_count} keys and {slot_count} slots are not a table"
        )));
    }

    let mut starts = [0usize; SECTION_COUNT];
    for (index, start) in starts.iter_mut().enumerate() {
        let Ok(value) = usize::try_from(u64_at(OFF_SECTIONS + 8 * index)) else {
            return Err(bad(format!("section {index} start is off")));
        };
        *start = value;
        if *start < HEADER_SIZE || *start % ALIGN != 0 {
            return Err(bad(format!(
                "section {index} starts at {start}, before or misaligned with the header"
            )));
        }
    }
    for index in 0..SECTION_COUNT {
        let end = if index + 1 < SECTION_COUNT { starts[index + 1] } else { size };
        if starts[index] > end || end > size {
            return Err(bad(format!(
                "section {index} spans {}..{end} of a {size}-byte file",
                starts[index]
            )));
        }
        let want = match index {
            SECTION_ROWS => row_count.saturating_mul(ROW_STRIDE),
            SECTION_KEYS => key_count.saturating_mul(KEY_STRIDE),
            SECTION_SLOTS => slot_count.saturating_mul(SLOT_STRIDE),
            _ => continue,
        };
        // Section ends carry the next section's alignment pad, so a correct
        // writer lands within `ALIGN` bytes of the exact length.
        if !(want..want + ALIGN).contains(&(end - starts[index])) {
            return Err(bad(format!(
                "section {index} holds {} bytes, {want} are needed for its count",
                end - starts[index]
            )));
        }
    }
    Ok(Layout { starts, row_count, key_count, slot_count })
}
