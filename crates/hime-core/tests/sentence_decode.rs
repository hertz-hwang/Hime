//! End-to-end 形码整句 regression: encode gold sentences the way a user types
//! them (rank-1 简码 word codes, longest-match) and require the decoder to put
//! the gold sentence first.
//!
//! The encoding step mirrors Fire's `SentenceEval` (rank-1 简码, longest-match
//! word coding) — it is *test-only* scaffolding, not part of the engine: a real
//! shell feeds whatever keys the user types. Skipping the encoding would test
//! nothing about real typing.
//!
//! Runs only when the `.hng` model and the liuli sentence table are present
//! (both are large and the model is not committed); CI without them skips clean.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use hime_core::ngram::find_model_path;
use hime_core::sentence::{SentenceDecoder, SentenceLexicon};

const TABLE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/schemes/liuli_v1_sentence/liuli_table.txt"
);

fn code_candidates(text: &str) -> HashMap<String, Vec<String>> {
    let mut by_code: HashMap<String, Vec<String>> = HashMap::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cols = line.split('\t');
        let (Some(cand), Some(code)) = (cols.next(), cols.next()) else {
            continue;
        };
        let (cand, code) = (cand.trim(), code.trim());
        if cand.is_empty() || code.is_empty() || !code.bytes().all(|b| b.is_ascii_alphabetic()) {
            continue;
        }
        let list = by_code.entry(code.to_string()).or_default();
        if !list.iter().any(|c| c == cand) {
            list.push(cand.to_string());
        }
    }
    by_code
}

/// word -> its shortest rank-1 code (a code whose first candidate is the word),
/// min length `min_len`; ties resolved by shorter then lexicographic. Port of
/// `SentenceEval.jianmaCodes`.
fn jianma_codes(by_code: &HashMap<String, Vec<String>>, min_len: usize) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    for (code, cands) in by_code {
        let Some(first) = cands.first() else { continue };
        if code.chars().count() < min_len {
            continue;
        }
        map.entry(first.clone())
            .and_modify(|cur| {
                if code.chars().count() < cur.chars().count()
                    || (code.chars().count() == cur.chars().count() && code < cur)
                {
                    *cur = code.clone();
                }
            })
            .or_insert_with(|| code.clone());
    }
    map
}

/// Longest-match word coding of a han-only run (port of `SentenceEval.encode`).
/// `None` when some character has no rank-1 word code, i.e. unencodable typing.
fn encode(chars: &[char], words: &HashMap<String, String>, max_word: usize) -> Option<String> {
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let mut hit = None;
        let mut len = max_word.min(chars.len() - i);
        while len >= 1 {
            let key: String = chars[i..i + len].iter().collect();
            if let Some(code) = words.get(&key) {
                hit = Some((code.clone(), len));
                break;
            }
            len -= 1;
        }
        let (code, used) = hit?;
        out.push_str(&code);
        i += used.max(1);
    }
    Some(out)
}

/// Gold sentences a 形码 typist can produce with rank-1 简码 (≥2 keys).
const GOLD: &[&str] = &[
    "今天天气很好",
    "我们一起吃饭",
    "这个办法不错",
    "他不知道怎么说",
    "我现在很忙",
    "你过来一下",
    "他们去了北京",
    "这本书很有意思",
    "生命在于运动",
    "时间就是金钱",
    "我不知道该怎么办",
    "大家都很高兴",
    "中国是一个历史悠久的国家",
    "科学技术是第一生产力",
    "我们需要更多的时间",
    "他在房间里看书",
];

#[test]
fn real_model_and_table_decode_gold_sentences_to_first_place() {
    let Some(model_path) = find_model_path() else {
        eprintln!("skip: no .hng model (set HIME_SENTENCE_MODEL or add data/sentence-ngram.hng)");
        return;
    };
    if !Path::new(TABLE).is_file() {
        eprintln!("skip: no liuli sentence table at {TABLE}");
        return;
    }

    let model = hime_core::NgramModel::load(&model_path).expect("load model");
    let lexicon = SentenceLexicon::load(TABLE).expect("load lexicon");
    let table_text = fs::read_to_string(TABLE).unwrap();
    let words = jianma_codes(&code_candidates(&table_text), 2);

    let mut checked = 0;
    for gold in GOLD {
        let chars: Vec<char> = gold.chars().collect();
        let Some(code) = encode(&chars, &words, 8) else {
            continue; // this sentence needs a word with no rank-1 code
        };
        let candidates = SentenceDecoder::decode(&model, &lexicon, &code, "", true);
        assert!(
            !candidates.is_empty(),
            "no candidate for {gold:?} (code {code:?})"
        );
        assert_eq!(
            candidates[0].text, *gold,
            "gold not first for code {code:?}; got {:?}",
            candidates.iter().take(3).map(|c| c.text.as_str()).collect::<Vec<_>>()
        );
        checked += 1;
    }
    // Guard against silently testing nothing (e.g. a table that stopped matching).
    assert!(checked >= 12, "only {checked} sentences were encodable; expected ≥ 12");
}
