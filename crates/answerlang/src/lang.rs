//! Language table, script classification and prompt-based detection.
//!
//! Only scripts can be measured. The latin-script languages (english, french,
//! german, spanish, portuguese, italian) therefore all share one target script
//! and cannot be told apart from each other — a documented limitation.

/// A language answerlang knows the target script of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Japanese,
    Korean,
    Chinese,
    Russian,
    Ukrainian,
    Greek,
    Arabic,
    Hebrew,
    Thai,
    Hindi,
    English,
    French,
    German,
    Spanish,
    Portuguese,
    Italian,
}

/// (language, canonical name, ISO 639-1 tag).
const TABLE: [(Lang, &str, &str); 16] = [
    (Lang::Japanese, "japanese", "ja"),
    (Lang::Korean, "korean", "ko"),
    (Lang::Chinese, "chinese", "zh"),
    (Lang::Russian, "russian", "ru"),
    (Lang::Ukrainian, "ukrainian", "uk"),
    (Lang::Greek, "greek", "el"),
    (Lang::Arabic, "arabic", "ar"),
    (Lang::Hebrew, "hebrew", "he"),
    (Lang::Thai, "thai", "th"),
    (Lang::Hindi, "hindi", "hi"),
    (Lang::English, "english", "en"),
    (Lang::French, "french", "fr"),
    (Lang::German, "german", "de"),
    (Lang::Spanish, "spanish", "es"),
    (Lang::Portuguese, "portuguese", "pt"),
    (Lang::Italian, "italian", "it"),
];

impl Lang {
    /// Canonical lowercase name (`japanese`).
    pub fn name(self) -> &'static str {
        match self {
            Lang::Japanese => "japanese",
            Lang::Korean => "korean",
            Lang::Chinese => "chinese",
            Lang::Russian => "russian",
            Lang::Ukrainian => "ukrainian",
            Lang::Greek => "greek",
            Lang::Arabic => "arabic",
            Lang::Hebrew => "hebrew",
            Lang::Thai => "thai",
            Lang::Hindi => "hindi",
            Lang::English => "english",
            Lang::French => "french",
            Lang::German => "german",
            Lang::Spanish => "spanish",
            Lang::Portuguese => "portuguese",
            Lang::Italian => "italian",
        }
    }

    /// Exact canonical name, or a tag mapped by its primary subtag
    /// (`ja-JP`, `ja_JP.UTF-8`, `en_US` …). `None` when neither matches.
    pub fn from_name_or_tag(s: &str) -> Option<Lang> {
        let s = s.trim().to_ascii_lowercase();
        if let Some((l, _, _)) = TABLE.iter().find(|(_, n, _)| *n == s) {
            return Some(*l);
        }
        let primary = s.split(['-', '_', '.', '@']).next().unwrap_or("");
        TABLE
            .iter()
            .find(|(_, n, t)| *t == primary || *n == primary)
            .map(|(l, _, _)| *l)
    }

    /// Whether a letter of `script` counts as written in this language.
    pub fn accepts(self, script: Script) -> bool {
        use Script as S;
        match self {
            Lang::Japanese => matches!(script, S::Hiragana | S::Katakana | S::Han),
            Lang::Chinese => script == S::Han,
            Lang::Korean => matches!(script, S::Hangul | S::Han),
            Lang::Russian | Lang::Ukrainian => script == S::Cyrillic,
            Lang::Greek => script == S::Greek,
            Lang::Arabic => script == S::Arabic,
            Lang::Hebrew => script == S::Hebrew,
            Lang::Thai => script == S::Thai,
            Lang::Hindi => script == S::Devanagari,
            Lang::English
            | Lang::French
            | Lang::German
            | Lang::Spanish
            | Lang::Portuguese
            | Lang::Italian => script == S::Latin,
        }
    }
}

/// Writing system of one letter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    Hiragana,
    Katakana,
    Han,
    Hangul,
    Cyrillic,
    Greek,
    Arabic,
    Hebrew,
    Thai,
    Devanagari,
    Latin,
    Other,
}

/// Classify a character by Unicode block. Callers only pass letters
/// (`char::is_alphabetic`).
pub fn script_of(c: char) -> Script {
    let u = c as u32;
    match u {
        0x3040..=0x309F => Script::Hiragana,
        0x30A0..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F => Script::Katakana,
        0x3005..=0x3007
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0x20000..=0x3FFFF => Script::Han,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xA960..=0xA97F | 0xAC00..=0xD7AF => Script::Hangul,
        0x0400..=0x052F | 0x1C80..=0x1C8F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => Script::Cyrillic,
        0x0370..=0x03FF | 0x1F00..=0x1FFF => Script::Greek,
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => {
            Script::Arabic
        }
        0x0590..=0x05FF | 0xFB1D..=0xFB4F => Script::Hebrew,
        0x0E00..=0x0E7F => Script::Thai,
        0x0900..=0x097F | 0xA8E0..=0xA8FF => Script::Devanagari,
        0x0041..=0x005A
        | 0x0061..=0x007A
        | 0x00AA
        | 0x00BA
        | 0x00C0..=0x024F
        | 0x1E00..=0x1EFF
        | 0xFF21..=0xFF3A
        | 0xFF41..=0xFF5A => Script::Latin,
        _ => Script::Other,
    }
}

/// Step 1 of auto detection: the script of the user's prompt.
///
/// Any kana → japanese; else any hangul → korean; else the non-latin script
/// with the most letters. Only latin letters (or none) → `None` (falls
/// through to the next step).
pub fn detect_prompt(text: &str) -> Option<Lang> {
    let mut counts = [0usize; 12];
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        counts[script_of(c) as usize] += 1;
    }
    let n = |s: Script| counts[s as usize];
    if n(Script::Hiragana) + n(Script::Katakana) > 0 {
        return Some(Lang::Japanese);
    }
    if n(Script::Hangul) > 0 {
        return Some(Lang::Korean);
    }
    let candidates = [
        (Script::Han, Lang::Chinese),
        (Script::Cyrillic, Lang::Russian),
        (Script::Greek, Lang::Greek),
        (Script::Arabic, Lang::Arabic),
        (Script::Hebrew, Lang::Hebrew),
        (Script::Thai, Lang::Thai),
        (Script::Devanagari, Lang::Hindi),
    ];
    let mut best: Option<(usize, Lang)> = None;
    for (s, l) in candidates {
        let c = n(s);
        if c > 0 && best.is_none_or(|(b, _)| c > b) {
            best = Some((c, l));
        }
    }
    best.map(|(_, l)| l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_map_by_primary_subtag() {
        assert_eq!(Lang::from_name_or_tag("ja-JP"), Some(Lang::Japanese));
        assert_eq!(Lang::from_name_or_tag("en_US.UTF-8"), Some(Lang::English));
        assert_eq!(Lang::from_name_or_tag("zh-Hans"), Some(Lang::Chinese));
        assert_eq!(Lang::from_name_or_tag("C.UTF-8"), None);
        assert_eq!(Lang::from_name_or_tag("POSIX"), None);
        assert_eq!(Lang::from_name_or_tag("klingon"), None);
    }

    #[test]
    fn prompt_detection_order() {
        assert_eq!(detect_prompt("修正してください"), Some(Lang::Japanese));
        assert_eq!(detect_prompt("수정 修正"), Some(Lang::Korean));
        assert_eq!(detect_prompt("only latin here"), None);
        assert_eq!(detect_prompt("123 !!"), None);
        assert_eq!(detect_prompt("привет 你"), Some(Lang::Russian));
    }
}
