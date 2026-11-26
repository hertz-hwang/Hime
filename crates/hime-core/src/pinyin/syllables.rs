//! Mandarin syllable table, segmentation and syllable-sequence indexing
//! helpers (SPEC §8.2).
//!
//! The table is the set of toneless syllables the pinyin lexicon may contain.
//! It answers three questions only: is this a complete syllable, is it a
//! proper prefix of one, and how many letters of this position form a
//! syllable. The reference implementation keeps the same list and the same
//! `lue → lve` / `nue → nve` normalization for ü.

use std::collections::{HashMap, HashSet};
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

/// Syllable initials that may stand alone as a 简拼 position (`kf` → `k` `f`,
/// `zhg` → `zh` `g`). `y` / `w` count as initials by pinyin spelling even
/// though they are glides.
pub const INITIALS: &[&str] = &[
    "b", "p", "m", "f", "d", "t", "n", "l", "g", "k", "h", "j", "q", "x", "zh", "ch", "sh", "r",
    "z", "c", "s", "y", "w",
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

/// Number of bits one syllable id occupies in a packed key. 411 syllables fit
/// in 9 bits, so a key holds up to 14 positions; [`MAX_WORD_SYLLABLES`] (8)
/// is what the index stores, well inside that.
const ID_BITS: u32 = 9;

/// Syllable id (1-based; 0 is the wildcard of a 简拼 position). `None` for a
/// string that is not a complete syllable.
#[must_use]
pub fn syllable_id(text: &str) -> Option<u32> {
    static IDS: OnceLock<HashMap<&'static str, u32>> = OnceLock::new();
    let ids = IDS.get_or_init(|| {
        let mut map = HashMap::with_capacity(ALL.len());
        for (index, syllable) in ALL.iter().enumerate() {
            map.entry(*syllable).or_insert(index as u32 + 1);
        }
        map
    });
    ids.get(canonical(text)).copied()
}

/// The syllable a packed id stands for.
#[must_use]
pub fn syllable_of(id: u32) -> Option<&'static str> {
    id.checked_sub(1)
        .and_then(|index| ALL.get(index as usize))
        .copied()
}

/// A word's syllable sequence as one integer, one id per position. Two
/// sequences with the same syllables in the same order pack equal, so the
/// index is a plain integer sort instead of a string sort. A position that is
/// not a syllable (a table row with an unsegmentable code) packs as 0, the
/// wildcard a 简拼 position also uses.
#[must_use]
pub fn pack_ids(syllables: &[&str]) -> u128 {
    let mut key = 0u128;
    for (depth, syllable) in syllables.iter().enumerate().take(MAX_WORD_SYLLABLES) {
        key |= u128::from(syllable_id(syllable).unwrap_or(0)) << shift_for(depth);
    }
    key
}

/// Bit offset of position `depth` inside a packed key (position 0 is the most
/// significant syllable, so an integer comparison is a lexicographic one).
#[must_use]
pub const fn shift_for(depth: usize) -> u32 {
    (128 - ID_BITS * (depth as u32 + 1)) as u32
}

/// The id at position `depth` of a packed key.
#[must_use]
pub fn id_at(key: u128, depth: usize) -> u32 {
    if depth >= MAX_WORD_SYLLABLES {
        return 0;
    }
    ((key >> shift_for(depth)) & ((1 << ID_BITS) - 1)) as u32
}

/// Build a packed key from the ids at its positions, in order.
#[must_use]
pub fn pack_positions(ids: &[u32]) -> u128 {
    let mut key = 0u128;
    for (depth, &id) in ids.iter().enumerate().take(MAX_WORD_SYLLABLES) {
        key |= u128::from(id) << shift_for(depth);
    }
    key
}

/// How many letters of `bytes[from..]` could be a 简拼 initial (`zh` before
/// `z`), longest first.
#[must_use]
pub fn initial_lengths(bytes: &[u8], from: usize) -> Vec<usize> {
    let mut lengths: Vec<usize> = INITIALS
        .iter()
        .filter(|initial| {
            let length = initial.len();
            from + length <= bytes.len()
                && &bytes[from..from + length] == initial.as_bytes()
        })
        .map(|initial| initial.len())
        .collect();
    lengths.sort_unstable_by(|a, b| b.cmp(a));
    lengths
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
/// the whole input; positions are complete syllables, 简拼 initials (`kf` →
/// `k` `f`, `zhg` → `zh` `g`) or an unfinished syllable prefix (`zho`), and
/// only the last position may be an unfinished prefix. Forms are ordered by
/// longest syllables first, at most `max` of them.
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
    // Dynamic programming over positions keeps the work bounded: 简拼 makes
    // the number of full segmentations grow exponentially, so each position
    // keeps only its best `max` prefixes. Insertion order is what breaks
    // ties, which keeps the candidate order reproducible.
    let mut best: Vec<Vec<Vec<Pattern>>> = vec![Vec::new(); bytes.len() + 1];
    best[0] = vec![Vec::new()];
    for position in 0..bytes.len() {
        if best[position].is_empty() {
            continue;
        }
        prune_forms(&mut best[position], max);
        let bases = std::mem::take(&mut best[position]);
        for token in query_tokens(bytes, position) {
            let length = token.text.len();
            let next = position + length;
            for base in &bases {
                let mut form = Vec::with_capacity(base.len() + 1);
                form.extend(base.iter().cloned());
                form.push(token.clone());
                best[next].push(form);
            }
        }
        best[position] = bases;
    }
    let mut results = std::mem::take(&mut best[bytes.len()]);
    prune_forms(&mut results, max);
    results
}

/// Keep the `max` best of `forms`: fewest positions, then fewest incomplete
/// ones, then longer leading syllables; ties keep insertion order.
fn prune_forms(forms: &mut Vec<Vec<Pattern>>, max: usize) {
    if forms.len() <= max {
        return;
    }
    let mut decorated: Vec<(usize, Vec<Pattern>)> =
        std::mem::take(forms).into_iter().enumerate().collect();
    decorated.sort_by(|(left_index, left), (right_index, right)| {
        left.len()
            .cmp(&right.len())
            .then_with(|| {
                incomplete_count(left).cmp(&incomplete_count(right))
            })
            .then_with(|| {
                let left_lengths = left.iter().map(|p| p.text.len());
                let right_lengths = right.iter().map(|p| p.text.len());
                // Longer leading syllables first (greedy reading).
                right_lengths.cmp(left_lengths)
            })
            .then_with(|| left_index.cmp(right_index))
    });
    decorated.truncate(max);
    forms.extend(decorated.into_iter().map(|(_, form)| form));
}

fn incomplete_count(form: &[Pattern]) -> usize {
    form.iter().filter(|pattern| !pattern.complete).count()
}

/// The tokens a query position may stand for: complete syllables (longest
/// first), a 简拼 initial, and — for the final position only — an unfinished
/// syllable prefix.
fn query_tokens(bytes: &[u8], position: usize) -> Vec<Pattern> {
    let mut tokens: Vec<Pattern> = Vec::new();
    for length in syllable_lengths(bytes, position).iter().rev() {
        let text =
            std::str::from_utf8(&bytes[position..position + length]).expect("pinyin queries are ASCII");
        tokens.push(Pattern {
            text: text.to_owned(),
            canonical: canonical(text).to_owned(),
            complete: true,
        });
    }
    // 简拼: an initial alone stands for any syllable starting with it
    // (`kf` = kai fa, `zhg` = zhong guo). It is incomplete, so the lookup
    // matches by prefix and the display keeps the typed letter.
    for length in initial_lengths(bytes, position) {
        if is_syllable(bytes, position, length) {
            continue;
        }
        let text =
            std::str::from_utf8(&bytes[position..position + length]).expect("pinyin queries are ASCII");
        tokens.push(Pattern {
            text: text.to_owned(),
            canonical: canonical(text).to_owned(),
            complete: false,
        });
    }
    // The tail may be a still-unfinished syllable (`zho`, `kaif`'s `f`).
    // A complete syllable can also be a proper prefix of a longer one (`fa`
    // of `fan`), so require that it is not complete itself.
    let rest = bytes.len() - position;
    if position + rest == bytes.len()
        && !is_syllable(bytes, position, rest)
        && is_prefix(bytes, position, rest)
    {
        let text = std::str::from_utf8(&bytes[position..]).expect("pinyin queries are ASCII");
        tokens.push(Pattern {
            text: text.to_owned(),
            canonical: canonical(text).to_owned(),
            complete: false,
        });
    }
    tokens
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
        assert!(
            forms[0].iter().all(|p| p.complete),
            "the all-complete reading ranks first: {forms:?}"
        );
        assert!(forms.iter().any(|form| form.len() == 2
            && form[0].text == "kai"
            && form[1].text == "fa"));
        // Readings that spell a position with fewer letters exist only as
        // 简拼, and every form still covers the whole input.
        for form in &forms {
            let covered: String = form.iter().map(|p| p.text.as_str()).collect();
            assert_eq!(covered, "kaifa");
        }

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

    #[test]
    fn initials_segment_as_abbreviations_in_any_position() {
        let forms = segmentations("kf", 8);
        assert!(
            forms.iter().any(|form| {
                form.len() == 2 && form[0].text == "k" && form[1].text == "f"
            }),
            "kf must read as the two initials: {forms:?}"
        );
        // A complete-syllable position is never repeated as an initial.
        let forms = segmentations("zhg", 8);
        assert!(forms.iter().any(|form| {
            form.len() == 2 && form[0].text == "zh" && form[1].text == "g"
        }));
        // Mixed input: an initial followed by a full syllable.
        let forms = segmentations("nhao", 8);
        assert!(forms.iter().any(|form| {
            form.len() == 2 && form[0].text == "n" && form[1].text == "hao"
        }));
        assert!(
            segmentations("kaifa", 8)
                .iter()
                .any(|form| form.len() == 2 && form[0].text == "kai"),
            "a full-pinyin reading keeps its complete syllables"
        );
    }

    #[test]
    fn packed_keys_round_trip_and_order_lexicographically() {
        let ni_hao = pack_ids(&["ni", "hao"]);
        let ni = pack_ids(&["ni"]);
        assert_eq!(id_at(ni_hao, 0), syllable_id("ni").unwrap());
        assert_eq!(id_at(ni_hao, 1), syllable_id("hao").unwrap());
        assert_eq!(id_at(ni_hao, 2), 0);
        assert_eq!(pack_positions(&[syllable_id("ni").unwrap(), syllable_id("hao").unwrap()]), ni_hao);
        // Packed order is syllable order, which is what the index's binary
        // search relies on.
        let ni_hao_ma = pack_ids(&["ni", "hao", "ma"]);
        assert!(ni < ni_hao && ni_hao < ni_hao_ma);
        assert_eq!(syllable_of(syllable_id("hao").unwrap()), Some("hao"));
        assert_eq!(syllable_id("lue"), syllable_id("lve"), "ü spellings share one id");
        assert_eq!(syllable_id("zzz"), None);
    }

    /// A long 简拼 string stays bounded: the DP keeps only `max` prefixes at
    /// each position instead of enumerating every segmentation.
    #[test]
    fn long_abbreviated_input_segments_with_a_bounded_work_set() {
        let forms = segmentations("zgrmjcy", 8);
        assert!(!forms.is_empty());
        assert!(forms.iter().all(|form| form.len() <= 8));
        assert!(
            forms.iter().any(|form| form.iter().all(|p| !p.complete)),
            "every position may be an initial"
        );
    }
}
