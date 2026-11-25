//! `.hdict` lexicon container (SPEC §8.1).
//!
//! The pinyin table ships as a `.hdict` file: a fixed 64-byte little-endian
//! header followed by one payload block, raw-deflate compressed. The payload
//! is the UTF-8 row stream `候选\tcode[\tweight]`. The format is the
//! reference implementation's private dictionary container; its builder writes
//! the same header (`scripts/build_pinyin_hdict.py` there).
//!
//! Only the fields a reader needs are validated: magic, version, codec,
//! declared lengths and the header CRC. The payload CRC is a full-file scan
//! and stays out of the load path (the reference verifies it only in its
//! self-check), so a recompressed table whose CRC was not refreshed still
//! loads.

use std::fmt;
use std::fs;
use std::path::Path;

/// Fixed header size in bytes.
pub const HEADER_LEN: usize = 64;

const MAGIC: [u8; 4] = *b"HDCT";
const VERSION: u16 = 1;
const CODEC_STORED: u16 = 0;
const CODEC_DEFLATE: u16 = 1;
/// Guard against a corrupt header asking for a multi-gigabyte allocation.
const MAX_PAYLOAD_BYTES: u64 = 512 << 20;

/// A decoded `.hdict` container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HDict {
    /// Payload text: `候选\tcode[\tweight]` rows, `\n` separated.
    pub text: String,
    /// Word entries the builder declared (`entry_count`), for diagnostics.
    pub entry_count: usize,
    /// Payload kind the builder wrote (`1` pinyin, `2` shuangpin).
    pub kind: u16,
    /// Header flags (`bit 0` weight column, `bit 1` canonical rows).
    pub flags: u16,
}

impl HDict {
    /// Whether the payload carries the optional weight column.
    #[must_use]
    pub const fn has_weight_column(&self) -> bool {
        self.flags & 1 != 0
    }

    /// Whether the builder declared the rows trimmed, `a-z` only and
    /// de-duplicated by `(text, code)`.
    #[must_use]
    pub const fn has_canonical_rows(&self) -> bool {
        self.flags & 2 != 0
    }
}

/// Why a `.hdict` file could not be read.
#[derive(Debug)]
pub enum HDictError {
    Io(std::io::Error),
    /// Header, lengths or encoding are inconsistent.
    Format(String),
    /// The raw-deflate payload did not inflate.
    Inflate(String),
}

impl fmt::Display for HDictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Format(message) => write!(f, "format: {message}"),
            Self::Inflate(message) => write!(f, "inflate: {message}"),
        }
    }
}

impl std::error::Error for HDictError {}

impl From<std::io::Error> for HDictError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Read a `.hdict` file: validate the header, inflate the payload, decode it
/// as UTF-8. The payload CRC is not checked (see the module note).
pub fn load(path: impl AsRef<Path>) -> Result<HDict, HDictError> {
    let bytes = fs::read(path)?;
    parse(&bytes)
}

/// Read a lexicon file as text: `.hdict` containers are inflated, every other
/// file is read as UTF-8. Detection uses the header magic, so a renamed file
/// still loads correctly.
pub fn load_table_text(path: impl AsRef<Path>) -> Result<String, HDictError> {
    let bytes = fs::read(path)?;
    if bytes.len() >= 4 && bytes[..4] == MAGIC {
        return parse(&bytes).map(|dict| dict.text);
    }
    String::from_utf8(bytes)
        .map_err(|_| HDictError::Format("table text is not valid UTF-8".into()))
}

/// Parse a complete container already in memory.
pub fn parse(bytes: &[u8]) -> Result<HDict, HDictError> {
    if bytes.len() < HEADER_LEN {
        return Err(HDictError::Format(format!(
            "file is {} bytes, shorter than the {HEADER_LEN}-byte header",
            bytes.len()
        )));
    }
    if bytes[..4] != MAGIC {
        return Err(HDictError::Format("magic is not HDCT".into()));
    }
    let version = u16_at(bytes, 4);
    if version != VERSION {
        return Err(HDictError::Format(format!("unsupported container version {version}")));
    }
    let kind = u16_at(bytes, 6);
    let codec = u16_at(bytes, 8);
    let flags = u16_at(bytes, 10);
    let entry_count = u32_at(bytes, 12) as usize;
    let payload_bytes = u64_at(bytes, 16);
    let stored_bytes = u64_at(bytes, 24);
    let header_crc = u32_at(bytes, 36);

    let mut header = [0u8; HEADER_LEN];
    header.copy_from_slice(&bytes[..HEADER_LEN]);
    header[36..40].fill(0);
    let computed = crc32(&header);
    if computed != header_crc {
        return Err(HDictError::Format(format!(
            "header CRC mismatch (stored {header_crc:#010x}, computed {computed:#010x})"
        )));
    }
    if payload_bytes > MAX_PAYLOAD_BYTES {
        return Err(HDictError::Format(format!(
            "declared payload {payload_bytes} bytes exceeds the {MAX_PAYLOAD_BYTES}-byte limit"
        )));
    }
    let stored_end = HEADER_LEN
        .checked_add(usize::try_from(stored_bytes).map_err(|_| {
            HDictError::Format(format!("stored size {stored_bytes} does not fit in memory"))
        })?)
        .ok_or_else(|| HDictError::Format("stored size overflows".into()))?;
    if bytes.len() < stored_end {
        return Err(HDictError::Format(format!(
            "payload is truncated: {} bytes present, {stored_end} declared",
            bytes.len()
        )));
    }
    if codec != CODEC_STORED && codec != CODEC_DEFLATE {
        return Err(HDictError::Format(format!("unknown codec {codec}")));
    }
    if codec == CODEC_STORED && stored_bytes != payload_bytes {
        return Err(HDictError::Format(
            "uncompressed payload length disagrees with the stored length".into(),
        ));
    }

    let payload = &bytes[HEADER_LEN..stored_end];
    let decoded = if codec == CODEC_DEFLATE {
        let limit = usize::try_from(payload_bytes)
            .map_err(|_| HDictError::Format("payload size does not fit in memory".into()))?;
        miniz_oxide::inflate::decompress_to_vec_with_limit(payload, limit)
            .map_err(|e| HDictError::Inflate(format!("{e:?}")))?
    } else {
        payload.to_vec()
    };
    if decoded.len() != payload_bytes as usize {
        return Err(HDictError::Format(format!(
            "inflated {} bytes, header declares {payload_bytes}",
            decoded.len()
        )));
    }
    let text = String::from_utf8(decoded)
        .map_err(|_| HDictError::Format("payload is not valid UTF-8".into()))?;
    Ok(HDict { text, entry_count, kind, flags })
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(raw)
}

/// CRC-32/IEEE (the same value as `zlib.crc32`), used only for the header.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a container with a raw-deflate payload, mirroring the builder.
    fn container(text: &str, kind: u16, flags: u16) -> Vec<u8> {
        let payload = text.as_bytes();
        let stored = miniz_oxide::deflate::compress_to_vec(payload, 9);
        let mut header = [0u8; HEADER_LEN];
        header[..4].copy_from_slice(&MAGIC);
        header[4..6].copy_from_slice(&VERSION.to_le_bytes());
        header[6..8].copy_from_slice(&kind.to_le_bytes());
        header[8..10].copy_from_slice(&CODEC_DEFLATE.to_le_bytes());
        header[10..12].copy_from_slice(&flags.to_le_bytes());
        header[12..16].copy_from_slice(&1u32.to_le_bytes());
        header[16..24].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        header[24..32].copy_from_slice(&(stored.len() as u64).to_le_bytes());
        header[32..36].copy_from_slice(&crc32(payload).to_le_bytes());
        header[36..40].fill(0);
        let crc = crc32(&header);
        header[36..40].copy_from_slice(&crc.to_le_bytes());
        let mut out = header.to_vec();
        out.extend_from_slice(&stored);
        out
    }

    #[test]
    fn crc_matches_the_reference_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn parses_a_deflate_container() {
        let bytes = container("的\tde\t100\n你好\tnihao\t90\n", 1, 3);
        let dict = parse(&bytes).expect("container parses");
        assert_eq!(dict.text, "的\tde\t100\n你好\tnihao\t90\n");
        assert_eq!(dict.entry_count, 1);
        assert_eq!(dict.kind, 1);
        assert!(dict.has_weight_column());
        assert!(dict.has_canonical_rows());
    }

    #[test]
    fn parses_a_stored_container() {
        let payload = b"a\tb\n";
        let mut header = [0u8; HEADER_LEN];
        header[..4].copy_from_slice(&MAGIC);
        header[4..6].copy_from_slice(&VERSION.to_le_bytes());
        header[8..10].copy_from_slice(&CODEC_STORED.to_le_bytes());
        header[16..24].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        header[24..32].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        header[36..40].fill(0);
        let crc = crc32(&header);
        header[36..40].copy_from_slice(&crc.to_le_bytes());
        let mut bytes = header.to_vec();
        bytes.extend_from_slice(payload);
        assert_eq!(parse(&bytes).unwrap().text, "a\tb\n");
    }

    #[test]
    fn rejects_broken_headers_and_payloads() {
        let good = container("的\tde\n", 1, 0);

        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert!(matches!(parse(&bad_magic), Err(HDictError::Format(_))));

        let mut bad_version = good.clone();
        bad_version[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(parse(&bad_version), Err(HDictError::Format(_))));

        let mut bad_crc = good.clone();
        bad_crc[20] ^= 0xFF;
        assert!(matches!(parse(&bad_crc), Err(HDictError::Format(_))));

        let mut truncated = good.clone();
        truncated.truncate(HEADER_LEN);
        assert!(matches!(parse(&truncated), Err(HDictError::Format(_))));

        assert!(matches!(parse(b"short"), Err(HDictError::Format(_))));

        let mut bad_codec = good.clone();
        bad_codec[8..10].copy_from_slice(&7u16.to_le_bytes());
        let crc = crc32(&bad_codec[..HEADER_LEN]);
        bad_codec[36..40].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(parse(&bad_codec), Err(HDictError::Format(_))));
    }

    #[test]
    fn loads_the_shipped_pinyin_table() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/schemes/pinyin/py.hdict");
        let dict = load(path).expect("shipped pinyin table loads");
        assert_eq!(dict.kind, 1);
        assert!(dict.entry_count > 900_000, "{} entries", dict.entry_count);
        assert!(dict.text.starts_with("的\tde\t"), "{}", &dict.text[..20]);
        assert!(dict.text.contains("你好\tnihao\t"));
    }
}
