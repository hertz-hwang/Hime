//! Hime input engine core ("Fat Core, Thin Shell", AGENTS.md §1).
//!
//! All typing state and candidate retrieval logic live in this crate. Platform
//! shells forward key codes to [`Session::process_key`] and render the
//! returned [`EngineResponse`] verbatim; they never keep typing state of
//! their own.
//!
//! Retrieval is a pipeline with three roles, so a pinyin source can be added
//! beside the 形码 table without touching the state machine:
//!
//! - [`Processor`] reads a key plus the current composition and decides what
//!   the key means ([`Action`]): append to the encoding, delete, select,
//!   abandon, do nothing, or pass the key to the host.
//! - [`Translator`] turns an encoding into candidates. [`TableTranslator`]
//!   covers the TSV lexicon; a `PinyinTranslator` is the planned second source.
//! - [`Filter`] post-processes the merged candidates: [`DedupFilter`] today,
//!   ranking, script conversion or emoji injection later.
//!
//! [`Pipeline`] wires the configured translators and filters together and is
//! the only thing `Session` consults about candidates.

pub mod config;
pub mod dictionary;
pub mod filter;
pub mod ngram;
pub mod paths;
pub mod pipeline;
pub mod processor;
pub mod punct;
pub mod scheme;
pub mod sentence;
pub mod session;
pub mod translator;
pub mod types;

pub use config::{CodeType, Config, MAX_CODE_LEN_LIMIT, PAGE_SIZE_MAX};
pub use dictionary::{Dictionary, TableFormat};
pub use filter::{DedupFilter, Filter, LimitFilter};
pub use ngram::NgramModel;
pub use paths::{config_dir, ensure_config_layout, model_path, schemes_dir};
pub use pipeline::Pipeline;
pub use processor::{Action, Processor};
pub use scheme::{load_scheme, scan_schemes, SchemeError, SchemeInfo, SchemeMetadata};
pub use session::Session;
pub use translator::{TableTranslator, Translator};
pub use types::{Candidate, EngineResponse};

// Virtual key codes (SPEC §2.1). Printable keys use their ASCII code:
// letters are 97..=122, digits are 49..=57.
pub const KEY_BACKSPACE: u32 = 8;
pub const KEY_ENTER: u32 = 13;
pub const KEY_ESCAPE: u32 = 27;
pub const KEY_SPACE: u32 = 32;

/// Candidate paging keys (`SPEC` §3): `-` turns to the previous page,
/// `=` to the next. They carry their own ASCII code (the shell forwards
/// printable keys unchanged) and only take effect while a candidate is shown.
pub const KEY_PAGE_UP: u32 = 45; // b'-'
pub const KEY_PAGE_DOWN: u32 = 61; // b'='

/// Default maximum length of a Xingma encoding (`Config::default`).
pub const MAX_CODE_LEN: usize = 4;

/// Default candidates per page (`Config::default`); the user may set a
/// different page size through [`Config`].
pub const MAX_CANDIDATES: usize = 5;

/// Candidates each translator is asked for, before filters run.
///
/// Deliberately larger than [`MAX_CANDIDATES`]: filters drop entries (see
/// [`DedupFilter`]) and add others, so retrieving exactly one page would let
/// the dropped entries shrink the page instead of being replaced by the next
/// matches.
pub const FETCH_CANDIDATES: usize = 32;
