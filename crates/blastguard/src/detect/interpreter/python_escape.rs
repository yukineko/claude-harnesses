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
//! What cannot be decoded statically is NEVER given a guessed value:
//! - a well-formed `\N{NAME}` named escape (str only) is exactly ONE code point
//!   blastguard cannot name (there is no Unicode name table here), so the value
//!   is [`Decoded::Holed`]: the decoded text with [`HOLE`] standing for each
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
                i += 1 + close + 1;
                out.push(HOLE);
                *holed = true;
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Some(out)
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
    fn named_escape_is_one_hole_not_a_guess() {
        assert_eq!(
            decode_non_raw("\\N{SOLIDUS}etc/ho\\N{LATIN SMALL LETTER S}ts", false),
            Decoded::Holed(format!("{HOLE}etc/ho{HOLE}ts"))
        );
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
