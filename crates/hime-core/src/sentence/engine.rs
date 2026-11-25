//! The engine facade for 形码整句 (whole-sentence shape codes).
//!
//! [`SentenceEngine`] owns the two loaded resources a sentence scheme needs —
//! the HFNGMF n-gram model (see [`crate::ngram`]) and the shape-code word graph
//! (see [`super::lexicon`]) — and answers an encoding with whole-sentence
//! candidates by running [`super::decoder`] over the two. It is built once at
//! scheme-load time and shared across sessions behind an `Arc`; decoding is
//! stateless, so one engine is safe to call from every thread (AGENTS.md §1).
//!
//! [`SentenceTranslator`] adapts the engine to the [`Translator`] role, so a
//! sentence scheme plugs the same candidate pipeline the rest of the engine
//! uses: [`Session`](crate::session::Session) lists, pages and commits the
//! sentences exactly like table words.

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use super::decoder::{SentenceCandidate, SentenceDecoder};
use super::lexicon::SentenceLexicon;
use crate::ngram::NgramModel;
use crate::translator::Translator;
use crate::types::Candidate;

/// Why a sentence engine could not be built from its two files.
#[derive(Debug)]
pub enum SentenceModelError {
    /// The `.hng` model could not be read or was not a valid HFNGMF v1 file.
    Model(crate::ngram::LoadError),
    /// The sentence table could not be read.
    Table(std::io::Error),
}

impl fmt::Display for SentenceModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Model(e) => write!(f, "sentence model: {e}"),
            Self::Table(e) => write!(f, "sentence table: {e}"),
        }
    }
}

impl std::error::Error for SentenceModelError {}

impl From<crate::ngram::LoadError> for SentenceModelError {
    fn from(e: crate::ngram::LoadError) -> Self {
        Self::Model(e)
    }
}

impl From<std::io::Error> for SentenceModelError {
    fn from(e: std::io::Error) -> Self {
        Self::Table(e)
    }
}

/// The loaded whole-sentence model and word graph, shared behind an `Arc`.
#[derive(Debug)]
pub struct SentenceEngine {
    model: NgramModel,
    lexicon: SentenceLexicon,
}

impl SentenceEngine {
    /// Assemble from already-loaded parts.
    pub fn new(model: NgramModel, lexicon: SentenceLexicon) -> Self {
        Self { model, lexicon }
    }

    /// Load the `.hng` model and the sentence table (the same `候选\t编码`
    /// layout as a scheme table) from disk.
    pub fn load(
        model_path: impl AsRef<Path>,
        table_path: impl AsRef<Path>,
    ) -> Result<Self, SentenceModelError> {
        let model = NgramModel::load(model_path)?;
        let lexicon = SentenceLexicon::load(table_path)?;
        Ok(Self { model, lexicon })
    }

    /// Whole-sentence candidates for `code`, best first. `context` is the text
    /// left of the cursor, used only as model history. Applies the reference default
    /// 单字重码组句 (`allow_duplicate_single` on).
    #[must_use]
    pub fn decode(&self, code: &str, context: &str) -> Vec<SentenceCandidate> {
        SentenceDecoder::decode(&self.model, &self.lexicon, code, context, true)
    }

    /// Whole-sentence candidates for `code` plus the early-commit evidence the
    /// probabilistic auto-commit consumes (SPEC §7.6). The decode is identical
    /// to [`Self::decode`]; only the prefix-maturity evidence is extra.
    #[must_use]
    pub(crate) fn decode_with_evidence(
        &self,
        code: &str,
        context: &str,
    ) -> super::decoder::SentenceDecode {
        SentenceDecoder::decode_with_evidence(&self.model, &self.lexicon, code, context, true)
    }

    /// The word graph, for the auto-commit's "can the keys still be covered
    /// completely" check (SPEC §7.6).
    #[must_use]
    pub(crate) fn lexicon(&self) -> &SentenceLexicon {
        &self.lexicon
    }

    /// Whether the word graph is empty; an empty engine decodes to nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lexicon.is_empty()
    }
}

/// A [`Translator`] that reports whole-sentence candidates, the candidate source
/// a 形码整句 scheme installs in its [`Pipeline`](crate::pipeline::Pipeline).
///
/// The sentence text is what the user commits; the ␣-joined segmentation the
/// decoder computes is not shown (the engine consumes the whole encoding, so
/// there is no remaining-coding hint like a table match carries).
#[derive(Debug, Clone)]
pub struct SentenceTranslator {
    engine: Arc<SentenceEngine>,
}

impl SentenceTranslator {
    pub fn new(engine: Arc<SentenceEngine>) -> Self {
        Self { engine }
    }
}

impl Translator for SentenceTranslator {
    fn translate(&self, input: &str, limit: usize) -> Vec<Candidate> {
        self.engine
            .decode(input, "")
            .into_iter()
            .take(limit)
            .map(|sentence| Candidate {
                text: sentence.text,
                comment: String::new(),
                code: sentence.segmented,
                spans: Vec::new(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sentence_table() -> std::path::PathBuf {
        std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../data/schemes/liuli_v1_sentence/liuli_table.txt"
        ))
    }

    /// Build the real engine, or `None` when the model/table are not present
    /// (the model is loaded by path and not committed).
    fn real_engine() -> Option<Arc<SentenceEngine>> {
        let model = crate::ngram::find_model_path()?;
        let table = sentence_table();
        match SentenceEngine::load(&model, &table) {
            Ok(engine) => Some(Arc::new(engine)),
            Err(e) => {
                eprintln!("engine test: skipped ({e})");
                None
            }
        }
    }

    /// A sentence is committed whole and carries no remaining-coding hint, so
    /// the candidate comment stays empty; the retrieval budget is honored.
    #[test]
    fn translator_reports_sentences_with_an_empty_comment() {
        let Some(engine) = real_engine() else { return };
        let translator = SentenceTranslator::new(engine);
        // `jin tian tian qi` is a valid multi-word sentence code in the table.
        let list = translator.translate("jintiantianqi", 5);
        assert!(!list.is_empty(), "a real sentence code must decode");
        assert!(list.iter().all(|c| c.comment.is_empty()), "sentences carry no ~comment");
        assert!(list.iter().all(|c| !c.text.is_empty()), "every candidate has text");
        assert!(list.len() <= 5, "the budget is honored");
        assert!(translator.translate("jintiantianqi", 0).is_empty(), "a zero budget is empty");
    }

    /// An encoding a sentence code cannot be (no letters / a stray character)
    /// decodes to nothing rather than a partial guess.
    #[test]
    fn translator_ignores_non_sentence_input() {
        let Some(engine) = real_engine() else { return };
        let translator = SentenceTranslator::new(engine);
        for code in ["", "123", "好abc", "a b!", "a,b"] {
            assert!(translator.translate(code, 8).is_empty(), "code {code:?} is not a sentence code");
        }
    }
}
