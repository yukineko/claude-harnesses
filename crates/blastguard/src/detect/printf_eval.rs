//! Static evaluation of a `printf` stage that feeds an interpreter's stdin
//! (backlog 00569f7d).
//!
//! `printf 'chm\x6fd 777 /etc/hosts' | bash` hands the shell `chmod 777
//! /etc/hosts`: printf expands escapes and conversions before the text is
//! piped. Reading the quoted literals verbatim (as `echo` needs) judged
//! `chm\x6fd …` and missed the command. This module evaluates the stage the
//! way printf does so the OUTPUT can be judged as the program.
//!
//! The answer is three-valued ([`PrintfStage`]): not a printf stage, its
//! possible outputs, or "cannot be evaluated" (an operand that is not a literal
//! word, an unknown conversion, a number argument the printfs read differently,
//! an oversized result). The last is never read as "harmless": the caller
//! records an Ask.
//!
//! WHICH printf runs the stage is not known: the Bash tool's shell is zsh, a
//! nested `sh -c` / `bash -c` runs bash (3.2 on macOS, 5.x elsewhere), and
//! `env printf` runs the external BSD or GNU binary. Their escape handling
//! differs (observed 2026-11-14 on this machine for zsh 5.9, bash 3.2.57 and
//! `/usr/bin/printf`): zsh turns a bare `\x` into NUL, BSD drops the backslash
//! of an unknown escape (`\od` is `od`), bash keeps `\c` in the format
//! literally while zsh stops there. So the stage is evaluated once per
//! [`Dialect`] and EVERY distinct output is returned; the caller judges each
//! one, and a Deny from any reading wins. The dialect list is the set of
//! printfs modelled, not every printf in existence (dash and busybox are not
//! modelled separately).

use super::{is_exec_wrapper, normalized_command, wrapped_command_heads};

/// What [`evaluate_printf_stage`] found.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum PrintfStage {
    /// The stage's command (behind any exec-wrapper chain) is not `printf`.
    NotPrintf,
    /// Everything the stage writes to stdout, once per distinct reading (one
    /// per [`Dialect`] whose output differs; never empty).
    Outputs(Vec<String>),
    /// A printf stage whose output cannot be determined statically under at
    /// least one dialect; the text says why.
    Undetermined(String),
}

/// A printf implementation whose escape rules are modelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Dialect {
    /// bash 3.2 builtin (`/bin/bash`, `/bin/sh` on macOS): no `\u`.
    Bash32,
    /// bash 4.2+ builtin: `\u` / `\U`.
    Bash5,
    /// zsh builtin (the Bash tool's shell).
    Zsh,
    /// BSD `/usr/bin/printf` (macOS `env printf`).
    Bsd,
    /// GNU coreutils `printf` (Linux `env printf`).
    Gnu,
}

const DIALECTS: [Dialect; 5] = [
    Dialect::Bash32,
    Dialect::Bash5,
    Dialect::Zsh,
    Dialect::Bsd,
    Dialect::Gnu,
];

const MAX_OUTPUT: usize = 1 << 16;
const MAX_WIDTH: usize = 4096;

/// Evaluate `stage` (one pipe stage, as text) when it is a `printf`, under
/// every modelled [`Dialect`].
pub(super) fn evaluate_printf_stage(stage: &str) -> PrintfStage {
    match printf_operands(stage) {
        Err(stage) => stage,
        Ok(None) => PrintfStage::Outputs(vec![String::new()]),
        Ok(Some((fmt, args))) => {
            let mut outputs: Vec<String> = Vec::new();
            for d in DIALECTS {
                match run_printf(&fmt, &args, d) {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        if !outputs.contains(&text) {
                            outputs.push(text);
                        }
                    }
                    Err(why) => return PrintfStage::Undetermined(why),
                }
            }
            PrintfStage::Outputs(outputs)
        }
    }
}

/// The literal format and arguments of a printf stage: `Ok(None)` for a bare
/// `printf` (no format), `Err` with the final answer for a stage that is not
/// printf or whose operands cannot be read.
#[allow(clippy::type_complexity)]
fn printf_operands(stage: &str) -> Result<Option<(Vec<u8>, Vec<Vec<u8>>)>, PrintfStage> {
    let Some(raw) = split_raw_words(stage) else {
        // An unbalanced quote: not parseable. Only claim "printf" when the
        // first word says so; any other stage keeps its existing verdicts.
        let first = stage.split_whitespace().next().unwrap_or("");
        return Err(
            if matches!(
                normalized_command(first).as_str(),
                "printf" | "env" | "command"
            ) {
                PrintfStage::Undetermined("its quoting cannot be parsed".into())
            } else {
                PrintfStage::NotPrintf
            },
        );
    };
    let words = drop_redirects(raw);
    let refs: Vec<&str> = words.iter().map(String::as_str).collect();
    let heads = wrapped_command_heads(&refs);
    let Some(&last) = heads.last() else {
        return Err(PrintfStage::NotPrintf);
    };
    if normalized_command(refs[last]) != "printf" {
        return Err(PrintfStage::NotPrintf);
    }
    // Same rule as `stage_emits_only_its_arguments`: any other head candidate
    // must be a wrapper, else the chain is ambiguous and the caller's
    // "not a literal emitter" Ask applies.
    if !heads[..heads.len() - 1].iter().all(|&i| {
        let n = normalized_command(refs[i]);
        n == "printf" || is_exec_wrapper(&n)
    }) {
        return Err(PrintfStage::NotPrintf);
    }
    let mut operands: Vec<Vec<u8>> = Vec::new();
    for w in &refs[last + 1..] {
        match literal_value(w) {
            Some(v) => operands.push(v),
            None => {
                return Err(PrintfStage::Undetermined(format!(
                    "the printf operand `{w}` is not a literal word (expansion, glob or \
unparseable quoting)"
                )))
            }
        }
    }
    if operands.first().is_some_and(|f| f == b"--") {
        operands.remove(0);
    }
    if operands.is_empty() {
        return Ok(None);
    }
    let fmt = operands.remove(0);
    if fmt.first() == Some(&b'-') && fmt.len() > 1 {
        return Err(PrintfStage::Undetermined(
            "printf is given an option (`-v` …) that blastguard does not model".into(),
        ));
    }
    Ok(Some((fmt, operands)))
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

fn run_printf(fmt: &[u8], args: &[Vec<u8>], d: Dialect) -> Result<Vec<u8>, Undet> {
    let mut out = Vec::new();
    let mut argi = 0;
    loop {
        let start = argi;
        if run_format(fmt, args, &mut argi, &mut out, d)? {
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

/// One pass over the format. `Ok(true)` when output stopped for good (`\c`,
/// or an escape GNU printf rejects as fatal).
fn run_format(
    fmt: &[u8],
    args: &[Vec<u8>],
    argi: &mut usize,
    out: &mut Vec<u8>,
    d: Dialect,
) -> Result<bool, Undet> {
    let mut i = 0;
    while i < fmt.len() {
        match fmt[i] {
            b'\\' => {
                let (bytes, next, stop) = escape_at(fmt, i, false, d);
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
                if apply_spec(&spec, args, argi, out, d)? {
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

/// Decode the escape at `s[i] == '\\'` the way dialect `d` does (`%b`
/// argument rules when `b_mode`). Returns `(bytes, next index, stop)`; `stop`
/// ends ALL output (`\c`, and GNU's fatal "missing hex digit" errors, after
/// which nothing more is written).
///
/// Observed 2026-11-14 (zsh 5.9, bash 3.2.57, macOS `/usr/bin/printf`); bash
/// 5 and GNU rules are from their documented escape lists:
/// - bare `\x` (no hex digit): bash `\x`, zsh NUL, GNU fatal, BSD `x`.
/// - `\u` / `\U`: bash 3.2 and BSD do not know it; bash 5 / zsh / GNU read
///   hex digits; with none, bash 5 writes `\u`, zsh NUL, GNU is fatal.
/// - `\'`, `\"`, `\?`: bash's FORMAT writes the character, its `%b` keeps the
///   backslash; zsh keeps it; GNU writes `"` only; BSD drops the backslash.
/// - `\c` in the format: bash and BSD write it as text (`\c` / `c`), zsh and
///   GNU stop. In `%b` every dialect stops.
/// - `\e`: all but BSD (`e`); `\E`: bash only.
/// - octal: the format takes up to 3 digits; `%b` takes `\0` plus up to 3,
///   and bash/BSD/GNU also take a bare `\NNN` there, zsh does not (literal).
/// - any other escape: BSD drops the backslash, the rest write both bytes.
fn escape_at(s: &[u8], i: usize, b_mode: bool, d: Dialect) -> (Vec<u8>, usize, bool) {
    use Dialect::{Bash32, Bash5, Bsd, Gnu, Zsh};
    let Some(&n) = s.get(i + 1) else {
        return (vec![b'\\'], i + 1, false);
    };
    let one = |b: u8| (vec![b], i + 2, false);
    let stop = (Vec::new(), i + 2, true);
    let unknown = if d == Bsd {
        (vec![n], i + 2, false)
    } else {
        (vec![b'\\', n], i + 2, false)
    };
    let bash = matches!(d, Bash32 | Bash5);
    match n {
        b'\\' => one(b'\\'),
        b'a' => one(7),
        b'b' => one(8),
        b'f' => one(12),
        b'n' => one(b'\n'),
        b'r' => one(b'\r'),
        b't' => one(b'\t'),
        b'v' => one(11),
        b'e' if d != Bsd => one(27),
        b'E' if bash => one(27),
        b'"' if (bash && !b_mode) || d == Gnu => one(n),
        b'\'' | b'?' if bash && !b_mode => one(n),
        b'c' if b_mode || matches!(d, Zsh | Gnu) => stop,
        b'0'..=b'7' => {
            if b_mode && d == Zsh && n != b'0' {
                return unknown;
            }
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
        b'x' if d != Bsd => {
            let mut v: u32 = 0;
            let mut j = i + 2;
            while j < s.len() && j - (i + 2) < 2 && s[j].is_ascii_hexdigit() {
                v = v * 16 + hex(s[j]);
                j += 1;
            }
            if j > i + 2 {
                (vec![v as u8], j, false)
            } else {
                match d {
                    Zsh => (vec![0], i + 2, false),
                    Gnu => stop,
                    _ => (b"\\x".to_vec(), i + 2, false),
                }
            }
        }
        b'u' | b'U' if matches!(d, Bash5 | Zsh | Gnu) => {
            let max = if n == b'u' { 4 } else { 8 };
            let mut v: u32 = 0;
            let mut j = i + 2;
            while j < s.len() && j - (i + 2) < max && s[j].is_ascii_hexdigit() {
                v = v.saturating_mul(16).saturating_add(hex(s[j]));
                j += 1;
            }
            let digits = j - (i + 2);
            match d {
                Gnu if digits != max => stop,
                Zsh if digits == 0 => (vec![0], i + 2, false),
                _ if digits == 0 => (vec![b'\\', n], i + 2, false),
                _ => {
                    let ch = char::from_u32(v).unwrap_or('\u{FFFD}');
                    (ch.to_string().into_bytes(), j, false)
                }
            }
        }
        _ => unknown,
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
        parse_int(a).ok_or_else(|| {
            "a printf `*` width is not a number every printf reads the same way".to_string()
        })
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

/// Apply one conversion. `Ok(true)` when a `%b` argument's `\c` (or a fatal
/// GNU escape) stops all output.
fn apply_spec(
    sp: &Spec,
    args: &[Vec<u8>],
    argi: &mut usize,
    out: &mut Vec<u8>,
    d: Dialect,
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
                        let (bytes, next, st) = escape_at(a, j, true, d);
                        // BSD formats a `%b` argument as a C string: a NUL
                        // ends that argument (observed: `%b|%s|` of `a\0b`,
                        // `x` writes `a|x|`).
                        if d == Dialect::Bsd && bytes == [0] {
                            break;
                        }
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
            // The printfs disagree on any other argument: zsh evaluates it
            // as an arithmetic expression (`1+1` is 2, a parameter name is
            // its value, `010` is 10), bash/BSD/GNU print 0 or read octal.
            let v = parse_int(arg.unwrap_or(b"")).ok_or_else(|| {
                format!(
                    "the printf `%{}` argument `{}` is not a number every printf reads the same \
way (zsh evaluates it as arithmetic)",
                    sp.conv as char,
                    String::from_utf8_lossy(arg.unwrap_or(b""))
                )
            })?;
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
            let v = plain_float(a).ok_or_else(|| {
                "a printf float conversion has an argument that is not a plain decimal number"
                    .to_string()
            })?;
            let p = sp.precision.unwrap_or(6);
            // Output is digits, `.`, `e`, `+`, `-` only (it cannot spell a
            // command); `%g` here is the shortest round-trip form, not C's
            // 6-significant-digit one.
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

/// An integer argument every modelled printf reads the same way: empty (0),
/// `'c` / `"c` (the character's code), a decimal without a leading zero, or
/// `0x` hex, each with an optional sign. `None` for anything else — leading
/// zeros (octal in bash, decimal in zsh), spaces, names, expressions.
fn parse_int(a: &[u8]) -> Option<i64> {
    if a.is_empty() {
        return Some(0);
    }
    if matches!(a[0], b'\'' | b'"') {
        return a.get(1).map(|&c| i64::from(c));
    }
    let t = std::str::from_utf8(a).ok()?;
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        if h.is_empty() || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        i64::from_str_radix(h, 16).ok()?
    } else {
        if t.is_empty()
            || !t.bytes().all(|b| b.is_ascii_digit())
            || (t.len() > 1 && t.starts_with('0'))
        {
            return None;
        }
        t.parse::<i64>().ok()?
    };
    Some(if neg { -v } else { v })
}

/// A float argument spelled as a plain decimal (`1`, `-2.5`, `.5`, `1e3`);
/// `None` for names, `inf`/`nan`, spaces or expressions (zsh evaluates those
/// arithmetically).
fn plain_float(a: &[u8]) -> Option<f64> {
    let t = std::str::from_utf8(a).ok()?;
    let body = t.strip_prefix(['-', '+']).unwrap_or(t);
    let (mantissa, exp) = match body.find(['e', 'E']) {
        Some(k) => (&body[..k], Some(&body[k + 1..])),
        None => (body, None),
    };
    let mut parts = mantissa.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next().unwrap_or("");
    let ok_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (int.is_empty() && frac.is_empty()) || !ok_digits(int) || !ok_digits(frac) {
        return None;
    }
    if let Some(e) = exp {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        if e.is_empty() || !ok_digits(e) {
            return None;
        }
    }
    t.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The output under one dialect.
    fn out_in(stage: &str, d: Dialect) -> String {
        let ops = printf_operands(stage);
        assert!(
            matches!(ops, Ok(Some(_))),
            "`{stage}` is not a printf with a format"
        );
        let Ok(Some((fmt, args))) = ops else {
            return String::new();
        };
        let r = run_printf(&fmt, &args, d);
        assert!(r.is_ok(), "`{stage}` under {d:?} -> {r:?}");
        String::from_utf8_lossy(&r.unwrap_or_default()).into_owned()
    }
    /// The bash 5 reading (what this module evaluated before dialects).
    fn out(stage: &str) -> String {
        out_in(stage, Dialect::Bash5)
    }
    fn outputs(stage: &str) -> Vec<String> {
        let r = evaluate_printf_stage(stage);
        assert!(matches!(r, PrintfStage::Outputs(_)), "`{stage}` -> {r:?}");
        let PrintfStage::Outputs(v) = r else {
            return Vec::new();
        };
        v
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
    fn x_without_digit_is_literal_in_bash() {
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
        // In the FORMAT only zsh and GNU stop at `\c`; bash writes it as text
        // (observed with bash 3.2.57: `printf 'a\c|b'` writes `a\c|b`).
        assert_eq!(out_in(r"printf 'ab\cde%s' x", Dialect::Zsh), "ab");
        assert_eq!(out_in(r"printf 'ab\cde%s' x", Dialect::Gnu), "ab");
        assert_eq!(out(r"printf 'ab\cde%s' x"), r"ab\cdex");
        // In a `%b` argument every dialect stops.
        for d in DIALECTS {
            assert_eq!(out_in(r"printf '%b%s' 'ab\cde' x", d), "ab", "{d:?}");
        }
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
        // zsh evaluates a number argument arithmetically; bash reads octal.
        assert!(undet("printf '%d' 1+1"));
        assert!(undet("printf '%d' x"));
        assert!(undet("printf '%d' 010"));
        assert!(undet("printf '%f' inf"));
    }

    #[test]
    fn plain_numbers_stay_determined() {
        assert_eq!(
            outputs("printf '%d|%x|%d' -5 0x10 \"'a\""),
            vec!["-5|10|97"]
        );
        assert_eq!(outputs("printf '%.1f' 2.5"), vec!["2.5"]);
    }

    #[test]
    fn nul_escapes_are_nul() {
        for d in DIALECTS {
            assert_eq!(out_in(r"printf 'chm\0od'", d), "chm\0od", "{d:?}");
            assert_eq!(out_in(r"printf 'chm\000od'", d), "chm\0od", "{d:?}");
        }
        assert_eq!(out_in(r"printf 'chm\x00od'", Dialect::Zsh), "chm\0od");
        assert_eq!(out_in(r"printf '%b' 'chm\0od'", Dialect::Bash32), "chm\0od");
        // BSD ends a `%b` argument at its NUL.
        assert_eq!(out_in(r"printf '%b|%s|' 'a\0b' x", Dialect::Bsd), "a|x|");
    }

    #[test]
    fn dialects_differ_and_every_reading_is_returned() {
        let v = outputs(r"printf 'chm\xod 777 /etc/hosts'");
        for want in [
            r"chm\xod 777 /etc/hosts",
            "chm\0od 777 /etc/hosts",
            "chmxod 777 /etc/hosts",
            "chm",
        ] {
            assert!(v.iter().any(|o| o == want), "{want:?} not in {v:?}");
        }
        assert_eq!(out_in(r"printf '%b' 'chm\xod'", Dialect::Zsh), "chm\0od");
        assert_eq!(out_in(r"printf 'a\x'", Dialect::Zsh), "a\0");
        // BSD drops the backslash of an escape it does not know.
        assert_eq!(out_in(r"printf 'chm\od'", Dialect::Bsd), "chmod");
        assert_eq!(out_in(r"printf 'chm\od'", Dialect::Bash5), r"chm\od");
        // `\c` in the format: zsh/GNU stop, bash keeps it, BSD writes `c`.
        assert_eq!(out_in(r"printf 'a\cb'", Dialect::Zsh), "a");
        assert_eq!(out_in(r"printf 'a\cb'", Dialect::Bash32), r"a\cb");
        assert_eq!(out_in(r"printf 'a\cb'", Dialect::Bsd), "acb");
        // `\u`: bash 3.2 does not know it.
        assert_eq!(out_in(r"printf 'a\u6f'", Dialect::Bash32), r"a\u6f");
        assert_eq!(out_in(r"printf 'a\u6f'", Dialect::Zsh), "ao");
        // `\?` in the format: bash writes `?`, zsh keeps the backslash.
        assert_eq!(out_in(r"printf 'a\?b'", Dialect::Bash5), "a?b");
        assert_eq!(out_in(r"printf 'a\?b'", Dialect::Zsh), r"a\?b");
        // zsh's `%b` reads only `\0NNN`.
        assert_eq!(out_in(r"printf '%b' 'a\1234'", Dialect::Zsh), r"a\1234");
        assert_eq!(out_in(r"printf '%b' 'a\1234'", Dialect::Bash32), "aS4");
    }
}
