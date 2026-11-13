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
//! `None` — NOT a guessed value — when the body cannot be decoded statically:
//! a `\N{…}` named escape (no Unicode name table here), a malformed `\x`/`\u`
//! /`\U`, or a code point that is not a `char`. The caller then keeps the
//! reading it had before; it must not treat `None` as "no literal".

/// Decode `body` (the text between the quotes, prefix and quotes excluded) of
/// a non-raw literal. `bytes` is true for a `b''`/`B''` literal.
pub(super) fn decode_non_raw(body: &str, bytes: bool) -> Option<String> {
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
            'N' if !bytes => return None,
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
    use super::decode_non_raw;

    #[test]
    fn backslash_newline_is_removed() {
        assert_eq!(
            decode_non_raw("/etc/ho\\\nsts", false).as_deref(),
            Some("/etc/hosts")
        );
        assert_eq!(decode_non_raw("/e\\\r\ntc", true).as_deref(), Some("/etc"));
    }

    #[test]
    fn common_escapes_decode_like_python() {
        assert_eq!(
            decode_non_raw("\\x2fetc\\057x\\u0041\\U00000042\\\\\\'", false).as_deref(),
            Some("/etc/xAB\\'")
        );
        assert_eq!(
            decode_non_raw("a\\nb\\tc", false).as_deref(),
            Some("a\nb\tc")
        );
    }

    #[test]
    fn unknown_and_bytes_only_escapes_are_kept() {
        assert_eq!(decode_non_raw("\\d\\q", false).as_deref(), Some("\\d\\q"));
        assert_eq!(
            decode_non_raw("\\u0041\\N{x}", true).as_deref(),
            Some("\\u0041\\N{x}")
        );
    }

    #[test]
    fn undecodable_is_none_not_a_guess() {
        assert_eq!(decode_non_raw("\\N{SOLIDUS}etc", false), None);
        assert_eq!(decode_non_raw("\\x2", false), None);
        assert_eq!(decode_non_raw("\\uD800", false), None);
    }
}
