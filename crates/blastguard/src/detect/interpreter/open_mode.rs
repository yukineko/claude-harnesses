//! Structural judgement of the MODE an `open(...)`-family call is made with
//! (backlog 6cac93af, CRITICAL).
//!
//! Before this module the write table spelled only the `'w'`-family mode
//! operands (`,'w')`, `, 'wb')`, …), so `open('/etc/hosts','a')`, `'a+'`,
//! `'ab'`, `'r+'`, `'x'` and `mode='a'` never turned on the write-target probe
//! and a system-directory append reached Allow. Enumerating more spellings is
//! the v0.2.57 move that fails again at the next spelling, so the mode is now
//! PARSED out of the call instead (user ruling 2026-11-13):
//!
//! - a literal mode string is write-capable iff it contains any of `w`, `a`,
//!   `x`, `+` (any order, any `b`/`t`/`U` modifiers) — or `>`, which is how
//!   Perl's three-argument `open(FH, '>>', $p)` spells a write and is never a
//!   valid Python/Node read mode;
//! - a mode that cannot be determined statically — a variable, an
//!   expression, an f-string, a `*args`/`**kwargs` splat, a call whose
//!   argument list does not close — is write-capable (CLAUDE.md §3: cannot
//!   determine resolves to the restrictive side);
//! - an integer in a mode/flags position is read as `os.open` flags (the
//!   provenance of a bare `open` is not proven, `from os import open` makes
//!   `open(p, 1)` a write): `0` is read-only, any other integer is a write;
//! - an absent mode is the default `'r'` and is NOT write-capable, so a plain
//!   read (`open(p)`, `open(p,'rb')`, `open(p, mode='rt')`) adds nothing.
//!
//! A CALL is a recognised name, then optional whitespace (newlines
//! included), then `(` — `open ('/etc/hosts','a')` and `open\n(…)` are the
//! same call as `open(…)` (backlog 6cac93af rework, RED 123e5d91).
//!
//! PROVENANCE. Any occurrence of `open` / `fdopen` / `opensync` as a whole
//! identifier that is NOT followed by `(` — `from io import open as o`,
//! `from os import open`, `o = open`, `partial(open, …)`, `'open'` in a
//! `getattr` — means the program can reach an open through a name this module
//! does not follow. The whole program is then undetermined, i.e.
//! write-capable, rather than judged call by call.
//!
//! Which operand is the mode depends on the call shape, all case-folded:
//!
//! - bare `open(` / `opensync(` / `fdopen(` (and `.open(` on a receiver whose
//!   first argument is a path or descriptor: `io`, `codecs`, `gzip`, `bz2`,
//!   `lzma`, `tarfile`, `fs`, `promises`, Ruby `file`, `image`, `webbrowser`,
//!   `builtins`, and any `fdopen(`), and the mode-taking FILE CLASSES
//!   (`FileIO`, `ZipFile`, `PyZipFile`, `GzipFile`, `TarFile`, `BZ2File`,
//!   `LZMAFile`) — the SECOND positional operand, or `mode=` / `flags=`;
//! - `os.open(` / `posix.open(` — the second positional operand or `flags=`
//!   is a flag expression; it is read-only iff every `|`-joined term is a
//!   read-only flag (`O_RDONLY`, `O_CLOEXEC`, `O_NONBLOCK`, …) or the integer
//!   `0`. Anything else (`O_WRONLY`, `O_APPEND`, `O_CREAT`, a non-zero
//!   integer, a variable, a missing operand) is write-capable;
//! - `shelve.open(` / `dbm.open(` (and `dbm.gnu`/`ndbm`/`dumb`) — the second
//!   operand or `flag=`: only the literal `'r'` is read-only; anything else is
//!   write-capable, and an absent flag is `shelve`'s default `'c'` (a write)
//!   or `dbm`'s default `'r'` (a read);
//! - `.open(` on any OTHER receiver is read as `pathlib.Path.open(mode, …)`,
//!   whose FIRST operand is the mode: a literal made only of mode letters
//!   (`rwaxbtu+`) is judged by the rule above, any other literal (a member
//!   name such as `zf.open('a.txt')`) is not a mode, and a non-literal first
//!   operand is undetermined → write-capable. The second operand is judged
//!   too (`zipfile`'s `zf.open(name, 'w')`; it cannot be told from a mode).
//!
//! What this does NOT decide: which literal the call writes (the caller's
//! recoverability / system-path probes do that), and writes not spelled as
//! one of these calls (`Path.write_text`, Node `appendFileSync`, Perl
//! two-argument `open(FH, ">$p")`, `open my $fh, '>>', $p` without
//! parentheses). Those are not made write-capable here. The identifier scan
//! does not skip string literals or comments, so the word `open` in prose
//! also makes a program undetermined (restrictive side).

/// Label reported as the write call when a literal mode is write-capable.
pub(super) const WRITE_MODE_LABEL: &str = "open() in a write-capable mode";
/// Label reported when the mode/flags could not be determined statically.
pub(super) const UNDETERMINED_MODE_LABEL: &str =
    "open() whose mode cannot be determined statically";

/// Receivers whose `.open(` takes a path/descriptor first and the mode second.
const PATH_FIRST_RECEIVERS: &[&str] = &[
    "io",
    "codecs",
    "gzip",
    "bz2",
    "lzma",
    "tarfile",
    "fs",
    "file",
    "image",
    "webbrowser",
    "builtins",
    "promises",
];

/// Identifiers that name an open function. A non-call occurrence of one of
/// these is an alias/import whose calls are not followed: undetermined.
const OPEN_NAMES: &[&str] = &["open", "fdopen", "opensync"];

/// File classes constructed as `Class(path, mode)` (case-folded).
const MODE_CLASSES: &[&str] = &[
    "fileio",
    "zipfile",
    "pyzipfile",
    "gzipfile",
    "tarfile",
    "bz2file",
    "lzmafile",
];

/// Receivers whose `.open(path, flag)` is a dbm-style flag (`'r'` read-only).
const DBM_RECEIVERS: &[&str] = &["dbm", "gnu", "ndbm", "dumb"];

/// `os.open` flags that do not make the descriptor writable.
const READ_ONLY_FLAGS: &[&str] = &[
    "o_rdonly",
    "o_cloexec",
    "o_nonblock",
    "o_ndelay",
    "o_nofollow",
    "o_directory",
    "o_noctty",
    "o_binary",
    "o_text",
    "o_noinherit",
    "o_sequential",
    "o_random",
    "o_noatime",
    "o_path",
    "o_shlock",
    "o_exlock",
    "o_evtonly",
    "o_symlink",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Shape {
    /// Mode is the second positional operand (`open(path, mode)`).
    PathFirst,
    /// Flags are the second positional operand (`os.open(path, flags)`).
    OsFlags,
    /// Mode is the first positional operand (`pathlib.Path.open(mode)`).
    ModeFirst,
    /// dbm-style flag second (`dbm.open(p, 'c')`); `default_write` is what an
    /// absent flag means (`shelve` defaults to `'c'`).
    DbmFlag { default_write: bool },
}

/// One argument of a parsed call.
#[derive(Debug)]
enum Arg<'a> {
    Splat,
    Keyword(&'a str, Operand<'a>),
    Positional(Operand<'a>),
}

#[derive(Debug, PartialEq, Eq)]
enum Operand<'a> {
    /// A plain string literal's content.
    Str(&'a str),
    /// A plain integer literal.
    Int(&'a str),
    /// Anything else: a name, an expression, an f-string with fields. Carries
    /// its text so an `os.open` flag expression can still be read.
    Other(&'a str),
}

/// The strongest write-capable `open`-family call in `lowered` (already
/// ASCII-lowercased), if any. A determined write-capable mode wins over an
/// undetermined one only in the label; both answer `Some`.
pub(super) fn write_capable_open(lowered: &str) -> Option<&'static str> {
    let bytes = lowered.as_bytes();
    let mut undetermined = false;
    let mut i = 0usize;
    while i < bytes.len() {
        if !is_ident(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_ident(bytes[i]) {
            i += 1;
        }
        let name = &lowered[start..i];
        let is_open = OPEN_NAMES.contains(&name);
        if !is_open && !MODE_CLASSES.contains(&name) {
            continue;
        }
        // A call is the name, optional whitespace (newlines included), `(`.
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if bytes.get(j) != Some(&b'(') {
            // `from io import open as o`, `o = open`, `getattr(b, 'open')`:
            // an open reachable through a name not followed here.
            if is_open {
                undetermined = true;
            }
            continue;
        }
        let shape = call_shape(lowered, bytes, start, name);
        match judge_call(&lowered[j + 1..], shape) {
            Judged::Read => {}
            Judged::Write => return Some(WRITE_MODE_LABEL),
            Judged::Undetermined => undetermined = true,
        }
    }
    undetermined.then_some(UNDETERMINED_MODE_LABEL)
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Classify the call of `name` whose identifier starts at byte `start`.
fn call_shape(lowered: &str, bytes: &[u8], start: usize, name: &str) -> Shape {
    if MODE_CLASSES.contains(&name) || name != "open" {
        // File classes, `fdopen(fd, mode)`, `openSync(path, flags)`.
        return Shape::PathFirst;
    }
    // Receiver: the identifier before a `.` (whitespace around it skipped).
    let mut k = start;
    while k > 0 && bytes[k - 1].is_ascii_whitespace() {
        k -= 1;
    }
    if k == 0 || bytes[k - 1] != b'.' {
        return Shape::PathFirst;
    }
    let mut dot = k - 1;
    while dot > 0 && bytes[dot - 1].is_ascii_whitespace() {
        dot -= 1;
    }
    let mut r = dot;
    while r > 0 && is_ident(bytes[r - 1]) {
        r -= 1;
    }
    let receiver = &lowered[r..dot];
    if receiver == "os" || receiver == "posix" {
        Shape::OsFlags
    } else if receiver == "shelve" {
        Shape::DbmFlag {
            default_write: true,
        }
    } else if DBM_RECEIVERS.contains(&receiver) {
        Shape::DbmFlag {
            default_write: false,
        }
    } else if PATH_FIRST_RECEIVERS.contains(&receiver) {
        Shape::PathFirst
    } else {
        Shape::ModeFirst
    }
}

enum Judged {
    Read,
    Write,
    Undetermined,
}

fn judge_call(args_text: &str, shape: Shape) -> Judged {
    let Some(args) = split_args(args_text) else {
        return Judged::Undetermined;
    };
    let mut verdicts = Vec::new();
    let mut positional = 0usize;
    let mut flag_given = false;
    for arg in &args {
        match arg {
            Arg::Splat => verdicts.push(Judged::Undetermined),
            Arg::Keyword(name, op) => {
                if let Shape::DbmFlag { .. } = shape {
                    if *name == "flag" {
                        flag_given = true;
                        verdicts.push(judge_dbm_flag(op));
                    }
                } else if *name == "mode" || *name == "flags" {
                    verdicts.push(judge_operand(op, shape, name == &"flags"));
                }
            }
            Arg::Positional(op) => {
                match (shape, positional) {
                    (Shape::PathFirst, 1) => verdicts.push(judge_operand(op, shape, false)),
                    (Shape::OsFlags, 1) => verdicts.push(judge_operand(op, shape, true)),
                    (Shape::DbmFlag { .. }, 1) => {
                        flag_given = true;
                        verdicts.push(judge_dbm_flag(op));
                    }
                    (Shape::ModeFirst, 0) => match op {
                        Operand::Str(s) if !is_mode_alphabet(s) => {}
                        _ => verdicts.push(judge_operand(op, shape, false)),
                    },
                    (Shape::ModeFirst, 1) => verdicts.push(judge_operand(op, shape, false)),
                    _ => {}
                }
                positional += 1;
            }
        }
    }
    if let Shape::DbmFlag {
        default_write: true,
    } = shape
    {
        if !flag_given {
            verdicts.push(Judged::Write);
        }
    }
    // `os.open(path)` with no flags at all cannot be read as read-only.
    if shape == Shape::OsFlags
        && positional < 2
        && !args
            .iter()
            .any(|a| matches!(a, Arg::Keyword("flags", _) | Arg::Splat))
    {
        verdicts.push(Judged::Undetermined);
    }
    if verdicts.iter().any(|v| matches!(v, Judged::Write)) {
        Judged::Write
    } else if verdicts.iter().any(|v| matches!(v, Judged::Undetermined)) {
        Judged::Undetermined
    } else {
        Judged::Read
    }
}

fn is_mode_alphabet(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b"rwaxbtu+".contains(&b))
}

fn judge_operand(op: &Operand<'_>, shape: Shape, flags: bool) -> Judged {
    if flags || shape == Shape::OsFlags {
        return match op {
            Operand::Int(n) => judge_int_flags(n),
            Operand::Str(s) => judge_mode_str(s),
            Operand::Other(text) => judge_flags_expr(text),
        };
    }
    match op {
        Operand::Str(s) => judge_mode_str(s),
        // Builtin `open` rejects an integer mode, but the provenance of the
        // name is not proven (`from os import open`), so the integer is read
        // as `os.open` flags.
        Operand::Int(n) => judge_int_flags(n),
        Operand::Other(_) => Judged::Undetermined,
    }
}

/// An integer as `os.open` flags: `0` (`O_RDONLY`) is read-only, any other
/// value can carry a write bit.
fn judge_int_flags(n: &str) -> Judged {
    if n.bytes().all(|b| b == b'0') {
        Judged::Read
    } else {
        Judged::Write
    }
}

/// A dbm/shelve flag: only the literal `'r'` is read-only.
fn judge_dbm_flag(op: &Operand<'_>) -> Judged {
    match op {
        Operand::Str(s) if *s == "r" => Judged::Read,
        Operand::Str(_) => Judged::Write,
        _ => Judged::Undetermined,
    }
}

fn judge_mode_str(s: &str) -> Judged {
    if s.bytes().any(|b| b"wax+>".contains(&b)) {
        Judged::Write
    } else {
        Judged::Read
    }
}

/// Judge an `os.open` flag expression that is not a single literal.
fn judge_flags_expr(text: &str) -> Judged {
    let all_read_only = text.split('|').all(|term| {
        let t = term.trim();
        let t = t.rsplit('.').next().unwrap_or(t);
        t == "0" || READ_ONLY_FLAGS.contains(&t)
    });
    if all_read_only {
        Judged::Read
    } else {
        Judged::Undetermined
    }
}

/// Split the argument list starting just after `(`. `None` when the list does
/// not close or a quote does not terminate (undetermined).
fn split_args(text: &str) -> Option<Vec<Arg<'_>>> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    let mut seg_start = 0usize;
    let mut segments = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'\'' | b'"' | b'`' => {
                i = skip_quoted(bytes, i)?;
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    if b != b')' {
                        return None;
                    }
                    segments.push(&text[seg_start..i]);
                    return Some(
                        segments
                            .into_iter()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(classify_arg)
                            .collect(),
                    );
                }
                depth -= 1;
            }
            b',' if depth == 0 => {
                segments.push(&text[seg_start..i]);
                seg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Index just past the quote that closes the one opening at `open`.
fn skip_quoted(bytes: &[u8], open: usize) -> Option<usize> {
    let q = bytes[open];
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b if b == q => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

fn classify_arg(seg: &str) -> Arg<'_> {
    if seg.starts_with('*') {
        return Arg::Splat;
    }
    if let Some((name, value)) = keyword_split(seg) {
        return Arg::Keyword(name, classify_operand(value));
    }
    Arg::Positional(classify_operand(seg))
}

/// `name = value` (not `==`), Python keyword or Perl/Ruby-free form.
fn keyword_split(seg: &str) -> Option<(&str, &str)> {
    let bytes = seg.as_bytes();
    let mut end = 0usize;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    if end == 0 || bytes[0].is_ascii_digit() {
        return None;
    }
    let rest = seg[end..].trim_start();
    let value = rest.strip_prefix('=')?;
    if value.starts_with('=') {
        return None;
    }
    Some((&seg[..end], value.trim()))
}

fn classify_operand(seg: &str) -> Operand<'_> {
    let s = seg.trim();
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return Operand::Int(s);
    }
    // Optional Python string prefix without `f` (an f-string with fields is
    // an expression).
    let body = s.trim_start_matches(['r', 'b', 'u']);
    let bytes = body.as_bytes();
    if bytes.len() >= 2 {
        let q = bytes[0];
        if (q == b'\'' || q == b'"')
            && bytes[bytes.len() - 1] == q
            && skip_quoted(bytes, 0) == Some(bytes.len())
        {
            let inner = &body[1..body.len() - 1];
            // Triple-quoted or escaped content is left undetermined.
            if inner.contains('\\') || inner.starts_with(q as char) {
                return Operand::Other(s);
            }
            return Operand::Str(inner);
        }
    }
    Operand::Other(s)
}
