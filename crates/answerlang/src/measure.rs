//! Strip quoted / code / identifier material from an answer and measure the
//! share of letters written in the target language's script.
//!
//! What is removed (so that it counts neither for nor against the target):
//! fenced code blocks (``` / ~~~, whole block — an unclosed fence is NOT
//! treated as a fence, so prose after it still counts), inline code spans,
//! blockquote lines, quote lines (`$ `, `test result:`, `error:`, `error[`,
//! `warning:`, or a line wholly wrapped in `"…"` / `「…」`), URLs, file paths,
//! and identifier tokens (hex ids, run ids, version tokens, `name:line`).

use crate::lang::{script_of, Lang};

/// Fewer letters than this left after stripping → Clean by rule.
pub const MIN_LETTERS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub letters: usize,
    pub target: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    /// Too few letters to judge (< [`MIN_LETTERS`]); clean by rule.
    TooShort(Counts),
    Clean(Counts),
    Violation {
        counts: Counts,
        share: f64,
    },
}

pub fn judge(text: &str, lang: Lang, min_share: f64) -> Outcome {
    let counts = count(text, lang);
    if counts.letters < MIN_LETTERS {
        return Outcome::TooShort(counts);
    }
    let share = counts.target as f64 / counts.letters as f64;
    // An incomparable pair (a NaN on either side) resolves to a violation,
    // never to clean.
    match share.partial_cmp(&min_share) {
        Some(std::cmp::Ordering::Less) | None => Outcome::Violation { counts, share },
        Some(_) => Outcome::Clean(counts),
    }
}

pub fn count(text: &str, lang: Lang) -> Counts {
    let mut c = Counts {
        letters: 0,
        target: 0,
    };
    for line in prose_lines(text) {
        let line = strip_inline_code(line);
        for tok in line.split_whitespace() {
            count_token(tok, lang, &mut c);
        }
    }
    c
}

fn fence_marker(line: &str) -> Option<(char, usize)> {
    let t = line.trim_start();
    let ch = t.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let run = t.chars().take_while(|&c| c == ch).count();
    (run >= 3).then_some((ch, run))
}

fn is_closing_fence(line: &str, ch: char, len: usize) -> bool {
    let t = line.trim();
    let run = t.chars().take_while(|&c| c == ch).count();
    run >= len && t.chars().skip(run).all(char::is_whitespace)
}

fn is_quote_line(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with('>') {
        return true;
    }
    if t.starts_with("$ ")
        || t.starts_with("test result:")
        || t.starts_with("error:")
        || t.starts_with("error[")
        || t.starts_with("warning:")
    {
        return true;
    }
    let wrapped = |open: char, close: char| {
        t.chars().count() >= 2 && t.starts_with(open) && t.ends_with(close)
    };
    wrapped('"', '"') || wrapped('「', '」')
}

/// Lines that survive fence and quote-line removal.
fn prose_lines(text: &str) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some((ch, len)) = fence_marker(line) {
            if let Some(off) = lines[i + 1..]
                .iter()
                .position(|l| is_closing_fence(l, ch, len))
            {
                i += off + 2;
                continue;
            }
            // Unclosed fence: not a fence; keep counting what follows.
        }
        if !is_quote_line(line) {
            out.push(line);
        }
        i += 1;
    }
    out
}

/// Remove `code` spans. A backtick run is closed only by a run of the same
/// length on the same line; an unmatched run stays as literal text (and the
/// text after it keeps counting).
fn strip_inline_code(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let run = chars[i..].iter().take_while(|&&c| c == '`').count();
        let mut j = i + run;
        let mut close = None;
        while j < chars.len() {
            if chars[j] == '`' {
                let r = chars[j..].iter().take_while(|&&c| c == '`').count();
                if r == run {
                    close = Some(j + r);
                    break;
                }
                j += r;
            } else {
                j += 1;
            }
        }
        match close {
            Some(end) => {
                out.push(' ');
                i = end;
            }
            None => {
                for _ in 0..run {
                    out.push('`');
                }
                i += run;
            }
        }
    }
    out
}

fn count_token(tok: &str, lang: Lang, c: &mut Counts) {
    // URLs run up to whitespace.
    let cut = [tok.find("http://"), tok.find("https://")]
        .into_iter()
        .flatten()
        .min();
    let tok = match cut {
        Some(i) => &tok[..i],
        None => tok,
    };
    if tok.contains('/') || tok.starts_with('~') {
        return;
    }
    let keep = |ch: char| ch.is_alphanumeric() || matches!(ch, '.' | ':' | '-' | '_');
    for sub in tok.split(|ch: char| !keep(ch)) {
        let core = sub.trim_matches(|ch: char| matches!(ch, '.' | ':' | '-' | '_'));
        if core.is_empty() || is_identifier_noise(core) {
            continue;
        }
        for ch in core.chars().filter(|ch| ch.is_alphabetic()) {
            c.letters += 1;
            if lang.accepts(script_of(ch)) {
                c.target += 1;
            }
        }
    }
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_hex_id(t: &str) -> bool {
    (7..=40).contains(&t.len())
        && t.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && t.bytes().any(|b| b.is_ascii_digit())
}

/// `run-` followed by alnum/dash, containing at least one digit (so an
/// English word like `run-time` still counts as letters).
fn is_run_id(t: &str) -> bool {
    match t.strip_prefix("run-") {
        Some(rest) => {
            !rest.is_empty()
                && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && rest.bytes().any(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// `v?\d+(\.\d+)+`
fn is_version(t: &str) -> bool {
    let t = t.strip_prefix('v').unwrap_or(t);
    let parts: Vec<&str> = t.split('.').collect();
    parts.len() >= 2 && parts.iter().all(|p| all_digits(p))
}

/// Anything ending in `:<digits>` (which includes `:<digits>:<digits>`).
fn is_name_line(t: &str) -> bool {
    match t.rsplit_once(':') {
        Some((head, tail)) => !head.is_empty() && all_digits(tail),
        None => false,
    }
}

fn is_identifier_noise(t: &str) -> bool {
    is_hex_id(t) || is_run_id(t) || is_version(t) || is_name_line(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_tokens() {
        assert!(is_hex_id("a1fdc674"));
        assert!(!is_hex_id("deadbeef"));
        assert!(!is_hex_id("a1f"));
        assert!(is_run_id("run-20261112-072718-37987"));
        assert!(!is_run_id("run-time"));
        assert!(is_version("0.1.38"));
        assert!(is_version("v1.2"));
        assert!(!is_version("v1"));
        assert!(is_name_line("main.rs:42"));
        assert!(is_name_line("lib.rs:10:5"));
        assert!(!is_name_line("note:"));
    }

    #[test]
    fn unclosed_fence_keeps_counting() {
        let text = format!("```\n{}", "word ".repeat(20));
        assert_eq!(count(&text, Lang::Japanese).letters, 80);
    }

    #[test]
    fn inline_code_and_unmatched_backtick() {
        let c = count("abc `def` ghi", Lang::English);
        assert_eq!(c.letters, 6);
        let c2 = count("abc `def ghi", Lang::English);
        assert_eq!(c2.letters, 9);
    }

    #[test]
    fn nan_min_share_is_violation() {
        let text = "a".repeat(50);
        assert!(matches!(
            judge(&text, Lang::English, f64::NAN),
            Outcome::Violation { .. }
        ));
    }
}
