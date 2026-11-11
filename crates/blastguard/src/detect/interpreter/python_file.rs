//! The Python judge for a QUOTED here-document body that `cat`/`tee` writes
//! to a literal `.py` file (user ruling 2026-11-08; the shape rule is
//! [`super::super::heredoc::python_file_here_document`]), and the
//! Python-tokenised redirect view of a body `python3 -` runs as its program
//! ([`python_redirect_view`]). Kept apart from
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

use super::super::heredoc::HereDocumentParts;
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
///
/// Relative paths are placed where the program runs: every command the judge
/// derives from the program (`rm p`, `: > p`, an `os.system` line) is judged
/// with `cd <dir> && ` before it (`Ctx::cwd_prefix`), so the shell rules'
/// own `cd` tracking places it (tests/subagent_heredoc_fp_20261108.rs added
/// the `cd <wt>;` head; before this a `cd /etc && … os.remove('hosts')` body
/// was judged as `rm hosts` in the session's directory and allowed). Only ONE
/// absolute `cd` operand with no `.` / `..` component, `~` or glob is
/// followed; any other `cd` before the reader, and any directory change on a
/// later line ([`later_lines_change_dir`]), puts [`UNPLACED_CWD`] there
/// instead, a `cd` the shell rules cannot resolve, so a relative path is
/// judged as after `cd "$X"` (the restrictive side).
pub(in crate::detect) fn python_file_body_verdict(
    parts: &HereDocumentParts,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Option<PythonFileVerdict> {
    let (body, after, targets) = (&parts.body, &parts.after, &parts.targets);
    let first = body.lines().next().unwrap_or("");
    if first.starts_with("#!") && !first.contains("python") {
        return None;
    }
    let also_shell = targets.iter().any(|t| !t.ends_with(".py"))
        || !later_lines_run_only_with_python(after, targets);
    let argv = script_argv(after, targets);
    let placed = placed_ctx(&parts.cds_before, later_lines_change_dir(after), ctx);
    Some(PythonFileVerdict {
        verdict: python_program_verdict(
            "the here-document writes a Python file",
            body,
            argv.as_ref(),
            depth,
            placed.as_ref().unwrap_or(ctx),
        ),
        also_shell,
    })
}

/// `ctx` with [`Ctx::cwd_prefix`] set to where a here-document's Python
/// program runs: `None` (the session's directory) when no `cd` runs before
/// the reader; `cd <dir> && ` for ONE [`plain_absolute_dir`] operand; and
/// [`UNPLACED_CWD`] for any other `cd` list, or when `unplaced` (a later line
/// may change the directory a later run starts in).
fn placed_ctx<'a>(cds_before: &[String], unplaced: bool, ctx: &Ctx<'a>) -> Option<Ctx<'a>> {
    let prefix = match cds_before {
        _ if unplaced => Some(UNPLACED_CWD.to_string()),
        [] => None,
        [dir] if plain_absolute_dir(dir) => Some(format!("cd {dir} && ")),
        _ => Some(UNPLACED_CWD.to_string()),
    };
    prefix.map(|p| Ctx {
        cwd_prefix: Some(p),
        ..ctx.clone()
    })
}

/// The Python reading of a QUOTED here-document body a bare Python
/// interpreter runs as its program (`python3 - <<'EOF'`; the shape is
/// [`super::super::heredoc::python_stdin_here_document`]'s), for the
/// stdin-program audit of `payload::analyze_interpreter_stdin_exec`.
pub(in crate::detect) struct PythonStdinReading {
    /// [`python_file_body_verdict`]'s Python reading of the body, with its
    /// relative paths placed after the `cd` that runs before the reader
    /// (tests/heredoc_system_harm_holes_20261111.rs case A:
    /// `cd /etc && python3 - <<'EOF'` / `open('hosts','w')` is `: > hosts`
    /// in `/etc`).
    pub(in crate::detect) verdict: Decision,
    /// True ONLY when the parser shows no string of the program can reach a
    /// shell or a process: the body holds no [`super::SHELL_STRING_EXEC_MARKERS`]
    /// entry or dynamic-execution marker anywhere (strings included), and
    /// passes [`python_strings_never_reach_a_shell`]. The contents of its
    /// string literals are then data, not program text, so the caller's
    /// program-text scan (`interpreter_code_verdict`) does not descend into
    /// them as code (case G: a printed `"shutil.copy2('a','/etc/hosts')"`
    /// literal was denied as a write of `/etc/hosts`). The program text
    /// itself, literals as written included, is still scanned.
    pub(in crate::detect) strings_are_data: bool,
}

/// See [`PythonStdinReading`]. `None` when the body does not tokenise as
/// Python: string boundaries are unknown, so nothing changes and the caller
/// judges the raw body exactly as before.
pub(in crate::detect) fn python_stdin_body_reading(
    parts: &HereDocumentParts,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Option<PythonStdinReading> {
    let body = &parts.body;
    code_view(body)?;
    // The program runs before any later line, so only the `cd`s before the
    // reader place it.
    let placed = placed_ctx(&parts.cds_before, false, ctx);
    let verdict = python_program_verdict(
        "the here-document feeds a Python program on stdin",
        body,
        None,
        depth,
        placed.as_ref().unwrap_or(ctx),
    );
    Some(PythonStdinReading {
        verdict,
        strings_are_data: !super::runs_shell_strings(body)
            && !super::has_dynamic_code_execution(body)
            && python_strings_never_reach_a_shell(body),
    })
}

/// The `cd` the Python judge puts before its derived commands when the
/// directory the program runs in is not known: an unset variable, which the
/// shell rules' `cd` tracking cannot resolve.
const UNPLACED_CWD: &str = "cd \"$BLASTGUARD_UNPLACED_CWD\" && ";

/// True for an absolute directory spelled with no `..` / `.` component, `~`,
/// glob or expansion: the one `cd` operand [`python_file_body_verdict`] follows.
fn plain_absolute_dir(dir: &str) -> bool {
    dir.starts_with('/')
        && !dir.contains(['~', '*', '?', '[', '$', '`', '\\', '\'', '"'])
        && dir.split('/').all(|c| c != ".." && c != ".")
}

/// True when a word of `after` (the lines after a written file's closing
/// delimiter) may change the directory a later run of the file starts in:
/// `cd`, `pushd` or `popd` (also after a `(` / `{`), or a `-C` / `--chdir`
/// option (`env -C`, `git -C`). A closed, restrictive check: such a run's
/// relative paths are then not placed at all.
fn later_lines_change_dir(after: &str) -> bool {
    quote_aware_words(after).iter().any(|w| {
        let w = w.trim_start_matches(['(', '{']);
        matches!(w, "cd" | "pushd" | "popd") || w.starts_with("-C") || w.starts_with("--chdir")
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
    shape: &str,
    body: &str,
    argv: Option<&sem::Argv>,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Decision {
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

/// True when `program` tokenises as Python ([`code_view`] is `Some`): every
/// string is closed and every bracket balanced.
pub(super) fn python_program_tokenises(program: &str) -> bool {
    code_view(program).is_some()
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
        refs.is_empty()
            || (refs.len() == 1
                && (python_script_word(&words) == Some(refs[0])
                    || pytest_file_word(&words) == Some(refs[0])))
    })
}

/// The index of the file word of a segment that runs pytest on ONE file
/// through a python interpreter: [`python_script_word`]'s head (assignments,
/// `env`, `python3`, flag-only options), then `-m pytest`, then only the
/// option words `-q`/`-v`/`-x`/`-s` (combined, e.g. `-qx`) and
/// `--no-header`, and exactly one other word, the file. pytest imports that
/// file as a Python module, so its body is read as Python, never as shell.
/// `None` for every other shape (an option taking a value, two files, …).
fn pytest_file_word(words: &[String]) -> Option<usize> {
    let k = python_head_end(words)?;
    if words.get(k).map(String::as_str) != Some("-m")
        || words.get(k + 1).map(String::as_str) != Some("pytest")
    {
        return None;
    }
    // An fd duplication (`2>&1`) names no file; any other redirect word counts
    // as a file word, so the segment is refused.
    let fd_dup = |w: &str| {
        let rest = w.strip_prefix(|c: char| c.is_ascii_digit()).unwrap_or(w);
        rest.strip_prefix(">&")
            .is_some_and(|n| n.len() == 1 && n.chars().all(|c| c.is_ascii_digit()))
    };
    let option = |w: &str| {
        fd_dup(w)
            || w == "--no-header"
            || w.strip_prefix('-')
                .is_some_and(|f| !f.is_empty() && f.chars().all(|c| "qvxs".contains(c)))
    };
    let files: Vec<usize> = (k + 2..words.len())
        .filter(|&j| !option(&words[j]))
        .collect();
    match files.as_slice() {
        [file] if !words[*file].starts_with('-') => Some(*file),
        _ => None,
    }
}

/// The index of the script word of a segment that runs a python interpreter
/// on a script: leading `NAME=value` assignments, optionally `env` with only
/// assignments after it, then `python`, `python3` or `python3.N` (any
/// directory), then only flag-only options (`-u`, `-B`, …; never `-c`, `-m`,
/// `-W`, `-X` or `-`), then the script word. `None` for every other shape.
fn python_script_word(words: &[String]) -> Option<usize> {
    let k = python_head_end(words)?;
    let script = words.get(k)?;
    (!script.starts_with('-')).then_some(k)
}

/// The index just past the interpreter head of a python run: leading
/// `NAME=value` assignments, optionally `env` with only assignments after it,
/// `python` / `python3` / `python3.N` (any directory), then flag-only options
/// (`-u`, `-B`, …). Stops at the first word that is not such an option; an
/// option outside the flag-only set other than `-m` answers `None`.
fn python_head_end(words: &[String]) -> Option<usize> {
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
        if flags == "m" {
            break;
        }
        if flags.is_empty() || !flags.chars().all(|c| "bBdEiIOPqsSuvR".contains(c)) {
            return None;
        }
        k += 1;
    }
    Some(k)
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

/// `src` (a Python program) with the text of every string literal and `#`
/// comment, and the literal's quote delimiters, replaced by spaces, newlines
/// kept, prefixes and all code kept as is, EXCEPT a literal or comment for
/// whose text `keep` answers true: that text is kept, but with each `'`, `"`
/// and `\` in it replaced by a space. An f-string is a literal here too (its fields
/// are Python code, never shell). `None` when `src` does not tokenise
/// ([`code_view`]'s rule: an unclosed string, an unbalanced bracket), so the
/// caller keeps the raw body. Used only for the redirect view of a body that
/// `python3 -` runs ([`super::super::heredoc::python_stdin_redirect_view`]).
pub(in crate::detect) fn python_redirect_view(
    src: &str,
    keep: impl Fn(&str) -> bool,
) -> Option<String> {
    code_view(src)?;
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    // A kept text loses its quote and backslash characters (each becomes a
    // space): they mean nothing to the shell here, and an apostrophe left in
    // (`# don't > \`x\``) would open a shell quote that swallows the lines
    // after the body, a real `> /etc/hosts` included.
    let blank = |text: &[char], out: &mut String| {
        let kept = keep(&text.iter().collect::<String>());
        out.extend(text.iter().map(|&c| match c {
            '\n' => '\n',
            '\'' | '"' | '\\' => ' ',
            c if kept => c,
            _ => ' ',
        }));
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '#' {
            let start = i;
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            blank(&chars[start..i], &mut out);
            continue;
        }
        if c != '\'' && c != '"' {
            out.push(c);
            i += 1;
            continue;
        }
        let triple = chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c);
        let q = if triple { 3 } else { 1 };
        out.extend(std::iter::repeat_n(' ', q));
        i += q;
        let start = i;
        loop {
            // `code_view` accepted `src`, so every literal closes.
            let d = *chars.get(i)?;
            if d == '\\' {
                i += 2;
                continue;
            }
            if d == c && (!triple || (chars.get(i + 1) == Some(&c) && chars.get(i + 2) == Some(&c)))
            {
                break;
            }
            i += 1;
        }
        blank(&chars[start..i], &mut out);
        out.extend(std::iter::repeat_n(' ', q));
        i += q;
    }
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
