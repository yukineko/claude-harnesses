//! Static evaluation of a `printf` stage that feeds an interpreter's stdin
//! (backlog 00569f7d).
//!
//! `printf 'chm\x6fd 777 /etc/hosts' | bash` hands the shell `chmod 777
//! /etc/hosts`: printf expands escapes and conversions before the text is
//! piped. Reading the quoted literals verbatim (as `echo` needs) judged
//! `chm\x6fd …` and missed the command. This module evaluates the stage the
//! way printf does so the OUTPUT can be judged as the program.
//!
//! The answer is three-valued ([`PrintfStage`]): not a printf stage, its exact
//! output, or "cannot be evaluated" (an operand that is not a literal word, an
//! unknown conversion, an oversized result). The last is never read as
//! "harmless": the caller records an Ask.

use super::{is_exec_wrapper, normalized_command, wrapped_command_heads};

/// What [`evaluate_printf_stage`] found.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PrintfStage {
    /// The stage's command (behind any exec-wrapper chain) is not `printf`.
    NotPrintf,
    /// Everything the stage writes to stdout.
    Output(String),
    /// A printf stage whose output cannot be determined statically; the text
    /// says why.
    Undetermined(String),
}

const MAX_OUTPUT: usize = 1 << 16;
const MAX_WIDTH: usize = 4096;

/// Evaluate `stage` (one pipe stage, as text) when it is a `printf`.
pub(super) fn evaluate_printf_stage(stage: &str) -> PrintfStage {
    let Some(raw) = split_raw_words(stage) else {
        // An unbalanced quote: not parseable. Only claim "printf" when the
        // first word says so; any other stage keeps its existing verdicts.
        let first = stage.split_whitespace().next().unwrap_or("");
        return if matches!(
            normalized_command(first).as_str(),
            "printf" | "env" | "command"
        ) {
            PrintfStage::Undetermined("its quoting cannot be parsed".into())
        } else {
            PrintfStage::NotPrintf
        };
    };
    let words = drop_redirects(raw);
    let refs: Vec<&str> = words.iter().map(String::as_str).collect();
    let heads = wrapped_command_heads(&refs);
    let Some(&last) = heads.last() else {
        return PrintfStage::NotPrintf;
    };
    if normalized_command(refs[last]) != "printf" {
        return PrintfStage::NotPrintf;
    }
    // Same rule as `stage_emits_only_its_arguments`: any other head candidate
    // must be a wrapper, else the chain is ambiguous and the caller's
    // "not a literal emitter" Ask applies.
    if !heads[..heads.len() - 1].iter().all(|&i| {
        let n = normalized_command(refs[i]);
        n == "printf" || is_exec_wrapper(&n)
    }) {
        return PrintfStage::NotPrintf;
    }
    let mut operands: Vec<Vec<u8>> = Vec::new();
    for w in &refs[last + 1..] {
        match literal_value(w) {
            Some(v) => operands.push(v),
            None => {
                return PrintfStage::Undetermined(format!(
                    "the printf operand `{w}` is not a literal word (expansion, glob or \
unparseable quoting)"
                ))
            }
        }
    }
    if operands.first().is_some_and(|f| f == b"--") {
        operands.remove(0);
    }
    let Some(fmt) = operands.first().cloned() else {
        return PrintfStage::Output(String::new());
    };
    if fmt.first() == Some(&b'-') && fmt.len() > 1 {
        return PrintfStage::Undetermined(
            "printf is given an option (`-v` …) that blastguard does not model".into(),
        );
    }
    match run_printf(&fmt, &operands[1..]) {
        Ok(bytes) => PrintfStage::Output(String::from_utf8_lossy(&bytes).into_owned()),
        Err(why) => PrintfStage::Undetermined(why),
    }
}

/// Quote-aware split on unquoted whitespace, keeping each word's raw text.
/// `None` on an unterminated quote.
fn split_raw_words(stage: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = stage.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        match c {
            '\\' => {
                cur.push(c);
                if let Some(&n) = chars.get(k + 1) {
                    cur.push(n);
                }
                have = true;
                k += 2;
                continue;
            }
            '\'' | '"' => {
                have = true;
                cur.push(c);
                k += 1;
                loop {
                    let &d = chars.get(k)?;
                    cur.push(d);
                    k += 1;
                    if c == '"' && d == '\\' {
                        cur.push(*chars.get(k)?);
                        k += 1;
                    } else if d == c {
                        break;
                    }
                }
                continue;
            }
            c if c.is_whitespace() => {
                if have {
                    words.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            _ => {
                cur.push(c);
                have = true;
            }
        }
        k += 1;
    }
    if have {
        words.push(cur);
    }
    Some(words)
}

/// Remove redirection words (`2>/dev/null`, `>f`, `2>&1`, `> f`): they do not
/// change what printf writes to the pipe.
fn drop_redirects(words: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for w in words {
        if skip_next {
            skip_next = false;
            continue;
        }
        let rest = w.trim_start_matches(|c: char| c.is_ascii_digit());
        if rest.starts_with('>') || rest.starts_with('<') {
            let target = rest.trim_start_matches(['>', '<', '&']);
            skip_next = target.is_empty() && !rest.ends_with('&');
            continue;
        }
        out.push(w);
    }
    out
}

/// The value of one raw shell word when it is fully determined by its text:
/// plain, single-quoted and double-quoted pieces without any expansion.
fn literal_value(raw: &str) -> Option<Vec<u8>> {
    let chars: Vec<char> = raw.chars().collect();
    if matches!(chars.first(), Some('~' | '#')) {
        return None;
    }
    let mut out = String::new();
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        match c {
            '\\' => {
                out.push(*chars.get(k + 1)?);
                k += 2;
            }
            '\'' => {
                k += 1;
                loop {
                    let d = *chars.get(k)?;
                    k += 1;
                    if d == '\'' {
                        break;
                    }
                    out.push(d);
                }
            }
            '"' => {
                k += 1;
                loop {
                    let d = *chars.get(k)?;
                    k += 1;
                    match d {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' => {
                            let n = *chars.get(k)?;
                            k += 1;
                            if !matches!(n, '$' | '`' | '"' | '\\' | '\n') {
                                out.push('\\');
                            }
                            if n != '\n' {
                                out.push(n);
                            }
                        }
                        _ => out.push(d),
                    }
                }
            }
            '$' | '`' | '*' | '?' | '[' | '{' | '(' | ')' | ';' | '&' | '|' | '<' | '>' | '!' => {
                return None
            }
            _ => {
                out.push(c);
                k += 1;
            }
        }
    }
    Some(out.into_bytes())
}

// ---------------------------------------------------------------- printf

type Undet = String;

fn run_printf(fmt: &[u8], args: &[Vec<u8>]) -> Result<Vec<u8>, Undet> {
    let mut out = Vec::new();
    let mut argi = 0;
    loop {
        let start = argi;
        if run_format(fmt, args, &mut argi, &mut out)? {
            break;
        }
        if out.len() > MAX_OUTPUT {
            return Err("the printf output is too large to evaluate".into());
        }
        if argi >= args.len() || argi == start {
            break;
        }
    }
    Ok(out)
}

fn next_arg<'a>(args: &'a [Vec<u8>], argi: &mut usize) -> Option<&'a [u8]> {
    let a = args.get(*argi)?;
    *argi += 1;
    Some(a)
}

/// One pass over the format. `Ok(true)` when `\c` stopped all output.
fn run_format(
    fmt: &[u8],
    args: &[Vec<u8>],
    argi: &mut usize,
    out: &mut Vec<u8>,
) -> Result<bool, Undet> {
    let mut i = 0;
    while i < fmt.len() {
        match fmt[i] {
            b'\\' => {
                let (bytes, next, stop) = escape_at(fmt, i, false);
                out.extend(bytes);
                if stop {
                    return Ok(true);
                }
                i = next;
            }
            b'%' => {
                if fmt.get(i + 1) == Some(&b'%') {
                    out.push(b'%');
                    i += 2;
                    continue;
                }
                let (spec, next) = parse_spec(fmt, i + 1, args, argi)?;
                i = next;
                if apply_spec(&spec, args, argi, out)? {
                    return Ok(true);
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
        if out.len() > MAX_OUTPUT {
            return Err("the printf output is too large to evaluate".into());
        }
    }
    Ok(false)
}

/// Decode the escape at `s[i] == '\\'` (`%b` rules when `b_mode`).
/// Returns `(bytes, next index, stop)`.
fn escape_at(s: &[u8], i: usize, b_mode: bool) -> (Vec<u8>, usize, bool) {
    let Some(&n) = s.get(i + 1) else {
        return (vec![b'\\'], i + 1, false);
    };
    let one = |b: u8| (vec![b], i + 2, false);
    match n {
        b'\\' => one(b'\\'),
        b'a' => one(7),
        b'b' => one(8),
        b'e' | b'E' => one(27),
        b'f' => one(12),
        b'n' => one(b'\n'),
        b'r' => one(b'\r'),
        b't' => one(b'\t'),
        b'v' => one(11),
        b'"' => one(b'"'),
        b'\'' => one(b'\''),
        b'?' => one(b'?'),
        b'c' => (Vec::new(), i + 2, true),
        b'0'..=b'7' => {
            // `%b`: `\0` takes up to three MORE digits; the format's `\NNN`
            // takes up to three in all (`\0157` is `\015` then `7`).
            let from = if b_mode && n == b'0' { i + 2 } else { i + 1 };
            let mut v: u32 = 0;
            let mut j = from;
            while j < s.len() && j - from < 3 && (b'0'..=b'7').contains(&s[j]) {
                v = v * 8 + u32::from(s[j] - b'0');
                j += 1;
            }
            (vec![(v & 0xFF) as u8], j, false)
        }
        b'x' => {
            let mut v: u32 = 0;
            let mut j = i + 2;
            while j < s.len() && j - (i + 2) < 2 && s[j].is_ascii_hexdigit() {
                v = v * 16 + hex(s[j]);
                j += 1;
            }
            if j == i + 2 {
                // No hex digit: printf writes `\x` literally.
                (b"\\x".to_vec(), i + 2, false)
            } else {
                (vec![v as u8], j, false)
            }
        }
        b'u' | b'U' => {
            let max = if n == b'u' { 4 } else { 8 };
            let mut v: u32 = 0;
            let mut j = i + 2;
            while j < s.len() && j - (i + 2) < max && s[j].is_ascii_hexdigit() {
                v = v.saturating_mul(16).saturating_add(hex(s[j]));
                j += 1;
            }
            if j == i + 2 {
                (vec![b'\\', n], i + 2, false)
            } else {
                let ch = char::from_u32(v).unwrap_or('\u{FFFD}');
                (ch.to_string().into_bytes(), j, false)
            }
        }
        _ => (vec![b'\\', n], i + 2, false),
    }
}

fn hex(b: u8) -> u32 {
    (b as char).to_digit(16).unwrap_or(0)
}

#[derive(Default)]
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero: bool,
    width: usize,
    precision: Option<usize>,
    conv: u8,
}

fn parse_spec(
    fmt: &[u8],
    mut i: usize,
    args: &[Vec<u8>],
    argi: &mut usize,
) -> Result<(Spec, usize), Undet> {
    let mut sp = Spec::default();
    while let Some(&f) = fmt.get(i) {
        match f {
            b'-' => sp.left = true,
            b'+' => sp.plus = true,
            b' ' => sp.space = true,
            b'#' => sp.alt = true,
            b'0' => sp.zero = true,
            b'\'' => {}
            _ => break,
        }
        i += 1;
    }
    let star = |argi: &mut usize| -> Result<i64, Undet> {
        let a = next_arg(args, argi).unwrap_or(b"0");
        parse_int(a).ok_or_else(|| "a printf `*` width is not a number".to_string())
    };
    if fmt.get(i) == Some(&b'*') {
        let w = star(argi)?;
        if w < 0 {
            sp.left = true;
        }
        sp.width = w.unsigned_abs() as usize;
        i += 1;
    } else {
        while let Some(d) = fmt.get(i).filter(|d| d.is_ascii_digit()) {
            sp.width = sp
                .width
                .saturating_mul(10)
                .saturating_add(usize::from(d - b'0'));
            i += 1;
        }
    }
    if fmt.get(i) == Some(&b'.') {
        i += 1;
        if fmt.get(i) == Some(&b'*') {
            let p = star(argi)?;
            sp.precision = (p >= 0).then_some(p as usize);
            i += 1;
        } else {
            let mut p = 0usize;
            while let Some(d) = fmt.get(i).filter(|d| d.is_ascii_digit()) {
                p = p.saturating_mul(10).saturating_add(usize::from(d - b'0'));
                i += 1;
            }
            sp.precision = Some(p);
        }
    }
    while matches!(
        fmt.get(i),
        Some(b'h' | b'l' | b'L' | b'j' | b'z' | b't' | b'q')
    ) {
        i += 1;
    }
    let Some(&c) = fmt.get(i) else {
        return Err("the printf format ends inside a conversion".into());
    };
    sp.conv = c;
    if sp.width > MAX_WIDTH || sp.precision.is_some_and(|p| p > MAX_WIDTH) {
        return Err("a printf width/precision is too large to evaluate".into());
    }
    Ok((sp, i + 1))
}

fn pad(mut body: Vec<u8>, sp: &Spec, zero_ok: bool) -> Vec<u8> {
    if body.len() >= sp.width {
        return body;
    }
    let fill = sp.width - body.len();
    if sp.left {
        body.extend(std::iter::repeat_n(b' ', fill));
        body
    } else if sp.zero && zero_ok {
        // Zeros go after any sign / 0x prefix.
        let mut at = usize::from(matches!(body.first(), Some(b'+' | b'-' | b' ')));
        if body[at..].starts_with(b"0x") || body[at..].starts_with(b"0X") {
            at += 2;
        }
        let mut v = body[..at].to_vec();
        v.extend(std::iter::repeat_n(b'0', fill));
        v.extend(&body[at..]);
        v
    } else {
        let mut v = vec![b' '; fill];
        v.extend(body);
        v
    }
}

/// Apply one conversion. `Ok(true)` when a `%b` argument's `\c` stops output.
fn apply_spec(
    sp: &Spec,
    args: &[Vec<u8>],
    argi: &mut usize,
    out: &mut Vec<u8>,
) -> Result<bool, Undet> {
    let arg = next_arg(args, argi);
    match sp.conv {
        b's' | b'b' | b'c' => {
            let a = arg.unwrap_or(b"");
            let mut stop = false;
            let mut body = if sp.conv == b'b' {
                let mut v = Vec::new();
                let mut j = 0;
                while j < a.len() {
                    if a[j] == b'\\' {
                        let (bytes, next, st) = escape_at(a, j, true);
                        v.extend(bytes);
                        j = next;
                        if st {
                            stop = true;
                            break;
                        }
                    } else {
                        v.push(a[j]);
                        j += 1;
                    }
                }
                v
            } else if sp.conv == b'c' {
                a.iter().take(1).copied().collect()
            } else {
                a.to_vec()
            };
            if sp.conv != b'c' {
                if let Some(p) = sp.precision {
                    body.truncate(p);
                }
            }
            out.extend(pad(body, sp, false));
            Ok(stop)
        }
        b'd' | b'i' | b'o' | b'u' | b'x' | b'X' => {
            // A non-number prints 0 (printf reports the error on stderr and
            // carries on); only digits reach the program.
            let v = parse_int(arg.unwrap_or(b"0")).unwrap_or(0);
            let mut digits = match sp.conv {
                b'd' | b'i' => v.unsigned_abs().to_string(),
                b'o' => format!("{:o}", v as u64),
                b'u' => (v as u64).to_string(),
                b'x' => format!("{:x}", v as u64),
                _ => format!("{:X}", v as u64),
            };
            if let Some(p) = sp.precision {
                while digits.len() < p {
                    digits.insert(0, '0');
                }
            }
            let mut s = String::new();
            if matches!(sp.conv, b'd' | b'i') {
                if v < 0 {
                    s.push('-');
                } else if sp.plus {
                    s.push('+');
                } else if sp.space {
                    s.push(' ');
                }
            } else if sp.alt && v != 0 {
                match sp.conv {
                    b'o' => s.push('0'),
                    b'x' => s.push_str("0x"),
                    b'X' => s.push_str("0X"),
                    _ => {}
                }
            }
            s.push_str(&digits);
            out.extend(pad(s.into_bytes(), sp, sp.precision.is_none()));
            Ok(false)
        }
        b'e' | b'E' | b'f' | b'F' | b'g' | b'G' => {
            let a = arg.unwrap_or(b"0");
            let v: f64 = std::str::from_utf8(a)
                .ok()
                .and_then(|t| t.trim().parse().ok())
                .ok_or_else(|| {
                    "a printf float conversion has a non-numeric argument".to_string()
                })?;
            let p = sp.precision.unwrap_or(6);
            // Output is digits, `.`, `e`, `+`, `-`, `inf`, `nan` only (it
            // cannot spell a command); `%g` here is the shortest round-trip
            // form, not C's 6-significant-digit one.
            let mut s = match sp.conv {
                b'f' | b'F' => format!("{v:.p$}"),
                b'e' => format!("{v:.p$e}"),
                b'E' => format!("{v:.p$E}"),
                _ => format!("{v}"),
            };
            if v >= 0.0 && sp.plus {
                s.insert(0, '+');
            }
            out.extend(pad(s.into_bytes(), sp, true));
            Ok(false)
        }
        other => Err(format!(
            "the printf conversion `%{}` is not modelled",
            other as char
        )),
    }
}

fn parse_int(a: &[u8]) -> Option<i64> {
    if a.is_empty() {
        return Some(0);
    }
    if matches!(a[0], b'\'' | b'"') {
        return a.get(1).map(|&c| i64::from(c));
    }
    let t = std::str::from_utf8(a).ok()?.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i64::from_str_radix(h, 16).ok()?
    } else if t.len() > 1 && t.starts_with('0') {
        i64::from_str_radix(&t[1..], 8).ok()?
    } else {
        t.parse::<i64>().ok()?
    };
    Some(if neg { -v } else { v })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(stage: &str) -> String {
        let r = evaluate_printf_stage(stage);
        assert!(matches!(r, PrintfStage::Output(_)), "`{stage}` -> {r:?}");
        match r {
            PrintfStage::Output(s) => s,
            _ => String::new(),
        }
    }
    fn undet(stage: &str) -> bool {
        matches!(evaluate_printf_stage(stage), PrintfStage::Undetermined(_))
    }

    #[test]
    fn hex_and_octal_escapes() {
        assert_eq!(
            out(r"printf 'chm\x6fd 777 /etc/hosts'"),
            "chmod 777 /etc/hosts"
        );
        assert_eq!(
            out(r"printf 'chm\157d 777 /etc/hosts'"),
            "chmod 777 /etc/hosts"
        );
        assert_eq!(out(r"printf 'chm\0157d'"), "chm\r7d");
        assert_eq!(out(r"printf 'chm\x6d'"), "chmm");
        assert_eq!(out(r"printf 'a\x6'"), "a\u{6}");
    }

    #[test]
    fn x_without_digit_is_literal() {
        assert_eq!(out(r"printf 'a\x'"), r"a\x");
        assert_eq!(out(r"printf 'a\xg'"), r"a\xg");
    }

    #[test]
    fn plain_literals_unchanged() {
        assert_eq!(out("printf 'echo hi'"), "echo hi");
        assert_eq!(out(r"printf 'echo hi\n'"), "echo hi\n");
        assert_eq!(out("printf 'echo 100%%'"), "echo 100%");
        assert_eq!(out("printf 'ls -la'"), "ls -la");
    }

    #[test]
    fn string_conversions_with_width() {
        assert_eq!(out("printf '%s' 'import os'"), "import os");
        assert_eq!(out(r"printf '%s\n' 'echo hi'"), "echo hi\n");
        assert_eq!(
            out("printf '%3sod 777 /etc/hosts' chm"),
            "chmod 777 /etc/hosts"
        );
        assert_eq!(out("printf '%1sod x' chm"), "chmod x");
        assert_eq!(out("printf '%-3sod x' chm"), "chmod x");
        assert_eq!(out("printf '%5s|' ab"), "   ab|");
        assert_eq!(out("printf '%-5s|' ab"), "ab   |");
        assert_eq!(out("printf '%.2s' abcdef"), "ab");
        assert_eq!(out("printf '%*s' 4 ab"), "  ab");
    }

    #[test]
    fn format_is_reused_while_arguments_remain() {
        assert_eq!(
            out("printf '%s ' chmod 777 /etc/hosts"),
            "chmod 777 /etc/hosts "
        );
        assert_eq!(out("printf 'a' x y"), "a");
        assert_eq!(out("printf '%s%s' ab"), "ab");
    }

    #[test]
    fn b_conversion_interprets_escapes() {
        assert_eq!(out(r"printf '%b' 'chm\x6fd'"), "chmod");
        assert_eq!(out(r"printf '%b' 'chm\0157d'"), "chmod");
        assert_eq!(out(r"printf '%b' 'ab\cde'"), "ab");
        assert_eq!(out(r"printf '%s' 'chm\x6fd'"), r"chm\x6fd");
    }

    #[test]
    fn backslash_c_stops_everything() {
        assert_eq!(out(r"printf 'ab\cde%s' x"), "ab");
    }

    #[test]
    fn numbers() {
        assert_eq!(out("printf '%d' 42"), "42");
        assert_eq!(out("printf '%x' 255"), "ff");
        assert_eq!(out("printf '%05d' 42"), "00042");
        assert_eq!(out("printf '%c' hello"), "h");
    }

    #[test]
    fn wrappers_and_assignments_resolve() {
        assert_eq!(out(r"env printf 'chm\x6fd'"), "chmod");
        assert_eq!(out(r"command printf 'chm\x6fd'"), "chmod");
        assert_eq!(out(r"FOO=1 printf 'chm\x6fd'"), "chmod");
        assert_eq!(out(r"printf -- 'chm\x6fd'"), "chmod");
        assert_eq!(out(r"printf 'chm\x6fd' 2>/dev/null"), "chmod");
    }

    #[test]
    fn non_printf_stages() {
        assert_eq!(evaluate_printf_stage("echo hi"), PrintfStage::NotPrintf);
        assert_eq!(evaluate_printf_stage("cat f"), PrintfStage::NotPrintf);
    }

    #[test]
    fn undeterminable_is_undetermined() {
        assert!(undet("printf 'chm\\x6fd %s' $X"));
        assert!(undet("printf \"chm$X\""));
        assert!(undet("printf 'a' `ls`"));
        assert!(undet("printf '%s' *"));
        assert!(undet("printf '%q' x"));
        assert!(undet("printf -v x y"));
        assert!(undet("printf '%s' $'a'"));
        assert!(undet("printf '%99999s' x"));
    }
}
