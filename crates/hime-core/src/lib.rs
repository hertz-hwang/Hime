//! Hime input engine core ("Fat Core, Thin Shell", AGENTS.md §1).
//!
//! All typing state and dictionary lookup logic live in this crate. Platform
//! shells forward key codes to [`Session::process_key`] and render the
//! returned [`EngineResponse`] verbatim; they never keep typing state of
//! their own.

pub mod dictionary;
pub mod session;
pub mod types;

pub use dictionary::Dictionary;
pub use session::Session;
pub use types::{Candidate, EngineResponse};

// Virtual key codes (SPEC_MVP §2.1). Printable keys use their ASCII code:
// letters are 97..=122, digits are 49..=57.
pub const KEY_BACKSPACE: u32 = 8;
pub const KEY_ENTER: u32 = 13;
pub const KEY_ESCAPE: u32 = 27;
pub const KEY_SPACE: u32 = 32;

/// Maximum length of a Xingma encoding.
pub const MAX_CODE_LEN: usize = 4;

/// Number of candidates returned to the shell in each response.
pub const MAX_CANDIDATES: usize = 5;
