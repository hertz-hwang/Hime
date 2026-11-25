//! Input schemes: the `metadata.toml` beside each table and the discovery of
//! scheme folders under `data/schemes/` (SPEC §5).
//!
//! A scheme is one folder holding a `metadata.toml` (name, description,
//! author, code type and the engine defaults the preferences window exposes)
//! and its table file. New keys may be added to `metadata.toml` at any time:
//! unknown keys are ignored, missing keys keep their defaults, so an older
//! engine still loads a newer scheme.
//!
//! The values in `metadata.toml` are the scheme's *defaults*: they become the
//! engine [`Config`] when the scheme is selected, the user may then change
//! them, and selecting a scheme reloads them.

use std::fmt;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{CodeType, Config};
use crate::MAX_CODE_LEN;

/// Metadata one scheme folder declares in its `metadata.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchemeMetadata {
    /// Scheme-table name shown in the picker; blank falls back to the folder
    /// name. Fixed on the panel.
    pub name: String,
    /// Free-form description; fixed on the panel.
    pub describe: String,
    /// Author; fixed on the panel.
    pub author: String,
    /// `type`: 0 全拼 / 1 双拼 / 2 形码 / 3 形码整句 (see [`CodeType`]).
    #[serde(rename = "type")]
    pub code_type: CodeType,
    /// Retrieval cap: encodings longer than this are invalid.
    pub max_code: usize,
    /// Commit automatically when the code is long enough and only one
    /// candidate is left.
    pub auto_commit: bool,
    /// Code length [`Self::auto_commit`] needs; inert while auto-commit is
    /// off.
    pub auto_commit_length: usize,
    /// N-th-letter overflow commit (see [`Config::commit_next_code`]);
    /// `0` turns it off.
    pub commit_next_code: usize,
    /// Punctuation output (see [`Config::ascii_punct`]): `0` English
    /// punctuation, `1` Chinese punctuation.
    pub ascii_punct: u8,
}

impl Default for SchemeMetadata {
    fn default() -> Self {
        Self {
            name: String::new(),
            describe: String::new(),
            author: String::new(),
            code_type: CodeType::Xingma,
            max_code: MAX_CODE_LEN,
            auto_commit: false,
            auto_commit_length: MAX_CODE_LEN,
            commit_next_code: 0,
            ascii_punct: 0,
        }
    }
}

impl SchemeMetadata {
    /// The engine configuration this scheme prescribes: every behavior key
    /// comes from the metadata, only the keys schemes do not define —
    /// `page_size`, the two punctuation preference switches, the preedit
    /// placement and the candidate layout — are carried over from `base`.
    /// Out-of-range metadata is clamped like any other config.
    #[must_use]
    pub fn defaults_for(&self, base: Config) -> Config {
        Config {
            page_size: base.page_size,
            max_code_len: self.max_code,
            code_type: self.code_type,
            auto_commit: self.auto_commit,
            auto_commit_length: self.auto_commit_length,
            commit_next_code: self.commit_next_code,
            ascii_punct: self.ascii_punct,
            english_punct_after_alnum: base.english_punct_after_alnum,
            semicolon_quote_select: base.semicolon_quote_select,
            show_code_in_window: base.show_code_in_window,
            vertical_candidates: base.vertical_candidates,
        }
        .sanitized()
    }
}

/// A scheme folder that loaded successfully: metadata plus where it lives.
/// Serializes flat for the shell picker (`dir` is the stable id).
#[derive(Debug, Clone, Serialize)]
pub struct SchemeInfo {
    /// Folder name; identifies the scheme across reinstalls.
    pub dir: String,
    /// Absolute path of the scheme folder, for [`load_scheme`].
    pub path: String,
    /// Absolute path of the table file inside the folder.
    pub table: String,
    #[serde(flatten)]
    pub metadata: SchemeMetadata,
}

/// Errors from loading one scheme folder.
#[derive(Debug)]
pub enum SchemeError {
    Io(std::io::Error),
    /// `metadata.toml` is not valid TOML or has a wrong-typed value.
    Metadata(String),
    /// The folder holds no `*.txt` or `*.hdict` table file.
    NoTable,
}

impl fmt::Display for SchemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchemeError::Io(e) => write!(f, "io error: {e}"),
            SchemeError::Metadata(message) => write!(f, "metadata.toml: {message}"),
            SchemeError::NoTable => write!(f, "no *.txt or *.hdict table file in the scheme folder"),
        }
    }
}

impl std::error::Error for SchemeError {}

impl From<std::io::Error> for SchemeError {
    fn from(e: std::io::Error) -> Self {
        SchemeError::Io(e)
    }
}

/// Load one scheme folder: its `metadata.toml` plus the first `*.txt` file
/// as table. A blank `name` falls back to the folder name.
pub fn load_scheme(dir: &Path) -> Result<SchemeInfo, SchemeError> {
    let metadata_text = fs::read_to_string(dir.join("metadata.toml"))?;
    let mut metadata: SchemeMetadata =
        toml::from_str(&metadata_text).map_err(|e| SchemeError::Metadata(e.to_string()))?;

    let dir_name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned());
    if metadata.name.trim().is_empty() {
        metadata.name = dir_name.clone();
    }

    let table = find_table(dir)?;
    Ok(SchemeInfo { dir: dir_name, path: dir.display().to_string(), table, metadata })
}

/// The table file of a scheme folder: the alphabetically first `*.txt`, or
/// — when the folder holds no `.txt` — the alphabetically first `*.hdict`.
/// A pinyin scheme ships its table as a compressed `.hdict` container
/// (SPEC §8.1); 形码 and 形码整句 schemes keep using plain text tables, and a
/// `.txt` beside an `.hdict` still wins so a user can drop a readable table
/// into an existing folder.
fn find_table(dir: &Path) -> Result<String, SchemeError> {
    let mut names: Vec<String> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let path = entry.path();
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext == "txt" || ext == "hdict")
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort_by_key(|name| (table_rank(name), name.clone()));
    names
        .first()
        .map(|name| dir.join(name).display().to_string())
        .ok_or(SchemeError::NoTable)
}

/// `.txt` tables outrank `.hdict` containers; the sort above is alphabetical
/// inside each rank.
fn table_rank(name: &str) -> u8 {
    if name.ends_with(".txt") { 0 } else { 1 }
}

/// Discover the schemes under `dir` (the `data/schemes/` layout), sorted by
/// folder name. Plain subfolders without a `metadata.toml` are not schemes;
/// a folder whose metadata or table fails to load is skipped with a note to
/// stderr so one broken scheme cannot hide the others.
pub fn scan_schemes(dir: &Path) -> Vec<SchemeInfo> {
    let mut folders: Vec<_> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().is_dir() && entry.path().join("metadata.toml").is_file())
                .map(|entry| entry.file_name())
                .collect()
        })
        .unwrap_or_default();
    folders.sort();

    folders
        .iter()
        .filter_map(|folder| {
            let path = dir.join(folder);
            match load_scheme(&path) {
                Ok(info) => Some(info),
                Err(e) => {
                    eprintln!("hime-core: skipping scheme {path:?}: {e}");
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scheme_root() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/schemes"))
    }

    /// A scratch folder that does not collide with other tests or runs.
    fn temp_folder(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hime-scheme-test-{}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp folder");
        dir
    }

    /// The metadata shipped with the repository is the contract for the real
    /// values: 琉璃v1 is a 形码 scheme with overflow commit on the 5th key.
    #[test]
    fn parses_the_shipped_liuli_metadata() {
        let info = load_scheme(&scheme_root().join("liuli_v1")).expect("shipped scheme loads");
        assert_eq!(info.dir, "liuli_v1");
        assert_eq!(info.metadata.name, "琉璃字词");
        assert_eq!(info.metadata.describe, "单字三码，词组四码");
        assert_eq!(info.metadata.author, "荒");
        assert_eq!(info.metadata.code_type, CodeType::Xingma);
        assert_eq!(info.metadata.max_code, 4);
        assert!(!info.metadata.auto_commit);
        assert_eq!(info.metadata.auto_commit_length, 4);
        assert_eq!(info.metadata.commit_next_code, 5);
        assert_eq!(info.metadata.ascii_punct, 1, "琉璃v1 types Chinese punctuation");
        assert!(info.table.ends_with("liuli_table.txt"), "{}", info.table);
        assert!(Path::new(&info.table).is_file());
    }

    #[test]
    fn scans_the_shipped_scheme_directory() {
        let schemes = scan_schemes(&scheme_root());
        assert!(schemes.iter().any(|s| s.dir == "liuli_v1"), "{schemes:?}");
        let json = serde_json::to_string(&schemes).unwrap();
        assert!(json.contains(r#""name":"琉璃字词""#), "{json}");
        assert!(json.contains(r#""type":2"#), "{json}");
    }

    /// The shipped 全拼 scheme keeps its table in a `.hdict` container and
    /// declares `type = 0`; scanning must find it without any `.txt` in the
    /// folder.
    #[test]
    fn parses_the_shipped_pinyin_scheme() {
        let info = load_scheme(&scheme_root().join("pinyin")).expect("pinyin scheme loads");
        assert_eq!(info.dir, "pinyin");
        assert_eq!(info.metadata.name, "全拼");
        assert_eq!(info.metadata.code_type, CodeType::Quanpin);
        assert!(info.table.ends_with("py.hdict"), "{}", info.table);
        assert!(
            scan_schemes(&scheme_root()).iter().any(|s| s.dir == "pinyin"),
            "the scheme list includes 全拼"
        );
    }

    /// A `.txt` beside a `.hdict` wins: the readable table is what the user
    /// dropped in on purpose.
    #[test]
    fn a_txt_table_outranks_an_hdict_in_the_same_folder() {
        let dir = temp_folder("prefer-txt");
        std::fs::write(dir.join("metadata.toml"), "type = 0\n").unwrap();
        std::fs::write(dir.join("a.hdict"), b"not read").unwrap();
        std::fs::write(dir.join("z.txt"), "在\ta\n").unwrap();
        let info = load_scheme(&dir).unwrap();
        assert!(info.table.ends_with("z.txt"), "{}", info.table);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_keys_default_and_unknown_keys_are_ignored() {
        let dir = temp_folder("minimal");
        fs::write(dir.join("metadata.toml"), "name = \"甲\"\nfuture_key = 7\n").unwrap();
        fs::write(dir.join("a_table.txt"), "在\ta\n").unwrap();

        let info = load_scheme(&dir).unwrap();
        assert_eq!(info.metadata.name, "甲");
        assert_eq!(info.metadata.describe, "");
        assert_eq!(info.metadata.code_type, CodeType::Xingma);
        assert_eq!(info.metadata.max_code, MAX_CODE_LEN);
        assert!(!info.metadata.auto_commit);
        assert_eq!(info.metadata.commit_next_code, 0);
        assert_eq!(info.metadata.ascii_punct, 0, "a missing key keeps English punctuation");
        assert_eq!(info.table, dir.join("a_table.txt").display().to_string());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn blank_name_falls_back_to_the_folder_name() {
        let dir = temp_folder("blank");
        fs::write(dir.join("metadata.toml"), "describe = \"x\"\n").unwrap();
        fs::write(dir.join("table.txt"), "在\ta\n").unwrap();
        let info = load_scheme(&dir).unwrap();
        assert_eq!(info.metadata.name, dir.file_name().unwrap().to_str().unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn broken_or_incomplete_folders_are_reported() {
        let dir = temp_folder("broken");
        fs::write(dir.join("metadata.toml"), "name = [[oops\n").unwrap();
        fs::write(dir.join("t.txt"), "在\ta\n").unwrap();
        assert!(matches!(load_scheme(&dir), Err(SchemeError::Metadata(_))));

        fs::write(dir.join("metadata.toml"), "name = \"x\"\n").unwrap();
        fs::remove_file(dir.join("t.txt")).unwrap();
        assert!(matches!(load_scheme(&dir), Err(SchemeError::NoTable)));

        fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(load_scheme(&dir), Err(SchemeError::Io(_))));
    }

    #[test]
    fn scan_skips_folders_without_usable_schemes() {
        let root = temp_folder("scan");
        let good = root.join("b_good");
        fs::create_dir_all(&good).unwrap();
        fs::write(good.join("metadata.toml"), "name = \"好\"\n").unwrap();
        fs::write(good.join("good.txt"), "在\ta\n").unwrap();
        fs::create_dir_all(root.join("plain_no_meta")).unwrap();
        let broken = root.join("a_broken");
        fs::create_dir_all(&broken).unwrap();
        fs::write(broken.join("metadata.toml"), "type = \"no number\"\n").unwrap();
        fs::write(broken.join("broken.txt"), "在\ta\n").unwrap();

        let schemes = scan_schemes(&root);
        assert_eq!(schemes.len(), 1, "only the loadable scheme survives: {schemes:?}");
        assert_eq!(schemes[0].dir, "b_good");

        assert!(scan_schemes(&root.join("does_not_exist")).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn defaults_replace_behavior_keys_and_keep_page_size() {
        let metadata = SchemeMetadata {
            max_code: 6,
            code_type: CodeType::Shuangpin,
            auto_commit: true,
            auto_commit_length: 3,
            commit_next_code: 7,
            ascii_punct: 1,
            ..SchemeMetadata::default()
        };
        let base = Config {
            page_size: 8,
            english_punct_after_alnum: true,
            semicolon_quote_select: true,
            vertical_candidates: true,
            ..Config::default()
        };
        let config = metadata.defaults_for(base);
        assert_eq!(config.page_size, 8, "page size survives a scheme switch");
        assert!(
            config.english_punct_after_alnum && config.semicolon_quote_select,
            "preference switches survive a scheme switch"
        );
        assert!(config.vertical_candidates, "the candidate layout survives a scheme switch");
        assert_eq!(config.max_code_len, 6);
        assert_eq!(config.code_type, CodeType::Shuangpin);
        assert!(config.auto_commit);
        assert_eq!(config.auto_commit_length, 3);
        assert_eq!(config.commit_next_code, 7);
        assert_eq!(config.ascii_punct, 1);

        // Out-of-range metadata is clamped, not rejected.
        let wild = metadata.defaults_for(Config { page_size: 0, ..Config::default() });
        assert_eq!(wild.max_code_len, 6);
        assert_eq!(wild.page_size, 1);

        let bad = SchemeMetadata { max_code: 0, commit_next_code: 1, ..SchemeMetadata::default() };
        let clamped = bad.defaults_for(Config::default());
        assert_eq!(clamped.max_code_len, 1);
        assert_eq!(clamped.commit_next_code, 0);
    }
}
