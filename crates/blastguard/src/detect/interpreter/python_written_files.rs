//! The file writes of a Python program, read from its tokens, for the
//! write-then-run check of `echo_script_file` (backlog 245a4c5f): a
//! `python3 - <<'PY'` program that writes `t_test.sh` followed in the same
//! command by `sh t_test.sh` was Allow, while the `cat > t_test.sh <<'EOF'`
//! twin was Deny.
//!
//! A program that does not tokenise as Python, or that holds a
//! dynamic-execution or process marker
//! (`inert_literal::has_dynamic_code_execution`: `exec`, `eval`,
//! `subprocess`, `os.system`, …), is reported as one write whose path and
//! text are both unknown. Otherwise its tokens are read.
//!
//! Two shapes are reported with their written text, when the path and the
//! text are each one string literal whose value the lexer decodes exactly
//! (not an f-string, no `\N{…}`, not a raw literal holding a backslash):
//!   * `open(P, M).write(C)`, the call named `open`, `io.open`, `codecs.open`
//!     or `builtins.open` (not after a `.`), with exactly the two operands
//!     `P`, `M` (both such literals) and `.write(C)` directly on that call;
//!     a mode `M` holding `a` appends, a mode holding `w` or `x` and no `a`
//!     replaces;
//!   * `Path(P).write_text(C)` / `.write_bytes(C)`, the receiver spelled
//!     `Path`, `pathlib.Path`, `PosixPath` or `pathlib.PosixPath`; it
//!     replaces.
//!
//! Other writes it recognises are reported with an unreadable text
//! (`content` is `Err`), which the caller turns into an Ask when the file
//! may be run:
//!   * a token whose last dotted segment is `open` or `fdopen`, when
//!     `open_mode::write_capable_open` finds a write-capable or undetermined
//!     mode anywhere in the program, unless it is one of the reads below or
//!     the `open` shape above. Its path is the first operand when the call is
//!     named `open`, `io.open`, `codecs.open`, `builtins.open` or `os.open`
//!     (not after a `.`) and that operand is an exact literal followed by `,`
//!     or `)`; otherwise the path is unknown. A read adds nothing: such a call
//!     with a literal path and no other operand (`open(P)`), or with the
//!     operands `P`, `M` where `M` is a literal made only of `r`, `b`, `t`,
//!     `U`. A read spelled any other way (`os.open(P, os.O_RDONLY)`,
//!     `mode='r'`, a variable mode) is reported, as is `r+`;
//!   * a `write_text` / `write_bytes` token other than the shape above: path
//!     kept when the receiver is that shape, unknown otherwise;
//!   * a token named `copy`, `copy2`, `copyfile`, `copytree` or `move` (bare
//!     and not after a `.`, or `shutil.`-prefixed), `renames`, `symlink` or
//!     `link` (bare and not after a `.`, or `os.`-prefixed), `os.replace`, or
//!     a token whose last segment is `rename`, `symlink_to`, `hardlink_to` or
//!     `urlretrieve`: path unknown. Being a token is enough; whether it is
//!     called is not checked.
//!
//! NOT read (gaps): writes through any call not listed above (an aliased
//! import such as `from shutil import copy as c`, `os.write` when no
//! write-capable `open` is in the program, `tarfile` / `zipfile`
//! extraction, pathlib `.replace`, `print(…, file=…)` on a handle that does
//! not come from an `open` token).

use super::inert_literal::has_dynamic_code_execution;
use super::open_mode::write_capable_open;
use super::python_file::{python_newlines, python_program_tokenises};
use super::python_semantics::{lex_marking_inexact, Tok};

/// One file write of a Python program.
pub(in crate::detect) struct PythonWrite {
    /// The path's literal value; `None` when it is not one exact literal.
    pub(in crate::detect) path: Option<String>,
    /// The text written, or why it cannot be read.
    pub(in crate::detect) content: Result<String, String>,
    /// Whether the write appends to the file rather than replacing it.
    pub(in crate::detect) append: bool,
    /// The call, as named in a reason.
    pub(in crate::detect) call: &'static str,
}

impl PythonWrite {
    fn unread(path: Option<String>, call: &'static str, why: &str) -> PythonWrite {
        PythonWrite {
            path,
            content: Err(why.to_string()),
            append: false,
            call,
        }
    }
}

const NOT_ONE_LITERAL: &str =
    "the Python program writes it with a call or a text that is not one plain string literal";

/// The writes of `program`, in source order; see the module documentation.
pub(in crate::detect) fn python_program_writes(program: &str) -> Vec<PythonWrite> {
    let program = python_newlines(program);
    if !python_program_tokenises(&program) {
        return vec![PythonWrite::unread(
            None,
            "the Python program",
            "the Python program does not tokenise, so blastguard does not read what it writes",
        )];
    }
    if has_dynamic_code_execution(&program) {
        return vec![PythonWrite::unread(
            None,
            "the Python program",
            "the Python program holds a dynamic-execution or process marker (`exec`, `eval`, \
`subprocess`, `os.system`, …), so blastguard does not read what it writes",
        )];
    }
    let (toks, inexact) = lex_marking_inexact(&program);
    let exact = |i: usize| -> Option<&str> {
        match toks.get(i) {
            Some(Tok::Str { text, fstring }) if !*fstring && !inexact.contains(&i) => Some(text),
            _ => None,
        }
    };
    let punct = |i: usize, c: char| toks.get(i) == Some(&Tok::Punct(c));
    let opens_write = write_capable_open(&program, true).is_some();
    let mut out = Vec::new();
    for (i, tok) in toks.iter().enumerate() {
        let Tok::Name { text, .. } = tok else {
            continue;
        };
        let last = text.rsplit('.').next().unwrap_or(text);
        let after_dot = i > 0 && punct(i - 1, '.');
        match last {
            "open" | "fdopen" if opens_write => {
                let bare = !after_dot
                    && matches!(
                        text.as_str(),
                        "open" | "io.open" | "codecs.open" | "builtins.open" | "os.open"
                    );
                let path = exact(i + 2).filter(|_| {
                    bare && punct(i + 1, '(') && (punct(i + 3, ',') || punct(i + 3, ')'))
                });
                let mode = exact(i + 4).filter(|_| {
                    text != "os.open" && path.is_some() && punct(i + 3, ',') && punct(i + 5, ')')
                });
                // `open(P)`: the default mode `'r'`, a read.
                let plain_read = path.is_some() && punct(i + 3, ')');
                let read_mode = |m: &str| m.chars().all(|c| matches!(c, 'r' | 'b' | 't' | 'U'));
                match mode {
                    _ if plain_read => {}
                    Some(m) if read_mode(m) => {}
                    Some(m) if m.contains(['w', 'a', 'x']) => {
                        let written = (punct(i + 6, '.')
                            && matches!(toks.get(i + 7), Some(Tok::Name { text, .. }) if text == "write")
                            && punct(i + 8, '(')
                            && punct(i + 10, ')'))
                        .then(|| exact(i + 9))
                        .flatten();
                        out.push(PythonWrite {
                            path: path.map(str::to_string),
                            content: written
                                .map(str::to_string)
                                .ok_or_else(|| NOT_ONE_LITERAL.to_string()),
                            append: m.contains('a'),
                            call: "open",
                        });
                    }
                    // `r+` (an in-place update), any other literal mode, or
                    // no literal path and mode.
                    _ => out.push(PythonWrite::unread(
                        path.map(str::to_string),
                        "open",
                        NOT_ONE_LITERAL,
                    )),
                }
            }
            "write_text" | "write_bytes" => {
                let path = (after_dot
                    && i >= 5
                    && punct(i - 2, ')')
                    && punct(i - 4, '(')
                    && matches!(
                        &toks[i - 5],
                        Tok::Name { text, .. } if matches!(
                            text.as_str(),
                            "Path" | "pathlib.Path" | "PosixPath" | "pathlib.PosixPath"
                        )
                    ))
                .then(|| exact(i - 3))
                .flatten();
                let written = (path.is_some() && punct(i + 1, '(') && punct(i + 3, ')'))
                    .then(|| exact(i + 2))
                    .flatten();
                out.push(PythonWrite {
                    path: path.map(str::to_string),
                    content: written
                        .map(str::to_string)
                        .ok_or_else(|| NOT_ONE_LITERAL.to_string()),
                    append: false,
                    call: if last == "write_text" {
                        "write_text"
                    } else {
                        "write_bytes"
                    },
                });
            }
            _ => {
                let bare = text == last && !after_dot;
                let shutil = matches!(last, "copy" | "copy2" | "copyfile" | "copytree" | "move")
                    && (bare || text.strip_prefix("shutil.") == Some(last));
                let os = matches!(last, "rename" | "renames" | "symlink" | "link")
                    && (bare || text.strip_prefix("os.") == Some(last));
                let any = matches!(
                    last,
                    "rename" | "symlink_to" | "hardlink_to" | "urlretrieve"
                );
                if shutil || os || any || text == "os.replace" {
                    out.push(PythonWrite::unread(
                        None,
                        "a copy, move, rename or link call",
                        NOT_ONE_LITERAL,
                    ));
                }
            }
        }
    }
    out
}
