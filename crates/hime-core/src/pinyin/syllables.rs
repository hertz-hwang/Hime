//! Mandarin syllable table, segmentation and syllable-sequence indexing
//! helpers (SPEC §8.2).
//!
//! The table is the set of toneless syllables the pinyin lexicon may contain.
//! It answers three questions only: is this a complete syllable, is it a
//! proper prefix of one, and how many letters of this position form a
//! syllable. The reference implementation keeps the same list and the same
//! `lue → lve` / `nue → nve` normalization for ü.

use std::collections::HashSet;
use std::sync::OnceLock;

/// Longest syllable (`zhuang` / `chuang` / `shuang`).
pub const MAX_SYLLABLE_LEN: usize = 6;

/// The longest code a pinyin sub-table row may carry (`max_code` of the
/// shipped 全拼 scheme; 32 is also the builder's limit).
pub const MAX_CODE_LEN: usize = 32;

/// Longest word, in syllables, the lexicon indexes. Longer dictionary rows
/// are skipped; they are extremely rare and would only add query work.
pub const MAX_WORD_SYLLABLES: usize = 8;

/// Complete segmentations kept per code when building the index.
pub const MAX_SYLLABIFICATIONS: usize = 4;

/// Segmentations kept per query.
pub const MAX_QUERY_SEGMENTATIONS: usize = 8;

/// Toneless Mandarin syllables (ü written `v` or `ue`), without the
/// standalone interjections `m` / `ng` / `hm`.
pub const ALL: &[&str] = &[
    // 零声母
    "a", "ai", "an", "ang", "ao", "e", "ei", "en", "eng", "er", "o", "ou", "yi", "ya", "yao",
    "ye", "you", "yan", "yin", "yang", "ying", "yong", "yu", "yue", "yuan", "yun", "yo", "wu",
    "wa", "wo", "wai", "wei", "wan", "wen", "wang", "weng",
    // b p m f
    "ba", "bo", "bai", "bei", "bao", "ban", "ben", "bang", "beng", "bi", "bie", "biao", "bian",
    "bin", "bing", "bu", "pa", "po", "pai", "pei", "pao", "pou", "pan", "pen", "pang", "peng",
    "pi", "pie", "piao", "pian", "pin", "ping", "pu", "ma", "mo", "me", "mai", "mei", "mao",
    "mou", "man", "men", "mang", "meng", "mi", "mie", "miao", "miu", "mian", "min", "ming",
    "mu", "fa", "fo", "fei", "fou", "fan", "fen", "fang", "feng", "fu",
    // d t n l
    "da", "de", "dai", "dei", "dao", "dou", "dan", "den", "dang", "deng", "dong", "di", "dia",
    "die", "diao", "diu", "dian", "ding", "du", "duo", "dui", "duan", "dun", "ta", "te", "tai",
    "tao", "tou", "tan", "tang", "teng", "tong", "ti", "tie", "tiao", "tian", "ting", "tu",
    "tuo", "tui", "tuan", "tun", "na", "ne", "nai", "nei", "nao", "nou", "nan", "nen", "nang",
    "neng", "nong", "ni", "nie", "niao", "niu", "nian", "nin", "niang", "ning", "nu", "nuo",
    "nuan", "nun", "nv", "nve", "nue", "la", "le", "lai", "lei", "lao", "lou", "lan", "lang",
    "leng", "long", "li", "lia", "lie", "liao", "liu", "lian", "lin", "liang", "ling", "lu",
    "luo", "luan", "lun", "lv", "lve", "lue", "lo",
    // g k h
    "ga", "ge", "gai", "gei", "gao", "gou", "gan", "gen", "gang", "geng", "gong", "gu", "gua",
    "guo", "guai", "gui", "guan", "gun", "guang", "ka", "ke", "kai", "kei", "kao", "kou",
    "kan", "ken", "kang", "keng", "kong", "ku", "kua", "kuo", "kuai", "kui", "kuan", "kun",
    "kuang", "ha", "he", "hai", "hei", "hao", "hou", "han", "hen", "hang", "heng", "hong",
    "hu", "hua", "huo", "huai", "hui", "huan", "hun", "huang",
    // j q x
    "ji", "jia", "jie", "jiao", "jiu", "jian", "jin", "jiang", "jing", "jiong", "ju", "jue",
    "juan", "jun", "qi", "qia", "qie", "qiao", "qiu", "qian", "qin", "qiang", "qing", "qiong",
    "qu", "que", "quan", "qun", "xi", "xia", "xie", "xiao", "xiu", "xian", "xin", "xiang",
    "xing", "xiong", "xu", "xue", "xuan", "xun",
    // zh ch sh r
    "zha", "zhe", "zhi", "zhai", "zhei", "zhao", "zhou", "zhan", "zhen", "zhang", "zheng",
    "zhong", "zhu", "zhua", "zhuo", "zhuai", "zhui", "zhuan", "zhun", "zhuang", "cha", "che",
    "chi", "chai", "chao", "chou", "chan", "chen", "chang", "cheng", "chong", "chu", "chua",
    "chuo", "chuai", "chui", "chuan", "chun", "chuang", "sha", "she", "shi", "shai", "shei",
    "shao", "shou", "shan", "shen", "shang", "sheng", "shu", "shua", "shuo", "shuai", "shui",
    "shuan", "shun", "shuang", "re", "ri", "rao", "rou", "ran", "ren", "rang", "reng", "rong",
    "ru", "rua", "ruo", "rui", "ruan", "run",
    // z c s
    "za", "ze", "zi", "zai", "zei", "zao", "zou", "zan", "zen", "zang", "zeng", "zong", "zu",
    "zuo", "zui", "zuan", "zun", "ca", "ce", "ci", "cai", "cao", "cou", "can", "cen", "cang",
    "ceng", "cong", "cu", "cuo", "cui", "cuan", "cun", "sa", "se", "si", "sai", "sao", "sou",
    "san", "sen", "sang", "seng", "song", "su", "suo", "sui", "suan", "sun",
];

/// Pack up to six ASCII bytes into one integer, zero-padded on the right.
/// `0` is not a letter byte, so `a` and `aa` never collide.
fn pack(bytes: &[u8], from: usize, length: usize) -> u64 {
    let end = (from + length).min(bytes.len());
    let mut value = 0u64;
    for &byte in &bytes[from..end] {
        value = (value << 8) | u64::from(byte);
    }
    let actual = end - from;
    value << (8 * (MAX_SYLLABLE_LEN - actual))
}

fn packed() -> &'static HashSet<u64> {
    static SET: OnceLock<HashSet<u64>> = OnceLock::new();
    SET.get_or_init(|| ALL.iter().map(|text| pack(text.as_bytes(), 0, text.len())).collect())
}

fn prefixes() -> &'static HashSet<u64> {
    static SET: OnceLock<HashSet<u64>> = OnceLock::new();
    SET.get_or_init(|| {
        let mut set = HashSet::new();
        for syllable in ALL {
            let bytes = syllable.as_bytes();
            for length in 1..bytes.len() {
                set.insert(pack(bytes, 0, length));
            }
        }
        set
    })
}

/// Whether `text` is a complete syllable.
#[must_use]
pub fn is_syllable_text(text: &str) -> bool {
    text.len() <= MAX_SYLLABLE_LEN
        && text.bytes().all(|b| b.is_ascii_lowercase())
        && packed().contains(&pack(text.as_bytes(), 0, text.len()))
}

/// Whether `bytes[from..from+length]` is a complete syllable.
#[must_use]
pub fn is_syllable(bytes: &[u8], from: usize, length: usize) -> bool {
    length > 0
        && length <= MAX_SYLLABLE_LEN
        && from + length <= bytes.len()
        && packed().contains(&pack(bytes, from, length))
}

/// Whether `bytes[from..from+length]` is a proper prefix of some syllable
/// (not a complete syllable itself).
#[must_use]
pub fn is_prefix(bytes: &[u8], from: usize, length: usize) -> bool {
    length > 0
        && length <= MAX_SYLLABLE_LEN
        && from + length <= bytes.len()
        && prefixes().contains(&pack(bytes, from, length))
}

/// Complete syllable lengths starting at `from`, ascending.
#[must_use]
pub fn syllable_lengths(bytes: &[u8], from: usize) -> Vec<usize> {
    let mut lengths = Vec::new();
    for length in 1..=MAX_SYLLABLE_LEN {
        if from + length > bytes.len() {
            break;
        }
        if is_syllable(bytes, from, length) {
            lengths.push(length);
        }
    }
    lengths
}

/// `lue` / `nue` are alternative spellings of `lve` / `nve`; normalize them
/// so a table written with either form answers both queries.
#[must_use]
pub fn canonical(syllable: &str) -> &str {
    match syllable {
        "lue" => "lve",
        "nue" => "nve",
        other => other,
    }
}

/// Every complete syllable segmentation of `code`, longest syllables tried
/// first, at most `limit` forms. Used while building the lexicon index: a
/// table row `西安\txian` must be reachable both as `xian` and as `xi an`.
#[must_use]
pub fn syllabifications(code: &str, limit: usize) -> Vec<Vec<String>> {
    let bytes = code.as_bytes();
    if limit == 0
        || bytes.is_empty()
        || bytes.len() > MAX_CODE_LEN
        || !bytes.iter().all(|b| b.is_ascii_lowercase())
    {
        return Vec::new();
    }
    let lengths: Vec<Vec<usize>> =
        (0..bytes.len()).map(|start| syllable_lengths(bytes, start)).collect();
    let mut results: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    walk_syllabifications(bytes, 0, &lengths, limit, &mut current, &mut results);
    results
}

fn walk_syllabifications(
    bytes: &[u8],
    position: usize,
    lengths: &[Vec<usize>],
    limit: usize,
    current: &mut Vec<String>,
    results: &mut Vec<Vec<String>>,
) {
    if results.len() >= limit {
        return;
    }
    if position == bytes.len() {
        if current.len() <= MAX_WORD_SYLLABLES {
            results.push(current.clone());
        }
        return;
    }
    // Long syllables first: the greedy reading (`xian`) precedes the split
    // reading (`xi an`), matching the reference's ranking.
    for &length in lengths[position].iter().rev() {
        let text = std::str::from_utf8(&bytes[position..position + length])
            .expect("pinyin codes are ASCII");
        current.push(canonical(text).to_owned());
        walk_syllabifications(bytes, position + length, lengths, limit, current, results);
        current.pop();
        if results.len() >= limit {
            return;
        }
    }
}

/// The fewest syllables a code can be read as (longest syllables first), or
/// `None` when the code cannot be segmented at all. A small DP: `best[i]` is
/// the fewest syllables covering the first `i` letters.
#[must_use]
pub fn min_syllables(code: &str) -> Option<usize> {
    let bytes = code.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_CODE_LEN
        || !bytes.iter().all(|b| b.is_ascii_lowercase())
    {
        return None;
    }
    let mut best = vec![usize::MAX; bytes.len() + 1];
    best[0] = 0;
    for start in 0..bytes.len() {
        if best[start] == usize::MAX {
            continue;
        }
        for length in syllable_lengths(bytes, start) {
            let next = start + length;
            best[next] = best[next].min(best[start] + 1);
        }
    }
    let total = best[bytes.len()];
    (total != usize::MAX).then_some(total)
}

/// One syllable position of a query: the letters as typed plus whether they
/// form a complete syllable or a still-unfinished prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// Letters as typed (used for display and coverage counts).
    pub text: String,
    /// Normalized matching form (`lue` → `lve`).
    pub canonical: String,
    /// Complete syllable; `false` is the unfinished tail of the query.
    pub complete: bool,
}

/// Segment a typed query into syllable positions. Every returned form covers
/// the whole input; only the last position may be an unfinished prefix. Forms
/// are ordered by longest syllables first, at most `max` of them.
#[must_use]
pub fn segmentations(input: &str, max: usize) -> Vec<Vec<Pattern>> {
    let bytes = input.as_bytes();
    if max == 0
        || bytes.is_empty()
        || bytes.len() > MAX_CODE_LEN
        || !bytes.iter().all(|b| b.is_ascii_lowercase())
    {
        return Vec::new();
    }
    let mut results: Vec<Vec<Pattern>> = Vec::new();
    let mut current: Vec<Pattern> = Vec::new();
    walk_query(bytes, 0, max, &mut current, &mut results);
    results
}

fn walk_query(
    bytes: &[u8],
    position: usize,
    max: usize,
    current: &mut Vec<Pattern>,
    results: &mut Vec<Vec<Pattern>>,
) {
    if results.len() >= max {
        return;
    }
    if position == bytes.len() {
        results.push(current.clone());
        return;
    }
    for &length in syllable_lengths(bytes, position).iter().rev() {
        let text = std::str::from_utf8(&bytes[position..position + length])
            .expect("pinyin queries are ASCII");
        current.push(Pattern {
            text: text.to_owned(),
            canonical: canonical(text).to_owned(),
            complete: true,
        });
        walk_query(bytes, position + length, max, current, results);
        current.pop();
        if results.len() >= max {
            return;
        }
    }
    // The tail may be a still-unfinished syllable (`zho`, `kaif`'s `f`).
    // A complete syllable can also be a proper prefix of a longer one (`fa`
    // of `fan`), so require that it is not complete itself.
    let rest = bytes.len() - position;
    if !is_syllable(bytes, position, rest) && is_prefix(bytes, position, rest) {
        let text =
            std::str::from_utf8(&bytes[position..]).expect("pinyin queries are ASCII");
        current.push(Pattern {
            text: text.to_owned(),
            canonical: canonical(text).to_owned(),
            complete: false,
        });
        results.push(current.clone());
        current.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(forms: &[Vec<String>]) -> Vec<String> {
        forms.iter().map(|form| form.join(" ")).collect()
    }

    #[test]
    fn syllable_table_answers_the_three_questions() {
        for syllable in ["a", "zhuang", "shuang", "xian", "lve", "nve", "er"] {
            assert!(is_syllable_text(syllable), "{syllable}");
        }
        for not in ["", "zh", "zz", "zho", "xiana", "n", "luee"] {
            assert!(!is_syllable_text(not), "{not}");
        }
        assert!(is_prefix(b"zho", 0, 3));
        assert!(is_prefix(b"zh", 0, 2));
        assert!(!is_prefix(b"zhong", 0, 5), "a complete syllable is not a prefix");
    }

    #[test]
    fn canonicalizes_the_two_ue_spellings() {
        assert_eq!(canonical("lue"), "lve");
        assert_eq!(canonical("nue"), "nve");
        assert_eq!(canonical("xue"), "xue");
    }

    #[test]
    fn syllabifications_try_long_syllables_first() {
        assert_eq!(texts(&syllabifications("xian", 4)), vec!["xian", "xi an"]);
        assert_eq!(texts(&syllabifications("lve", 4)), vec!["lve", "lv e"]);
        assert_eq!(texts(&syllabifications("lue", 4)), vec!["lve", "lu e"]);
        assert!(syllabifications("xyz", 4).is_empty());
        assert!(syllabifications("", 4).is_empty());
        assert_eq!(syllabifications("xian", 0).len(), 0);
    }

    #[test]
    fn query_segmentations_cover_the_whole_input() {
        let forms = segmentations("kaifa", 8);
        assert!(forms.iter().all(|form| form.iter().all(|p| p.complete)));
        assert!(forms.iter().any(|form| form.len() == 2
            && form[0].text == "kai"
            && form[1].text == "fa"));

        let forms = segmentations("kaif", 8);
        let partial: Vec<_> = forms
            .iter()
            .filter(|form| form.last().is_some_and(|p| !p.complete))
            .collect();
        assert!(!partial.is_empty(), "kaif must allow the unfinished f");
        assert!(partial.iter().any(|form| form.last().unwrap().text == "f"));

        assert!(
            segmentations("zh", 8).iter().any(|form| {
                form.len() == 1 && form[0].text == "zh" && !form[0].complete
            }),
            "zh is an unfinished syllable (zha / zhe / …)"
        );
        assert!(segmentations("zho", 8).iter().any(|form| {
            form.len() == 1 && form[0].text == "zho" && !form[0].complete
        }));
    }
}
