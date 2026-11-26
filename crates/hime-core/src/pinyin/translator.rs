//! [`PinyinTranslator`]: the 全拼 scheme's candidate source (SPEC §8.2).
//!
//! Two stages produce the window, in this order:
//!
//! 1. **Whole-sentence candidates.** The decoder ([`super::decoder`]) builds
//!    a word graph over the query's syllable positions and scores paths with
//!    the character n-gram. This is what answers a combination the table has
//!    no row for (`huangzhao` → 黄照) and what makes 全拼 usable for running
//!    text: typing a sentence's letters yields the sentence.
//! 2. **Table words.** [`PinyinLexicon::lookup`] ranks the table rows,
//!    including 简拼 (`kf` → 开发) and words covering a syllable prefix of
//!    the query (`开` while typing `kaifa`), which is how a long input is
//!    committed word by word.
//!
//! Sentence candidates head the window and their texts are removed from the
//! word list, so a page never shows one text twice. Without an n-gram model
//! the translator silently serves the word stage alone — 简拼 and prefix
//! words need no model, so a fresh install still types.
//!
//! A candidate's `code` is its syllable split (`ni hao`), which the session
//! displays as the encoding line; a word match with a remaining-code split
//! keeps the split it matched.

use std::sync::Arc;

use crate::ngram::NgramModel;
use crate::translator::Translator;
use crate::types::Candidate;
use crate::user_ngram::UserNgram;

use super::decoder;
use super::lexicon::PinyinLexicon;
use super::syllables::{self, MAX_QUERY_SEGMENTATIONS};

/// Sentence candidates placed at the head of the window.
const SENTENCE_HEAD: usize = 5;

/// Full-pinyin candidate source: table words, 简拼, and whole sentences.
pub struct PinyinTranslator {
    lexicon: Arc<PinyinLexicon>,
    model: Option<Arc<NgramModel>>,
    /// The learned user model, when the scheme declares `user_ngram = true`.
    /// It blends into the sentence score and into the single-character word
    /// rows, so a one-syllable query is re-ranked by both models (SPEC §7.7).
    user: Option<Arc<UserNgram>>,
}

impl std::fmt::Debug for PinyinTranslator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinyinTranslator")
            .field("lexicon", &self.lexicon)
            .field("model", &self.model.is_some())
            .field("user_ngram", &self.user.is_some())
            .finish()
    }
}

impl PinyinTranslator {
    /// Word-only source. 简拼 and prefix words work; out-of-table sentences
    /// keep to the single characters the table lists.
    pub fn new(lexicon: Arc<PinyinLexicon>) -> Self {
        Self { lexicon, model: None, user: None }
    }

    /// Word source plus whole-sentence decoding through `model`.
    #[must_use]
    pub fn with_model(lexicon: Arc<PinyinLexicon>, model: Arc<NgramModel>) -> Self {
        Self { lexicon, model: Some(model), user: None }
    }

    /// Attach the learned user model (or drop it with `None`).
    #[must_use]
    pub fn with_user_ngram(mut self, user: Option<Arc<UserNgram>>) -> Self {
        self.user = user;
        self
    }

    /// The lexicon this source reads.
    #[must_use]
    pub fn lexicon(&self) -> &Arc<PinyinLexicon> {
        &self.lexicon
    }

    /// Re-rank the single-character rows of the word window with both models.
    /// In 全拼 a one-syllable query is a character choice, so the default
    /// n-gram and the learned user n-gram order those candidates together
    /// (SPEC §7.7); multi-character words keep their positions.
    fn rerank_characters(&self, window: &mut Vec<Candidate>, context: &str) {
        let Some(model) = self.model.as_deref() else { return };
        let (prev2, prev1) = decoder::context_tail(context);
        let mut slots: Vec<usize> = Vec::new();
        let mut scored: Vec<(Candidate, f64)> = Vec::new();
        for (index, candidate) in window.iter().enumerate() {
            let mut chars = candidate.text.chars();
            let (Some(ch), None) = (chars.next(), chars.next()) else { continue };
            let cp = ch as u32;
            let base = model.logp(prev2, prev1, cp);
            let score = match self.user.as_deref() {
                Some(user) => {
                    user.blended_logp(base, prev2, prev1, cp, model.unigram(cp)).unwrap_or(base)
                }
                None => base,
            };
            slots.push(index);
            scored.push((candidate.clone(), score));
        }
        if scored.len() < 2 {
            return;
        }
        scored.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (slot, (candidate, _)) in slots.into_iter().zip(scored) {
            window[slot] = candidate;
        }
    }

    /// Whole-sentence candidates for `input`, best first.
    fn sentences(&self, input: &str, context: &str) -> Vec<Candidate> {
        let Some(model) = self.model.as_deref() else {
            return Vec::new();
        };
        // A query the table spells in full pinyin already has a word answer
        // (`nihao` → 你好); decoding it again would let generator noise
        // (你耗, 倪浩) push the real word down. Sentence decoding is for a
        // query no row covers, which is the whole point: `huangzhao` has no
        // row, and the model's 皇朝 / 黄照 do. An abbreviated query keeps its
        // sentence stage, since `nih` is still being typed.
        if self.lexicon.spells_in_full_pinyin(input) {
            return Vec::new();
        }
        let forms = syllables::segmentations(input, MAX_QUERY_SEGMENTATIONS);
        // A query that is itself one complete syllable is a character choice
        // (`xian` → 先), and the word list already answers it. Its other
        // readings (`xi an`) would let the model's sentence prior push a
        // two-syllable word ahead of those characters.
        let single = forms
            .first()
            .is_some_and(|form| form.len() == 1 && form[0].complete);
        if single {
            return Vec::new();
        }
        // Every syllable reading of the input is decoded, and the paths are
        // merged by score: one reading cannot reach a word that crosses its
        // syllable boundary (`xian` is both `xian` and `xi an`, and 先 / 西安
        // need different ones).
        //
        // A reading with an abbreviated syllable before the last one is left
        // to the word stage: `zho` can read `z ho…`, and so could become any
        // two-syllable word starting with `z`, which is a lookup guess rather
        // than a sentence. The final position may be abbreviated (`nih`).
        let mut merged: Vec<decoder::PinyinCandidate> = Vec::new();
        for form in forms {
            if form.len() < 2 {
                continue;
            }
            if form[..form.len() - 1].iter().any(|pattern| !pattern.complete) {
                continue;
            }
            merged.extend(decoder::decode_with_user(
                model,
                &self.lexicon,
                self.user.as_deref(),
                &form,
                context,
                SENTENCE_HEAD,
            ));
        }
        merged.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.word_count.cmp(&left.word_count))
        });
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        merged.retain(|path| seen.insert(path.text.clone()));
        merged
            .into_iter()
            .take(SENTENCE_HEAD)
            .map(|path| Candidate {
                text: path.text,
                comment: String::new(),
                code: path.segmented,
                spans: Vec::new(),
            })
            .collect()
    }
}

impl Translator for PinyinTranslator {
    fn translate(&self, input: &str, limit: usize) -> Vec<Candidate> {
        self.translate_with_context(input, "", limit)
    }

    fn translate_with_context(
        &self,
        input: &str,
        context: &str,
        limit: usize,
    ) -> Vec<Candidate> {
        if input.is_empty() || limit == 0 {
            return Vec::new();
        }
        let sentences = self.sentences(input, context);
        let taken: std::collections::HashSet<String> =
            sentences.iter().map(|candidate| candidate.text.clone()).collect();
        let budget = limit.saturating_sub(sentences.len());
        let mut window = sentences;
        window.extend(
            self.lexicon
                .lookup(input, budget.max(limit))
                .into_iter()
                .filter(|hit| !taken.contains(&hit.text))
                .map(|hit| Candidate {
                    text: hit.text,
                    comment: String::new(),
                    code: hit.key,
                    spans: Vec::new(),
                })
                .take(budget),
        );
        self.rerank_characters(&mut window, context);
        window
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_ngram::UserNgram;

    fn translator() -> PinyinTranslator {
        PinyinTranslator::new(Arc::new(PinyinLexicon::parse(
            "你好\tnihao\t200\n你\tni\t100\n开发\tkaifa\t100\n",
        )))
    }

    fn texts(list: &[Candidate]) -> Vec<&str> {
        list.iter().map(|c| c.text.as_str()).collect()
    }

    /// In 全拼 the two models re-rank single characters together: the default
    /// model orders them by the left context, and the learned model lifts a
    /// character the user actually types after that context.
    #[test]
    fn both_models_re_rank_single_characters_with_the_left_context() {
        let Some(model) = crate::session::tests::shipped_model() else { return };
        let path = std::env::temp_dir().join(format!(
            "hime-pinyin-single-{}.hng",
            std::process::id()
        ));
        let user = std::sync::Arc::new(UserNgram::empty(&path));
        // The context the query carries is 我, so that is the transition the
        // user has to have typed for the learned model to speak up.
        for _ in 0..400 {
            user.record("我你", "");
        }
        let lexicon = std::sync::Arc::new(PinyinLexicon::parse(
            "泥\tni\t100\n你\tni\t90\n呢\tni\t80\n",
        ));
        let plain = PinyinTranslator::with_model(
            std::sync::Arc::clone(&lexicon),
            std::sync::Arc::clone(&model),
        )
        .translate_with_context("ni", "我", 5);
        assert_ne!(plain[0].text, "泥", "the default model already orders them: {plain:?}");

        let learned = PinyinTranslator::with_model(lexicon, model)
            .with_user_ngram(Some(std::sync::Arc::clone(&user)))
            .translate_with_context("ni", "我", 5);
        assert_eq!(learned[0].text, "你", "the learned character wins: {learned:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reports_words_with_the_syllable_split() {
        let list = translator().translate("nihao", 5);
        assert_eq!(list[0].text, "你好");
        assert_eq!(list[0].code, "ni hao");
        assert!(list[0].comment.is_empty());
    }

    #[test]
    fn honors_the_retrieval_budget() {
        assert_eq!(translator().translate("ni", 1).len(), 1);
        assert!(translator().translate("ni", 0).is_empty());
    }

    #[test]
    fn initials_reach_their_words_without_a_model() {
        let lexicon = Arc::new(PinyinLexicon::parse(
            "开发\tkaifa\t100\n开放\tkaifang\t90\n开\tkai\t80\n",
        ));
        let list = PinyinTranslator::new(lexicon).translate("kf", 5);
        assert_eq!(texts(&list), vec!["开发", "开放", "开"]);
    }

    /// A model turns an out-of-table combination into a sentence candidate at
    /// the head of the window, and the word stage does not repeat its text.
    #[test]
    fn a_model_adds_out_of_table_sentences_at_the_head() {
        let Some(model) = crate::ngram::find_model_path().and_then(|path| {
            NgramModel::load(path).ok()
        }) else {
            return;
        };
        let lexicon = Arc::new(PinyinLexicon::parse(
            "黄\thuang\t5843\n照\tzhao\t100\n你好\tnihao\t5328\n",
        ));
        let translator = PinyinTranslator::with_model(lexicon, Arc::new(model));
        let list = translator.translate("huangzhao", 8);
        assert!(texts(&list).starts_with(&["黄照"]), "{list:?}");
        assert_eq!(list[0].code, "huang zhao");
        let texts = texts(&list);
        let unique: std::collections::HashSet<&&str> = texts.iter().collect();
        assert_eq!(unique.len(), texts.len(), "no text appears twice: {texts:?}");
    }
}
