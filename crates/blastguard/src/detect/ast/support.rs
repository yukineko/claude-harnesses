//! Stateless helpers of the static evaluator (`eval.rs`): quoting / escape
//! decoding, lexical path joining, fd and stdin bookkeeping of redirections,
//! the `set` options that change nothing tracked, and what a pipeline stage
//! feeds the next one. None of them sees the evaluator's shell state; they
//! were split out of `eval.rs` to keep it under the repository's 2000-line
//! file cap (the behaviour is unchanged).

use super::{parse, Cause, FdTarget, RedirOp, RedirTarget, Redirect, SimpleCommand};
use super::{StdinSource, Value, Word};
use brush_parser::ast;
use brush_parser::word::WordPieceWithSource;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// `$'..'` with simple escapes only; anything else (`\x`, `\u`, octal, `\c`)
/// is `None`.
pub(super) fn decode_ansi_c(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        out.push(match it.next()? {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'a' => '\x07',
            'b' => '\x08',
            'e' | 'E' => '\x1b',
            'f' => '\x0c',
            'v' => '\x0b',
            '\\' => '\\',
            '\'' => '\'',
            '"' => '"',
            '?' => '?',
            _ => return None,
        });
    }
    Some(out)
}

/// The program inside backquotes: `\$`, `` \` ``, `\\` (and `\"` inside
/// double quotes) lose their backslash. The bool says whether anything
/// changed (then spans inside it are not exact).
pub(super) fn unescape_backquote(s: &str, in_dq: bool) -> (String, bool) {
    let mut out = String::new();
    let mut changed = false;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\\' {
            if let Some(&n) = it.peek() {
                if n == '$' || n == '`' || n == '\\' || (in_dq && n == '"') {
                    out.push(n);
                    it.next();
                    changed = true;
                    continue;
                }
            }
        }
        out.push(c);
    }
    (out, changed)
}

pub(super) fn parse_heredoc_contained(body: &str) -> Result<Vec<WordPieceWithSource>, Cause> {
    let options = parse::parser_options();
    match catch_unwind(AssertUnwindSafe(|| {
        brush_parser::word::parse_heredoc(body, &options)
    })) {
        Ok(Ok(pieces)) => Ok(pieces),
        Ok(Err(e)) => Err(Cause::Parse(e.to_string())),
        Err(_) => Err(Cause::Panic),
    }
}

/// `base` (absolute, normalised) + relative `rel`, lexically; `None` for a
/// `..` component or an empty `rel`.
pub(super) fn join_path(base: &str, rel: &str) -> Option<String> {
    if rel.is_empty() {
        return None;
    }
    let mut out = base.trim_end_matches('/').to_string();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            p => {
                out.push('/');
                out.push_str(p);
            }
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    Some(out)
}

/// An absolute `cd` target, normalised (`.` and empty components dropped);
/// `None` for `..`.
pub(super) fn normalise_abs(p: &str) -> Option<String> {
    join_path("/", p.trim_start_matches('/')).or_else(|| {
        if p.trim_matches('/').is_empty() {
            Some("/".to_string())
        } else {
            None
        }
    })
}

/// Bash reads `<&N` / `>&N` with the word already dequoted.
pub(super) fn parse_fd_target(s: &str) -> Option<FdTarget> {
    if s == "-" {
        return Some(FdTarget::Close);
    }
    let digits = |d: &str| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit());
    if let Some(d) = s.strip_suffix('-') {
        if digits(d) {
            return d.parse().ok().map(FdTarget::Move);
        }
    }
    if digits(s) {
        return s.parse().ok().map(FdTarget::Number);
    }
    None
}

pub(super) fn conv_fd(fd: Option<ast::IoFd>) -> Result<Option<u32>, Cause> {
    fd.map(|n| {
        u32::try_from(n)
            .map_err(|_| Cause::Unsupported(format!("negative fd {n} in a redirection")))
    })
    .transpose()
}

/// What a redirection does to stdin (fd 0), applied in order.
pub(super) fn apply_stdin(stdin: &mut StdinSource, r: &Redirect) {
    let default_in = matches!(
        r.op,
        RedirOp::In | RedirOp::InOut | RedirOp::DupIn | RedirOp::HereDoc | RedirOp::HereString
    );
    let on_stdin = match r.fd {
        Some(0) => true,
        Some(_) => false,
        None => default_in,
    };
    if !on_stdin {
        return;
    }
    *stdin = match (r.op, &r.target) {
        (RedirOp::In | RedirOp::InOut, RedirTarget::Path(v)) => StdinSource::File(v.clone()),
        (RedirOp::In, RedirTarget::ProcessSubstitution) => StdinSource::FromCommand,
        (RedirOp::DupIn, RedirTarget::Fd(t)) => StdinSource::Fd(t.clone()),
        (RedirOp::HereDoc, RedirTarget::HereDoc(h)) => match &h.body {
            Value::Literal(b) => StdinSource::Literal(b.clone()),
            Value::Undetermined(r) => StdinSource::Undetermined(r.clone()),
        },
        (RedirOp::HereString, RedirTarget::HereString(v)) => match v {
            Value::Literal(s) => StdinSource::Literal(format!("{s}\n")),
            Value::Undetermined(r) => StdinSource::Undetermined(r.clone()),
        },
        _ => StdinSource::Undetermined("stdin: redirected in an unmodelled way".into()),
    };
}

/// `set` arguments that change nothing this evaluator tracks.
pub(super) fn set_is_harmless(args: &[Word]) -> bool {
    const SAFE_O: &[&str] = &[
        "errexit",
        "nounset",
        "pipefail",
        "xtrace",
        "verbose",
        "noglob",
        "noclobber",
        "errtrace",
        "functrace",
        "notify",
        "monitor",
        "hashall",
        "allexport",
    ];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let s = match &a.value {
            Value::Literal(s) => s.as_str(),
            Value::Undetermined(_) => return false,
        };
        if s == "--" {
            return true;
        }
        if s == "-o" || s == "+o" {
            match it.next().map(|w| &w.value) {
                Some(Value::Literal(o)) if SAFE_O.contains(&o.as_str()) => continue,
                _ => return false,
            }
        }
        let flags = match s.strip_prefix('-').or_else(|| s.strip_prefix('+')) {
            Some(f) if !f.is_empty() => f,
            _ => return false,
        };
        if !flags.chars().all(|c| "euxvfnhbmCBEHPTa".contains(c)) {
            return false;
        }
    }
    true
}

/// What the next pipeline stage reads from a stage: `Literal` only for a
/// simple `echo` with literal operands and no option / backslash, or a
/// `printf` with one literal format holding only simple escapes, without
/// redirections; anything else is `FromCommand` (content not determined).
pub(super) fn piped_stdin(stage: Option<&SimpleCommand>) -> StdinSource {
    let sc = match stage {
        Some(sc) if sc.redirects.is_empty() => sc,
        _ => return StdinSource::FromCommand,
    };
    let mut lits = Vec::new();
    for w in &sc.argv {
        match &w.value {
            Value::Literal(s) => lits.push(s.as_str()),
            Value::Undetermined(_) => return StdinSource::FromCommand,
        }
    }
    let out = match lits.split_first() {
        Some((&"echo", args)) => {
            let opt = args.first().is_some_and(|a| {
                a.len() > 1 && a.starts_with('-') && a.chars().skip(1).all(|c| "neE".contains(c))
            });
            (!opt && !args.iter().any(|a| a.contains('\\')))
                .then(|| format!("{}\n", args.join(" ")))
        }
        Some((&"printf", [fmt])) if !fmt.starts_with('-') => printf_simple(fmt),
        _ => None,
    };
    out.map_or(StdinSource::FromCommand, StdinSource::Literal)
}

fn printf_simple(fmt: &str) -> Option<String> {
    let mut out = String::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => out.push(match it.next()? {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                'a' => '\x07',
                'b' => '\x08',
                'f' => '\x0c',
                'v' => '\x0b',
                '\\' => '\\',
                '"' => '"',
                '\'' => '\'',
                _ => return None,
            }),
            '%' => match it.next()? {
                '%' => out.push('%'),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    Some(out)
}
