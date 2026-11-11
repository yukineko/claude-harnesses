//! The Python judge for a QUOTED here-document body that `cat`/`tee` writes
//! to a literal `.py` file (user ruling 2026-11-08; the shape rule is
//! [`super::super::heredoc::python_file_here_document`]). Kept apart from
//! `interpreter.rs` for the file-size cap.
//!
//! Such a body is CODE (a later `python3 edit.py` runs it), so it is not data;
//! but the shell never parses it, so it is not shell lines either. Read line by
//! line as shell, a Python comment or docstring holding `` `git stash clear` ``
//! was denied as a command substitution, while
//! `subprocess.run(["git", "stash", "clear"])` followed by `python3 edit.py`
//! was allowed — the shell judge sees nothing harmful in Python syntax
//! (tests/quoted_heredoc_backtick_fp_20261108.rs).
//!
//! Here comments and string literals are inert; the calls that hand a command
//! to the OS, and the calls that remove or overwrite a path, are judged by the
//! shell rules applied to the equivalent command. What cannot be read
//! statically is an Ask. The Python reading never decides alone when the body
//! may also be read as shell (see [`PythonFileVerdict::also_shell`]).

use super::super::{is_assignment, normalized_command, quote_aware_words, split_segments};
use super::{code_building_marker, python_semantics as sem, shell_calls, Ctx, Decision};

/// The Python judge's answer for one here-document body.
pub(in crate::detect) struct PythonFileVerdict {
    /// The verdict on the body read as a Python program.
    pub(in crate::detect) verdict: Decision,
    /// The body may ALSO be read as shell, so the caller must judge the whole
    /// command, body included, with the shell rules too and combine Deny over
    /// Ask over Allow. True when a target is not a `.py` file, or when a later
    /// line references a target other than as a direct python script argument
    /// ([`later_lines_run_only_with_python`]).
    pub(in crate::detect) also_shell: bool,
}

/// The verdict on the Python program `body` written to `targets`, or `None`
/// when the body must stay judged as shell lines only, exactly as before the
/// ruling: its first line is a `#!` line that does not name python (a script
/// for another interpreter).
///
/// The Python reading, Deny over Ask over Allow, on the body with its aliases
/// resolved ([`sem::bindings`] / [`sem::rewrite`]: `import subprocess as sp`,
/// `from os import system as s`, `s = os.system` — a call through the alias is
/// judged as a call of the original; `pty.spawn` and
/// `asyncio.create_subprocess_*` are judged as the `subprocess.run` /
/// `os.system` they equal):
///   * a body that cannot be tokenised as Python (an unterminated string, an
///     unbalanced bracket) is an Ask: string boundaries are unknown, so no
///     call in it can be shown inert;
///   * every OS-facing call is judged from its literal arguments by
///     [`shell_calls::program_call_verdict_keeping_argv_asks`]
///     (`os.system("…")` and `subprocess.run([...])` as the command they run,
///     an argv's Ask kept), on the raw body and on its code view (comments
///     and plain strings blanked, f-string `{…}` code kept), so a call inside
///     an f-string replacement field is judged too;
///   * a call whose command is computed ([`shell_calls::computed_shell_call`]:
///     `os.system(cmd)`) is an Ask;
///   * a [`super::CODE_BUILDING`] marker in the code view (`exec(`, `eval(`,
///     `__import__`, …) is an Ask: the text is not the program that runs;
///   * a runner referenced without being called (`f(os.system)`, a module
///     like `os` passed as a value; [`sem::escaped_runner`]) is an Ask;
///   * a path removed or overwritten ([`sem::filesystem_verdict`]:
///     `shutil.rmtree`, `os.remove`, `Path.unlink`, `open(p, 'w')`, …) is
///     judged as the equivalent `rm` / `>` command; a path it cannot resolve
///     statically is an Ask. A computed path is Allow only when provably
///     repo-scoped (user ruling 2026-11-08, `python_semantics/values.rs`): a
///     `for` target over `glob` / `Path.rglob` / `os.walk` / `os.listdir`
///     rooted at a repo-relative literal, `sys.argv[N]` when every run of the
///     file passes a repo-relative literal there ([`script_argv`]), or
///     `self.attr` stored from a constructor argument;
///   * a relative, star-from-runner or non-standard-library import
///     ([`sem::import_verdict`]) is an Ask: that module's code is not here;
///   * in a program that imports a DB-API driver, destructive or computed SQL
///     ([`sem::sql_verdict`]) is an Ask.
///
/// Text in a comment or in a plain string literal is never judged, so a
/// docstring that names `git stash clear` is Allow. A file written here and run
/// by a LATER, separate tool call is judged on that call's own text, as for
/// data files.
pub(in crate::detect) fn python_file_body_verdict(
    body: &str,
    after: &str,
    targets: &[String],
    depth: usize,
    ctx: &Ctx<'_>,
) -> Option<PythonFileVerdict> {
    let first = body.lines().next().unwrap_or("");
    if first.starts_with("#!") && !first.contains("python") {
        return None;
    }
    let also_shell = targets.iter().any(|t| !t.ends_with(".py"))
        || !later_lines_run_only_with_python(after, targets);
    let argv = script_argv(after, targets);
    Some(PythonFileVerdict {
        verdict: python_program_verdict(body, argv.as_ref(), depth, ctx),
        also_shell,
    })
}

/// The arguments of every run of the written file in `after` ([`sem::Argv`]),
/// or `None` when they are not all known: `after` holds no run, or a segment
/// that is not a direct python run of a target ([`python_script_word`]) — a
/// `cd`, another command, a `-m` / `-c` run, anything that might run the file
/// with other arguments.
fn script_argv(after: &str, targets: &[String]) -> Option<sem::Argv> {
    let names: Vec<&str> = targets
        .iter()
        .map(|t| t.rsplit('/').next().unwrap_or(t))
        .filter(|n| !n.is_empty())
        .collect();
    let mut runs = Vec::new();
    for seg in split_segments(after) {
        let words = quote_aware_words(&seg);
        if words.is_empty() {
            continue;
        }
        let k = python_script_word(&words)?;
        if !names.iter().any(|n| words[k].contains(n)) {
            return None;
        }
        runs.push(words[k + 1..].iter().map(|w| sem::argv_word(w)).collect());
    }
    (!runs.is_empty()).then_some(runs)
}

/// The Python reading of [`python_file_body_verdict`].
fn python_program_verdict(
    body: &str,
    argv: Option<&sem::Argv>,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Decision {
    let shape = "the here-document writes a Python file";
    let refuse = || {
        Decision::ask(format!(
            "{shape}, and its program cannot be tokenised as Python (an unterminated string or an \
unbalanced bracket) — blastguard cannot tell its code from its strings, so it cannot vouch for it"
        ))
    };
    let Some(code) = code_view(body) else {
        return refuse();
    };
    let names = sem::bindings(&code);
    let raw = sem::rewrite(body, &names);
    let Some(code2) = code_view(&raw).map(|c| sem::rewrite(&c, &names)) else {
        return refuse();
    };
    let mut pending: Option<Decision> = None;
    let mut note = |d: Decision| -> Option<Decision> {
        match d {
            d @ Decision::Deny(_) => Some(d),
            a @ Decision::Ask(_) => {
                pending.get_or_insert(a);
                None
            }
            Decision::Allow => None,
        }
    };
    for text in [raw.as_str(), code2.as_str()] {
        let calls = shell_calls::program_call_verdict_keeping_argv_asks(text, depth, ctx);
        if let Some(d) = note(calls) {
            return d;
        }
        if let Some(d) = note(sem::filesystem_verdict(text, argv, depth, ctx)) {
            return d;
        }
    }
    if let Some(call) = shell_calls::computed_shell_call(&code2) {
        note(Decision::ask(format!(
            "{shape}, and that program runs a command it computes (`{call}` with a non-literal \
argument) — blastguard cannot see what would actually execute"
        )));
    }
    if let Some(marker) = code_building_marker(&code).or_else(|| code_building_marker(&code2)) {
        note(Decision::ask(format!(
            "{shape}, and that program builds or decodes the code it runs (`{marker}`) — the text \
is not the program that will execute, so blastguard cannot tell whether anything here is \
irreversible"
        )));
    }
    if let Some(runner) = sem::escaped_runner(&code2) {
        note(Decision::ask(format!(
            "{shape}, and that program uses `{runner}` other than by calling it directly (it is \
passed around or stored) — blastguard cannot follow what it will run"
        )));
    }
    if let Some(d) = sem::import_verdict(&names) {
        note(Decision::ask(format!("{shape}: {}", reason(&d))));
    }
    if let Some(d) = sem::sql_verdict(&raw, &names) {
        note(Decision::ask(format!("{shape}: {}", reason(&d))));
    }
    pending.unwrap_or(Decision::Allow)
}

/// Standard-library modules an inline Python program may import and still
/// have its string literals read as data by
/// [`python_strings_never_reach_a_shell`]: none of them runs a process or
/// loads code from a string (`os` is the exception that is admitted only
/// because every process-starting name of it is refused by
/// [`sem::references_runner`]). An allowlist: any other import — `ctypes`,
/// `importlib`, `multiprocessing`, a third-party module — keeps the program
/// out of that reading.
const DATA_ONLY_MODULES: &[&str] = &[
    "sys",
    "os",
    "json",
    "re",
    "glob",
    "fnmatch",
    "pathlib",
    "collections",
    "itertools",
    "functools",
    "datetime",
    "time",
    "math",
    "textwrap",
    "string",
    "csv",
    "hashlib",
    "difflib",
    "statistics",
    "typing",
    "dataclasses",
    "enum",
    "unicodedata",
    "pprint",
    "tomllib",
    "struct",
    "operator",
    "heapq",
    "bisect",
];

/// True ONLY when the inline Python `program` (the text after `python3 -c`)
/// verifiably never hands one of its strings to a shell or a process, so a
/// shell `>` spelled inside one of them is data, not a redirect (#1626, user
/// ruling 2026-11-07). Every condition is read with the Python tokenizer, not
/// guessed from the shell spelling:
///   * the program tokenises ([`code_view`]): string and comment boundaries
///     are known. A program that does not tokenise answers `false`;
///   * no [`super::CODE_BUILDING`] marker in the raw text or in the code view
///     (`exec(`, `eval(`, `getattr(`, `__import__`, …);
///   * no relative import and no star import from a runner module, and every
///     imported module is in [`DATA_ONLY_MODULES`];
///   * with aliases resolved ([`sem::bindings`] / [`sem::rewrite`]), the code
///     references no process-starting name ([`sem::references_runner`]).
///
/// The caller additionally requires that no layer-0 payload carries a
/// shell-string or dynamic-execution marker anywhere (strings included).
pub(super) fn python_strings_never_reach_a_shell(program: &str) -> bool {
    let Some(code) = code_view(program) else {
        return false;
    };
    if code_building_marker(program).is_some() || code_building_marker(&code).is_some() {
        return false;
    }
    let names = sem::bindings(&code);
    if names.relative
        || names.star_runner.is_some()
        || names
            .imported
            .iter()
            .any(|m| !DATA_ONLY_MODULES.contains(&m.as_str()))
    {
        return false;
    }
    sem::references_runner(&sem::rewrite(&code, &names)).is_none()
}

fn reason(d: &Decision) -> &str {
    match d {
        Decision::Deny(r) | Decision::Ask(r) => r,
        Decision::Allow => "",
    }
}

/// True when every later reference to a target in `after` (the command lines
/// after the closing delimiter) is the script argument of a python
/// interpreter ([`python_script_word`]: `python3 edit.py`, `env python3
/// edit.py`), or there is none. This is an allowlist: any other reference —
/// `sh edit.py`, `./edit.py`, `find … -name edit.py -exec`, `xargs`, `cat
/// edit.py | sh`, an unknown command — answers false, and the body is then
/// judged as shell too. A reference is a word holding a target's file name, or
/// a word with a glob character (it may name the file).
fn later_lines_run_only_with_python(after: &str, targets: &[String]) -> bool {
    let names: Vec<&str> = targets
        .iter()
        .map(|t| t.rsplit('/').next().unwrap_or(t))
        .filter(|n| !n.is_empty())
        .collect();
    split_segments(after).iter().all(|seg| {
        let words = quote_aware_words(seg);
        let refs: Vec<usize> = words
            .iter()
            .enumerate()
            .filter(|(_, w)| names.iter().any(|n| w.contains(n)) || w.contains(['*', '?', '[']))
            .map(|(k, _)| k)
            .collect();
        refs.is_empty() || (refs.len() == 1 && python_script_word(&words) == Some(refs[0]))
    })
}

/// The index of the script word of a segment that runs a python interpreter
/// on a script: leading `NAME=value` assignments, optionally `env` with only
/// assignments after it, then `python`, `python3` or `python3.N` (any
/// directory), then only flag-only options (`-u`, `-B`, …; never `-c`, `-m`,
/// `-W`, `-X` or `-`), then the script word. `None` for every other shape.
fn python_script_word(words: &[String]) -> Option<usize> {
    let mut k = 0;
    while words.get(k).is_some_and(|w| is_assignment(w)) {
        k += 1;
    }
    if words.get(k).map(|w| normalized_command(w)).as_deref() == Some("env") {
        k += 1;
        while words.get(k).is_some_and(|w| is_assignment(w)) {
            k += 1;
        }
    }
    let head = normalized_command(words.get(k)?);
    let version = head.strip_prefix("python")?;
    let versioned = version.is_empty()
        || version == "3"
        || version
            .strip_prefix("3.")
            .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()));
    if !versioned {
        return None;
    }
    k += 1;
    while let Some(w) = words.get(k) {
        let Some(flags) = w.strip_prefix('-') else {
            break;
        };
        if flags.is_empty() || !flags.chars().all(|c| "bBdEiIOPqsSuvR".contains(c)) {
            return None;
        }
        k += 1;
    }
    let script = words.get(k)?;
    (!script.starts_with('-')).then_some(k)
}

/// `src` with `#` comments removed and the contents of plain string literals
/// replaced by spaces (newlines kept; quotes and prefixes kept), followed by
/// the code of every f-string replacement field, one per line. `None` when
/// `src` does not tokenise: a string not closed (a one-quote string at its
/// newline, any string at the end of the text), a bracket closed that was
/// never opened, or a bracket left open.
fn code_view(src: &str) -> Option<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut fields = String::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '\'' || c == '"' {
            let prefix: String = out
                .chars()
                .rev()
                .take_while(|p| p.is_ascii_alphanumeric() || *p == '_')
                .collect();
            let fstring = prefix.len() <= 2 && prefix.chars().any(|p| p == 'f' || p == 'F');
            let triple = chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c);
            let q = if triple { 3 } else { 1 };
            out.extend(std::iter::repeat_n(c, q));
            i += q;
            let mut content = String::new();
            let mut closed = false;
            while i < chars.len() {
                let d = chars[i];
                if d == '\\' {
                    let e = *chars.get(i + 1)?;
                    content.push(d);
                    content.push(e);
                    out.push(' ');
                    out.push(if e == '\n' { '\n' } else { ' ' });
                    i += 2;
                    continue;
                }
                if d == '\n' && !triple {
                    return None;
                }
                if d == c
                    && (!triple || (chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c)))
                {
                    out.extend(std::iter::repeat_n(c, q));
                    i += q;
                    closed = true;
                    break;
                }
                content.push(d);
                out.push(if d == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            if !closed {
                return None;
            }
            if fstring {
                for field in fstring_fields(&content) {
                    fields.push('\n');
                    fields.push_str(&field);
                }
            }
            continue;
        }
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.checked_sub(1)?,
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    if depth != 0 {
        return None;
    }
    out.push_str(&fields);
    Some(out)
}

/// The replacement-field code of an f-string body: the text of each top-level
/// `{…}` (`{{` / `}}` are literal braces). A field left open runs to the end.
fn fstring_fields(content: &str) -> Vec<String> {
    let chars: Vec<char> = content.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' && chars.get(i + 1) == Some(&'{') {
            i += 2;
            continue;
        }
        if chars[i] != '{' {
            i += 1;
            continue;
        }
        let mut depth = 1usize;
        let mut field = String::new();
        i += 1;
        while i < chars.len() {
            match chars[i] {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            field.push(chars[i]);
            i += 1;
        }
        out.push(field);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{code_view, fstring_fields};

    #[test]
    fn code_view_blanks_strings_and_comments_and_keeps_fstring_fields() {
        let v = code_view("s = 'a `x` b'  # c `y`\nt = f\"{os.system(c)}\"\n").unwrap();
        assert!(!v.contains('`'), "{v}");
        assert!(v.contains("os.system(c)"), "{v}");
        assert_eq!(
            fstring_fields("a {{b}} {c:>3} {d['k']}"),
            vec!["c:>3", "d['k']"]
        );
    }

    #[test]
    fn code_view_refuses_what_does_not_tokenise() {
        assert_eq!(code_view("s = 'abc\nx = 1\n"), None);
        assert_eq!(code_view("s = \"\"\"abc\n"), None);
        assert_eq!(code_view("f(x\n"), None);
        assert_eq!(code_view("x)\n"), None);
        assert!(code_view("\"\"\"\nmulti\nline\n\"\"\"\n").is_some());
    }
}
