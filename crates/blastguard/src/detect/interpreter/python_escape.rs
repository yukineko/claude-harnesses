//! The value Python gives a NON-raw string literal body, escape sequences
//! decoded (backlog 47b20647).
//!
//! Python removes a backslash + LF ("BN") inside a non-raw literal — single-,
//! double- or triple-quoted, `b''`, `f''` — so `'/etc/ho\<LF>sts'` IS
//! `/etc/hosts` at runtime. Reading the body without decoding it hid the
//! system path from every check that compares a literal against a path. A raw
//! literal (`r'…'`) is not decoded here: Python keeps its backslashes and LFs,
//! so the caller must not call this for one.
//!
//! Decoded (Python 3 lexical rules): BN (also backslash + CR LF) is removed;
//! `\\` `\'` `\"` `\a` `\b` `\f` `\n` `\r` `\t` `\v`; octal `\o`..`\ooo`;
//! `\xHH`; for `str` only, `\uXXXX` and `\UXXXXXXXX`. Any other backslash pair
//! (including `\u`/`\U`/`\N` in a bytes literal) is kept as the two characters,
//! as Python does. A bytes value byte is mapped to the char of the same number
//! (latin-1), which is the same text for every ASCII path.
//!
//! A `\N{NAME}` named escape (str only) naming a printable ASCII character
//! (all 95, case-insensitive, plus the alias `SP`; [`ascii_by_name`]) is
//! decoded to it.
//!
//! What cannot be decoded statically is NEVER given a guessed value:
//! - any other well-formed `\N{NAME}` is exactly ONE code point blastguard
//!   cannot name (there is no table of non-ASCII names here), so the value is
//!   [`Decoded::Holed`]: the decoded text with [`HOLE`] standing for each
//!   unknown code point. The caller must not read a hole as any particular
//!   character: it can be `/` or `.` too (see
//!   `python_literal_target::unknown_literal_target_verdict`). `lex` keeps
//!   the undecoded text for its `Tok::Str`; `python_semantics::literal_values`
//!   reports the `Holed`/`Undecodable` reading.
//! - a malformed escape (`\x2`, a short `\u`, `\N` without `{NAME}`, a code
//!   point that is not a `char`) is [`Decoded::Undecodable`]. Python rejects
//!   such a literal at compile time, but blastguard does not rely on that: the
//!   caller must not read the literal as a known value.

/// Stands for one code point of a `\N{…}` escape that is not decoded. A
/// Unicode noncharacter, so it never occurs in a decoded body by accident (a
/// body that already holds it is [`Decoded::Undecodable`]).
pub(super) const HOLE: char = '\u{FDD0}';

/// The value of a non-raw literal body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decoded {
    /// Fully decoded.
    Value(String),
    /// Decoded except for `\N{…}` escapes, each one [`HOLE`] in the text.
    Holed(String),
    /// A malformed escape: no value can be read.
    Undecodable,
}

/// Decode `body` (the text between the quotes, prefix and quotes excluded) of
/// a non-raw literal. `bytes` is true for a `b''`/`B''` literal.
pub(super) fn decode_non_raw(body: &str, bytes: bool) -> Decoded {
    if body.contains(HOLE) {
        return Decoded::Undecodable;
    }
    let mut holed = false;
    match decode(body, bytes, &mut holed) {
        None => Decoded::Undecodable,
        Some(text) if holed => Decoded::Holed(text),
        Some(text) => Decoded::Value(text),
    }
}

fn decode(body: &str, bytes: bool, holed: &mut bool) -> Option<String> {
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' {
            out.push(c);
            i += 1;
            continue;
        }
        let Some(&e) = chars.get(i + 1) else {
            // A trailing lone backslash: not a complete escape; keep it.
            out.push('\\');
            i += 1;
            continue;
        };
        i += 2;
        match e {
            '\n' => {}
            '\r' => {
                if chars.get(i) == Some(&'\n') {
                    i += 1;
                }
            }
            '\\' | '\'' | '"' => out.push(e),
            'a' => out.push('\u{07}'),
            'b' => out.push('\u{08}'),
            'f' => out.push('\u{0c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\u{0b}'),
            '0'..='7' => {
                let mut n = e.to_digit(8)?;
                let mut taken = 1;
                while taken < 3 {
                    match chars.get(i).and_then(|d| d.to_digit(8)) {
                        Some(d) => {
                            n = n * 8 + d;
                            i += 1;
                            taken += 1;
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(if bytes { n & 0xff } else { n })?);
            }
            'x' => {
                let n = hex(&chars, i, 2)?;
                i += 2;
                out.push(char::from_u32(n)?);
            }
            'u' | 'U' if !bytes => {
                let width = if e == 'u' { 4 } else { 8 };
                let n = hex(&chars, i, width)?;
                i += width;
                out.push(char::from_u32(n)?);
            }
            'N' if !bytes => {
                // `\N{NAME}`: a non-empty name up to the first `}`. Anything
                // else after `\N` is malformed.
                if chars.get(i) != Some(&'{') {
                    return None;
                }
                let close = chars[i + 1..].iter().position(|&d| d == '}')?;
                if close == 0 {
                    return None;
                }
                let name: String = chars[i + 1..i + 1 + close].iter().collect();
                i += 1 + close + 1;
                match ascii_by_name(&name) {
                    Some(c) => out.push(c),
                    None => {
                        out.push(HOLE);
                        *holed = true;
                    }
                }
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Some(out)
}

/// Unicode names of the 33 printable ASCII characters that are not a letter or
/// a digit (letters and digits are computed in [`ascii_by_name`]), as
/// `unicodedata.name` reports them.
const ASCII_PUNCTUATION_NAMES: [(&str, char); 33] = [
    ("SPACE", ' '),
    ("EXCLAMATION MARK", '!'),
    ("QUOTATION MARK", '"'),
    ("NUMBER SIGN", '#'),
    ("DOLLAR SIGN", '$'),
    ("PERCENT SIGN", '%'),
    ("AMPERSAND", '&'),
    ("APOSTROPHE", '\''),
    ("LEFT PARENTHESIS", '('),
    ("RIGHT PARENTHESIS", ')'),
    ("ASTERISK", '*'),
    ("PLUS SIGN", '+'),
    ("COMMA", ','),
    ("HYPHEN-MINUS", '-'),
    ("FULL STOP", '.'),
    ("SOLIDUS", '/'),
    ("COLON", ':'),
    ("SEMICOLON", ';'),
    ("LESS-THAN SIGN", '<'),
    ("EQUALS SIGN", '='),
    ("GREATER-THAN SIGN", '>'),
    ("QUESTION MARK", '?'),
    ("COMMERCIAL AT", '@'),
    ("LEFT SQUARE BRACKET", '['),
    ("REVERSE SOLIDUS", '\\'),
    ("RIGHT SQUARE BRACKET", ']'),
    ("CIRCUMFLEX ACCENT", '^'),
    ("LOW LINE", '_'),
    ("GRAVE ACCENT", '`'),
    ("LEFT CURLY BRACKET", '{'),
    ("VERTICAL LINE", '|'),
    ("RIGHT CURLY BRACKET", '}'),
    ("TILDE", '~'),
];

/// The printable ASCII character a `\N{NAME}` names, or `None` (a name of a
/// non-ASCII or control character, or no character: the caller keeps a
/// [`HOLE`]). Python matches the name case-insensitively and exactly (no
/// extra spaces). Covered: all 95 printable ASCII characters —
/// `LATIN SMALL|CAPITAL LETTER A..Z` and `DIGIT ZERO..NINE` computed, the rest
/// from [`ASCII_PUNCTUATION_NAMES`] — plus the alias `SP` (SPACE) from
/// Unicode's NameAliases.txt, which Python accepts.
fn ascii_by_name(name: &str) -> Option<char> {
    const DIGITS: [&str; 10] = [
        "ZERO", "ONE", "TWO", "THREE", "FOUR", "FIVE", "SIX", "SEVEN", "EIGHT", "NINE",
    ];
    let upper = name.to_ascii_uppercase();
    let letter = |prefix: &str, base: u8| {
        upper
            .strip_prefix(prefix)
            .filter(|l| l.len() == 1 && l.as_bytes()[0].is_ascii_uppercase())
            .map(|l| char::from(base + (l.as_bytes()[0] - b'A')))
    };
    if let Some(c) =
        letter("LATIN SMALL LETTER ", b'a').or_else(|| letter("LATIN CAPITAL LETTER ", b'A'))
    {
        return Some(c);
    }
    if let Some(d) = upper.strip_prefix("DIGIT ") {
        return DIGITS
            .iter()
            .position(|n| *n == d)
            .and_then(|k| u32::try_from(k).ok())
            .and_then(|k| char::from_digit(k, 10));
    }
    if upper == "SP" {
        return Some(' ');
    }
    ASCII_PUNCTUATION_NAMES
        .iter()
        .find(|(n, _)| *n == upper)
        .map(|(_, c)| *c)
}

/// Exactly `width` hex digits at `chars[at..]`, or `None` (Python rejects a
/// shorter `\x`/`\u`/`\U` escape as a syntax error).
fn hex(chars: &[char], at: usize, width: usize) -> Option<u32> {
    let digits = chars.get(at..at + width)?;
    digits
        .iter()
        .try_fold(0u32, |n, d| Some(n * 16 + d.to_digit(16)?))
}

#[cfg(test)]
mod tests {
    use super::{decode_non_raw, Decoded, HOLE};

    fn value(s: &str) -> Decoded {
        Decoded::Value(s.to_string())
    }

    #[test]
    fn backslash_newline_is_removed() {
        assert_eq!(decode_non_raw("/etc/ho\\\nsts", false), value("/etc/hosts"));
        assert_eq!(decode_non_raw("/e\\\r\ntc", true), value("/etc"));
    }

    #[test]
    fn common_escapes_decode_like_python() {
        assert_eq!(
            decode_non_raw("\\x2fetc\\057x\\u0041\\U00000042\\\\\\'", false),
            value("/etc/xAB\\'")
        );
        assert_eq!(decode_non_raw("a\\nb\\tc", false), value("a\nb\tc"));
    }

    #[test]
    fn unknown_and_bytes_only_escapes_are_kept() {
        assert_eq!(decode_non_raw("\\d\\q", false), value("\\d\\q"));
        assert_eq!(
            decode_non_raw("\\u0041\\N{x}", true),
            value("\\u0041\\N{x}")
        );
    }

    #[test]
    fn non_ascii_named_escape_is_one_hole_not_a_guess() {
        assert_eq!(
            decode_non_raw("/tmp/\\N{SNOWMAN}/x\\N{NOT A NAME}", false),
            Decoded::Holed(format!("/tmp/{HOLE}/x{HOLE}"))
        );
    }

    #[test]
    fn ascii_named_escapes_decode_case_insensitively() {
        assert_eq!(
            decode_non_raw(
                "\\N{SOLIDUS}\\N{latin small letter e}tc/\\N{Latin Capital Letter H}\\N{DIGIT SEVEN}\
\\N{FULL STOP}\\N{HYPHEN-MINUS}\\N{LOW LINE}\\N{REVERSE SOLIDUS}\\N{SP}\\N{SPACE}\\N{TILDE}",
                false
            ),
            value("/etc/H7.-_\\  ~")
        );
        // Exact match only (Python rejects the extra space; blastguard keeps a hole).
        assert_eq!(
            decode_non_raw("\\N{LATIN  SMALL LETTER E}", false),
            Decoded::Holed(HOLE.to_string())
        );
    }

    #[test]
    fn all_95_printable_ascii_names_resolve() {
        use super::ascii_by_name;
        let mut names: Vec<(String, char)> = super::ASCII_PUNCTUATION_NAMES
            .iter()
            .map(|(n, c)| ((*n).to_string(), *c))
            .collect();
        for (k, d) in [
            "ZERO", "ONE", "TWO", "THREE", "FOUR", "FIVE", "SIX", "SEVEN", "EIGHT", "NINE",
        ]
        .iter()
        .enumerate()
        {
            names.push((
                format!("DIGIT {d}"),
                char::from(b'0' + u8::try_from(k).unwrap()),
            ));
        }
        for l in b'A'..=b'Z' {
            names.push((
                format!("LATIN CAPITAL LETTER {}", char::from(l)),
                char::from(l),
            ));
            names.push((
                format!("LATIN SMALL LETTER {}", char::from(l)),
                char::from(l.to_ascii_lowercase()),
            ));
        }
        assert_eq!(names.len(), 95);
        let mut seen: Vec<char> = Vec::new();
        for (n, c) in &names {
            assert_eq!(ascii_by_name(n), Some(*c), "{n}");
            seen.push(*c);
        }
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen, (' '..='~').collect::<Vec<char>>());
    }

    #[test]
    fn malformed_is_undecodable_not_a_guess() {
        for body in [
            "\\x2",
            "\\uD800",
            "\\N",
            "\\Nx",
            "\\N{}",
            "\\N{open",
            "a\u{FDD0}",
        ] {
            assert_eq!(
                decode_non_raw(body, false),
                Decoded::Undecodable,
                "{body:?}"
            );
        }
    }
}
