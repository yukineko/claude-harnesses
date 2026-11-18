//! What `echo` / `printf` write to a file is the file's content, and when
//! another part of the same command runs that file, the content is judged as
//! the shell script it is (tests/script_write_then_run_harm_20261111.rs H1-H3:
//! `echo 'rm -rf ~' > x.rs && sh x.rs` was Allow). The `cat`/`tee`
//! here-string twin is [`super::here_string_file`]; this module gives the same
//! reading to the literal arguments of `echo` and `printf`.
//!
//! The files a quoted `python3 - <<'PY'` here-document program writes are a
//! second source of written files (backlog 245a4c5f;
//! [`python_heredoc_files`], read by
//! `interpreter::python_written_files`). They are judged by the same rules
//! below, except where [`python_heredoc_files`] says otherwise. The writer
//! rules in the next paragraph are about `echo` / `printf` only.
//!
//! A writer is a pipe stage whose command (behind any exec-wrapper,
//! [`command_candidates`]) is `echo` or `printf`, writing its stdout to a file
//! through `>`, `>>`, `>|`, `&>`, `1>` (glued or not), or through a following
//! `| tee FILE…` / `| cat > FILE` stage. A writer may stand behind a reserved
//! word (`do echo …`, `then echo …`, `{ echo …`) or inside a subshell word or
//! a substitution the shell runs (`(echo … > x.sh)`, `$(echo … > x.sh)`)
//! (tests/echo_script_variants_20261111.rs F2 / F6). Writes to the same file
//! (leading `./` ignored) accumulate in command order: `>` replaces, `>>`
//! appends; the content after EVERY write is judged, so neither a later
//! overwrite (which may fail: an inherited `noclobber`, `set -C`, a `chmod`;
//! user ruling 2026-11-11 "Back to Deny", V16) nor a split across two
//! `printf`s hides a script that ran.
//!
//! `echo` output is followed through every shell's reading of it (user ruling
//! 2026-11-11 "Deny", V13; [`echo_content`]): bash (escapes only under `-e`),
//! zsh or bash `xpg_echo` (escapes unless `-E`), dash (only a first `-n` is an
//! option, escapes always). Every reading is judged, so the verdict is Deny
//! when any reading is, and Ask when none is but one cannot be reproduced.
//!
//! The file is judged only when some OTHER stage of the command may run it:
//!   * a stage whose command word names the file (`./x.sh`, `/p/x.sh`, a word
//!     behind an exec-wrapper);
//!   * a pipeline (one statement) that names the file anywhere and holds a
//!     shell, `.` or `source` word anywhere (`sh x.sh`, `bash -e x.sh`,
//!     `. x.sh`, `sh < x.sh`, `cat x.sh | sh`, `find . -exec sh {} \;`);
//!   * read as a Python program, for a pipeline holding a `python` /
//!     `python3` / `python3.N` word whose program operand — the first
//!     non-option operand after python's own options ([`python_program`]) —
//!     names the file (`python3 x.py`); or whose program is stdin (`-`, no
//!     operand), comes from `-m`, or cannot be read statically (an unknown
//!     option, a non-literal operand), when the pipeline names the file
//!     anywhere (`python3 < x.py`, `cat x.py | python3`). A file named only
//!     as a later operand (`python3 s.py x.txt`, `sys.argv` data) or beside
//!     `-c` is not read as the program (backlog dc27f669 / bf90fa9d). A
//!     non-literal written path keeps the old reading: any python word runs it;
//!   * the same, read as that interpreter's program, for a pipeline that
//!     names the file and holds another code-interpreter word
//!     (`is_code_interpreter`: `node x.js`, `perl x.pl`, …; F7);
//!   * any of these inside a subshell word (`(sh x.sh)`) or a substitution the
//!     shell runs (`$(sh x.sh)`, `<(…)`), searched in those bodies as in a
//!     statement of their own (V9).
//!
//! A word names the file when its literal value has the file's basename
//! (`sh /tmp/x.sh` names `x.sh`), or, when the word holds an expansion, when
//! its text contains that basename. A written path that is not a literal
//! (`> "$F"`) is run by any pipeline holding a shell, `.` or `source` word, or
//! a command word with `$` or a backtick (and by any holding a python or other
//! code-interpreter word, as that program). These over-approximate on purpose:
//! a false match only means the written text is read as a program.
//!
//! Judged means a shell run's content goes through every shell rule
//! (`analyze_shell_payload`); a Python run's through the Python judge of the
//! `.py` here-document twin (`python_written_program_verdict`, its full
//! verdict); another interpreter's through the program audit its inline-eval
//! flag gets (`interpreter_code_verdict`, as for `node -e '<content>'`, whose
//! destructive-call list includes `rmSync`); Deny over Ask over Allow.
//! Content blastguard cannot reproduce is an Ask, never Allow:
//!   * an argument with `$`, a backtick, a backslash, a glob or brace
//!     character, a bare operator character, or a `~` other than a leading
//!     `~` / `~/` (kept as `~`, which the shell rules read as home, the same
//!     path the writing shell would put there);
//!   * an `echo` reading that expands an escape other than `\\`, `\a`, `\b`,
//!     `\c`, `\f`, `\n`, `\r`, `\t`, `\v`;
//!   * more than [`MAX_READINGS`] readings after appends;
//!   * a `printf` format with any `%` directive other than `%%`, any escape
//!     other than `\n`, `\t`, `\\`, a format starting with `-`, or arguments
//!     after the format (format reuse is not modelled).

use super::words::basename;
use super::{
    analyze_shell_payload, command_candidates, executed_substitution_payloads,
    interpreter_code_verdict, is_code_interpreter, is_shell, normalized_command, quote_aware_words,
    scan_balanced, split_statements_into_pipe_stages, Ctx, Decision, PythonReader, Stop,
    VerdictAcc,
};

/// A file an `echo`/`printf` stage writes: its literal path (leading `./`
/// removed), or the raw word when the path is not a literal.
#[derive(Clone, PartialEq, Eq)]
enum Target {
    Literal(String),
    Opaque(String),
}

impl Target {
    fn from_word(word: &str) -> Target {
        match word_literal(word) {
            Some(lit) => Target::from_path(&lit, word),
            None => Target::Opaque(word.to_string()),
        }
    }

    /// The target whose path value is `lit`; `Opaque(shown)` when that value
    /// has no file name.
    fn from_path(lit: &str, shown: &str) -> Target {
        if lit.is_empty() {
            return Target::Opaque(shown.to_string());
        }
        let mut p = lit;
        while let Some(rest) = p.strip_prefix("./") {
            p = rest;
        }
        let p = p.trim_end_matches('/');
        if basename(p).is_empty() || matches!(basename(p), "." | "..") {
            Target::Opaque(shown.to_string())
        } else {
            Target::Literal(p.to_string())
        }
    }

    fn shown(&self) -> &str {
        match self {
            Target::Literal(s) | Target::Opaque(s) => s,
        }
    }
}

/// One output write: where, and whether it appends.
struct Write {
    target: Target,
    append: bool,
}

/// One text a writer may put on its stdout (one shell's reading of it), or
/// why that text cannot be reproduced.
type Reading = Result<String, String>;

/// The readings of one writer's stdout, or of a file's content after a write.
type Content = Vec<Reading>;

/// The most readings a file's content is followed through; past it the rest
/// are replaced by one unreproducible reading (an Ask).
const MAX_READINGS: usize = 16;

/// One file written in the command: every content it holds after a write.
struct WrittenFile {
    target: Target,
    snapshots: Vec<Content>,
    writer: String,
}

/// The verdict on the files `echo`/`printf` write in `cmd`, and the files the
/// program of a quoted Python here-document in `cmd` writes
/// ([`python_heredoc_files`]), that another stage of `cmd` may run; Allow
/// when there are none. See the module documentation.
pub(super) fn echoed_script_verdict(cmd: &str, depth: usize, ctx: &Ctx<'_>) -> Decision {
    let statements = split_statements_into_pipe_stages(cmd);
    let mut files: Vec<WrittenFile> = Vec::new();
    let mut writer_stages: Vec<(usize, usize)> = Vec::new();
    collect_writes(&statements, &mut files, &mut writer_stages);
    let python = python_heredoc_files(cmd);
    let echoed = files
        .iter()
        .map(|f| (f, &statements, writer_stages.as_slice()));
    let python_written = python.iter().flat_map(|(files, statements, skipped)| {
        files
            .iter()
            .map(move |f| (f, statements, skipped.as_slice()))
    });
    let mut acc = VerdictAcc::default();
    for (file, statements, skipped) in echoed.chain(python_written) {
        let mut runs = Runs::default();
        for (s, stages) in statements.iter().enumerate() {
            let others: Vec<&str> = stages
                .iter()
                .enumerate()
                .filter(|(i, _)| !skipped.contains(&(s, *i)))
                .map(|(_, st)| st.as_str())
                .collect();
            runs.add(statement_may_run(&others, &file.target));
        }
        let mut readings: Vec<&Reading> = Vec::new();
        for reading in file.snapshots.iter().flatten() {
            if !readings.contains(&reading) {
                readings.push(reading);
            }
        }
        for reader in runs.readers() {
            for reading in &readings {
                let verdict = judge_content(file, reading, &reader, cmd, depth, ctx);
                if let Some(deny) = acc.record(verdict) {
                    return deny;
                }
            }
        }
    }
    acc.finish()
}

/// What runs a written file.
enum Reader {
    Shell,
    Python,
    /// Another code interpreter (`node`, `perl`, …), by its normalized name.
    Code(String),
}

/// The verdict on one reading of `file`'s content, read by `reader`.
fn judge_content(
    file: &WrittenFile,
    reading: &Reading,
    reader: &Reader,
    cmd: &str,
    depth: usize,
    ctx: &Ctx<'_>,
) -> Decision {
    let (how, what) = match reader {
        Reader::Shell => (String::new(), "shell script".to_string()),
        Reader::Python => (" with Python".to_string(), "Python program".to_string()),
        Reader::Code(name) => (format!(" with `{name}`"), format!("`{name}` program")),
    };
    let shape = format!(
        "`{}` writes `{}`, and another part of this command may run that file{how}, so the \
written text is judged as the {what} it is",
        file.writer,
        file.target.shown(),
    );
    match reading {
        Err(why) => Decision::ask(format!(
            "{shape}; {why}, so blastguard cannot see the program the file will hold"
        )),
        Ok(text) if text.trim().is_empty() => Decision::Allow,
        Ok(text) => match reader {
            Reader::Python => {
                super::interpreter::python_written_program_verdict(&shape, text, cmd, depth, ctx)
            }
            Reader::Code(_) => interpreter_code_verdict(
                &shape,
                std::slice::from_ref(text),
                depth,
                ctx,
                false,
                PythonReader::NotPython,
                false,
            ),
            Reader::Shell => match analyze_shell_payload(text, depth, ctx) {
                Decision::Ask(r) => Decision::ask(format!("{shape}: {r}")),
                d => d,
            },
        },
    }
}

/// Every reading of `prev` followed by every reading of `add` (an append).
fn appended(prev: &Content, add: &Content) -> Content {
    let mut out: Content = Vec::new();
    for p in prev {
        for a in add {
            let r = match (p, a) {
                (Ok(p), Ok(a)) => Ok(format!("{p}{a}")),
                (Err(why), _) | (_, Err(why)) => Err(why.clone()),
            };
            if !out.contains(&r) {
                out.push(r);
            }
        }
    }
    if out.len() > MAX_READINGS {
        out.truncate(MAX_READINGS);
        out.push(Err(format!(
            "its appends combine into more than {MAX_READINGS} readings, more than blastguard \
follows"
        )));
    }
    out
}

/// Adds to `files` every file the `echo` / `printf` writers of `statements`
/// write, in command order, and to `writer_stages` the (statement, stage) of
/// every writer stage (a `tee` / `cat` stage that writes a writer's pipe
/// included). A writer inside a subshell word, a `{ …; }` group, a loop or
/// branch body, or a substitution the shell runs ([`nested_bodies`]) writes
/// its file too (tests/echo_script_variants_20261111.rs F2 / F6); its stage
/// is not a top-level writer stage.
fn collect_writes(
    statements: &[Vec<String>],
    files: &mut Vec<WrittenFile>,
    writer_stages: &mut Vec<(usize, usize)>,
) {
    for (s, stages) in statements.iter().enumerate() {
        for (i, stage) in stages.iter().enumerate() {
            for body in nested_bodies(stage) {
                let inner = split_statements_into_pipe_stages(&body);
                collect_writes(&inner, files, &mut Vec::new());
            }
            let words = stage_words(stage);
            let Some(mut writer) = echo_writer(&words) else {
                continue;
            };
            writer_stages.push((s, i));
            if !writer.stdout_redirected {
                if let Some(next) = stages.get(i + 1) {
                    let next_words = stage_words(next);
                    if let Some(piped) = piped_writes(&next_words) {
                        writer_stages.push((s, i + 1));
                        writer.writes = piped;
                    }
                }
            }
            let content = writer.content();
            for w in writer.writes {
                record_write(files, w, &content, &writer.name);
            }
        }
    }
}

/// Adds the content `content` that `writer` writes by `w` to `files`: a new
/// file, or one more snapshot of a file already written (an append joins the
/// previous snapshot's readings with `content`'s).
fn record_write(files: &mut Vec<WrittenFile>, w: Write, content: &Content, writer: &str) {
    let idx = match files.iter().position(|f| f.target == w.target) {
        Some(idx) => idx,
        None => {
            files.push(WrittenFile {
                target: w.target,
                snapshots: Vec::new(),
                writer: writer.to_string(),
            });
            files.len() - 1
        }
    };
    let file = &mut files[idx];
    let next = match (w.append, file.snapshots.last()) {
        (true, Some(prev)) => appended(prev, content),
        _ => content.clone(),
    };
    file.snapshots.push(next);
}

/// The files a Python here-document program writes, the statements its runs
/// are looked for in, and the (statement, stage) pairs left out of that search.
type PythonWrittenFiles = (Vec<WrittenFile>, Vec<Vec<String>>, Vec<(usize, usize)>);

/// The files the program of a quoted Python here-document
/// ([`super::heredoc::python_stdin_here_document`], `python3 - <<'PY'`)
/// writes, as [`super::interpreter::python_program_writes`] reads them
/// (backlog 245a4c5f: `open("t.sh","w").write("chmod 777 /etc/hosts\n")` in
/// that body followed by `sh t.sh` was Allow). A path that is not an exact
/// literal is an `Opaque` target (see the module documentation for what may
/// run one).
///
/// Their runs are looked for in the statements of the command WITHOUT the
/// body, so a body line (where the write call names the file) is not read as
/// a run. The first stage there that opens a here-document — the Python
/// reader — is left out of the search, so its `python3` word is not read as a
/// run of the file as a Python program; when no stage opens one, none is
/// left out.
///
/// `None` when there is no such here-document or the body yields no write.
/// Not read here (gaps): a Python program given by `-c`, by a file, or under
/// an unquoted delimiter; a run inside the Python program itself; and the
/// order of the write and the run (a run before the program counts as a run
/// of what it writes, as for `echo`).
fn python_heredoc_files(cmd: &str) -> Option<PythonWrittenFiles> {
    let parts = super::heredoc::python_stdin_here_document(cmd)?;
    let writes = super::interpreter::python_program_writes(&parts.body);
    if writes.is_empty() {
        return None;
    }
    let mut files = Vec::new();
    for w in writes {
        let target = match &w.path {
            Some(p) => Target::from_path(p, p),
            None => Target::Opaque("a path the Python program does not spell as a literal".into()),
        };
        let write = Write {
            target,
            append: w.append,
        };
        record_write(&mut files, write, &vec![w.content], w.call);
    }
    let statements = split_statements_into_pipe_stages(&parts.without_body);
    let reader = statements.iter().enumerate().find_map(|(s, stages)| {
        stages
            .iter()
            .position(|st| super::heredoc::opens_here_document(st))
            .map(|i| (s, i))
    });
    Some((files, statements, reader.into_iter().collect()))
}

/// An `echo` / `printf` stage: its name, its argument words (quotes still on),
/// the files its stdout is written to, and whether its stdout goes anywhere
/// but the pipe.
struct EchoWriter {
    name: String,
    args: Vec<String>,
    writes: Vec<Write>,
    stdout_redirected: bool,
}

fn echo_writer(words: &[String]) -> Option<EchoWriter> {
    let tokens: Vec<&str> = words.iter().map(String::as_str).collect();
    // A loop or branch body (`do echo …`, `then echo …`) or a group
    // (`{ echo …`) starts with a reserved word.
    let start = tokens
        .iter()
        .position(|t| !LEADING_KEYWORDS.contains(t))
        .unwrap_or(tokens.len());
    let idx = start
        + command_candidates(&tokens[start..])
            .into_iter()
            .find(|&i| {
                matches!(
                    normalized_command(tokens[start + i]).as_str(),
                    "echo" | "printf"
                )
            })?;
    let name = normalized_command(tokens[idx]);
    let mut args = Vec::new();
    let mut writes = Vec::new();
    let mut stdout_redirected = false;
    let mut i = idx + 1;
    while i < words.len() {
        let w = words[i].as_str();
        i += 1;
        let Some(redirect) = redirect_parts(w) else {
            args.push(w.to_string());
            continue;
        };
        let target = if redirect.rest.is_empty() {
            i += 1;
            words.get(i - 1).map(String::as_str).unwrap_or("")
        } else {
            redirect.rest
        };
        if !redirect.output || !matches!(redirect.fd, "" | "1" | "&") {
            continue;
        }
        stdout_redirected = true;
        if target.starts_with('&') || target.is_empty() {
            continue;
        }
        writes.push(Write {
            target: Target::from_word(target),
            append: redirect.op == ">>",
        });
    }
    Some(EchoWriter {
        name,
        args,
        writes,
        stdout_redirected,
    })
}

/// The files a `tee` or `cat` stage reading a writer's pipe writes it to.
/// `None` when the stage is neither.
fn piped_writes(words: &[String]) -> Option<Vec<Write>> {
    let tokens: Vec<&str> = words.iter().map(String::as_str).collect();
    let idx = command_candidates(&tokens)
        .into_iter()
        .find(|&i| matches!(normalized_command(tokens[i]).as_str(), "tee" | "cat"))?;
    let tee = normalized_command(tokens[idx]) == "tee";
    let append_all = tee
        && words[idx + 1..]
            .iter()
            .any(|w| w == "-a" || w == "--append" || (w.starts_with('-') && w.contains('a')));
    let mut writes = Vec::new();
    let mut i = idx + 1;
    while i < words.len() {
        let w = words[i].as_str();
        i += 1;
        if let Some(redirect) = redirect_parts(w) {
            let target = if redirect.rest.is_empty() {
                i += 1;
                words.get(i - 1).map(String::as_str).unwrap_or("")
            } else {
                redirect.rest
            };
            if redirect.output
                && matches!(redirect.fd, "" | "1" | "&")
                && !target.starts_with('&')
                && !target.is_empty()
            {
                writes.push(Write {
                    target: Target::from_word(target),
                    append: redirect.op == ">>",
                });
            }
            continue;
        }
        if tee && !w.starts_with('-') {
            writes.push(Write {
                target: Target::from_word(w),
                append: append_all,
            });
        }
    }
    Some(writes)
}

/// A redirect word split into its fd prefix (`""`, digits or `&`), its
/// operator and the glued target (empty when the target is the next word).
struct Redirect<'a> {
    fd: &'a str,
    op: &'a str,
    rest: &'a str,
    output: bool,
}

fn redirect_parts(word: &str) -> Option<Redirect<'_>> {
    if word.starts_with(['\'', '"']) {
        return None;
    }
    let fd_len = if word.starts_with('&') {
        1
    } else {
        word.chars().take_while(char::is_ascii_digit).count()
    };
    let (fd, tail) = word.split_at(fd_len);
    for (op, output) in [
        (">>", true),
        (">|", true),
        (">", true),
        ("<<<", false),
        ("<<-", false),
        ("<<", false),
        ("<>", false),
        ("<", false),
    ] {
        if let Some(rest) = tail.strip_prefix(op) {
            if fd == "&" && !output {
                return None;
            }
            return Some(Redirect {
                fd,
                op,
                rest,
                output,
            });
        }
    }
    None
}

impl EchoWriter {
    /// The text this writer puts on its stdout.
    fn content(&self) -> Content {
        if self.name == "printf" {
            printf_content(&self.args)
        } else {
            echo_content(&self.args)
        }
    }
}

fn literal_args(args: &[String]) -> Result<Vec<String>, String> {
    args.iter()
        .map(|a| {
            word_literal(a).ok_or_else(|| {
                format!("its argument `{a}` is expanded or unquoted by the writing shell")
            })
        })
        .collect()
}

/// Every text `echo` may print for `args`, one per shell reading (user ruling
/// 2026-11-11, tests/echo_script_variants_20261111.rs V13):
///   * bash: leading `-n` / `-e` / `-E` words are options; escapes are
///     expanded only under `-e`;
///   * zsh, or bash under `xpg_echo`: the same options; escapes are expanded
///     unless `-E`;
///   * dash / POSIX `sh`: only a first `-n` is an option; escapes are always
///     expanded.
fn echo_content(args: &[String]) -> Content {
    let lits = match literal_args(args) {
        Ok(lits) => lits,
        Err(why) => return vec![Err(why)],
    };
    let mut newline = true;
    let mut escapes: Option<bool> = None;
    let mut start = 0;
    for lit in &lits {
        let Some(flags) = lit
            .strip_prefix('-')
            .filter(|f| !f.is_empty() && f.chars().all(|c| matches!(c, 'n' | 'e' | 'E')))
        else {
            break;
        };
        for c in flags.chars() {
            match c {
                'n' => newline = false,
                'e' => escapes = Some(true),
                _ => escapes = Some(false),
            }
        }
        start += 1;
    }
    let text = lits[start..].join(" ");
    let dash_n = lits.first().is_some_and(|l| l == "-n");
    let dash_text = lits[usize::from(dash_n)..].join(" ");
    let mut out: Content = Vec::new();
    for reading in [
        echo_reading(&text, escapes == Some(true), newline),
        echo_reading(&text, escapes != Some(false), newline),
        echo_reading(&dash_text, true, !dash_n),
    ] {
        if !out.contains(&reading) {
            out.push(reading);
        }
    }
    out
}

/// What `echo` prints for `text`, its escapes expanded when `expand`: `\\`,
/// `\a`, `\b`, `\f`, `\n`, `\r`, `\t`, `\v`, and `\c` (nothing more is
/// printed, not even the newline). Any other escape is not modelled.
fn echo_reading(text: &str, expand: bool, newline: bool) -> Reading {
    let mut out = String::new();
    if expand {
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            out.push(match chars.next() {
                Some('\\') => '\\',
                Some('a') => '\x07',
                Some('b') => '\x08',
                Some('f') => '\x0c',
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('v') => '\x0b',
                Some('c') => return Ok(out),
                other => {
                    return Err(format!(
                        "its text (`{text}`) holds the escape `\\{}`, whose expansion by `echo` \
is not modelled",
                        other.map(String::from).unwrap_or_default()
                    ))
                }
            });
        }
    } else {
        out.push_str(text);
    }
    if newline {
        out.push('\n');
    }
    Ok(out)
}

fn printf_content(args: &[String]) -> Content {
    vec![printf_reading(args)]
}

fn printf_reading(args: &[String]) -> Reading {
    let lits = literal_args(args)?;
    let Some(format) = lits.first() else {
        return Ok(String::new());
    };
    if format.starts_with('-') || lits.len() > 1 {
        return Err(format!(
            "its arguments (`{}`) need printf's option or format-reuse rules, which are not \
modelled",
            lits.join(" ")
        ));
    }
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                _ => {
                    return Err(format!(
                        "its format (`{format}`) holds an escape that is not modelled"
                    ))
                }
            },
            '%' => match chars.next() {
                Some('%') => out.push('%'),
                _ => {
                    return Err(format!(
                        "its format (`{format}`) holds a conversion that is not modelled"
                    ))
                }
            },
            x => out.push(x),
        }
    }
    Ok(out)
}

/// The value the shell gives `word` without expanding anything, or `None`
/// when it would expand, glob, unescape or split it (see the module
/// documentation for the characters refused). A leading `~` or `~/` is kept as
/// `~`.
fn word_literal(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = word.chars().peekable();
    let mut first = true;
    while let Some(c) = chars.next() {
        match c {
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    x => out.push(x),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    '$' | '`' | '\\' => return None,
                    x => out.push(x),
                }
            },
            '~' if first && matches!(chars.peek(), None | Some('/')) => out.push('~'),
            '$' | '`' | '\\' | '*' | '?' | '[' | ']' | '{' | '}' | '~' | '<' | '>' | '|' | ';'
            | '&' | '(' | ')' | '#' => return None,
            x => out.push(x),
        }
        first = false;
    }
    Some(out)
}

/// Shell reserved words and grouping tokens that may stand before a stage's
/// command word.
const LEADING_KEYWORDS: &[&str] = &[
    "{", "(", "!", "if", "then", "elif", "else", "do", "while", "until", "time",
];

/// How a command may run a written file: as a shell script, as a Python
/// program, as another code interpreter's program, or several of these.
#[derive(Default)]
struct Runs {
    shell: bool,
    python: bool,
    /// The first non-Python code interpreter ([`is_code_interpreter`]) that
    /// may run it.
    code: Option<String>,
}

impl Runs {
    fn add(&mut self, other: Runs) {
        self.shell |= other.shell;
        self.python |= other.python;
        if self.code.is_none() {
            self.code = other.code;
        }
    }

    fn readers(self) -> Vec<Reader> {
        let mut out = Vec::new();
        if self.shell {
            out.push(Reader::Shell);
        }
        if self.python {
            out.push(Reader::Python);
        }
        if let Some(name) = self.code {
            out.push(Reader::Code(name));
        }
        out
    }
}

/// True for `python`, `python3` or `python3.N` (path and wrapper removed by
/// [`normalized_command`]).
fn is_python(n: &str) -> bool {
    n.strip_prefix("python").is_some_and(|v| {
        v.is_empty()
            || v == "3"
            || v.strip_prefix("3.")
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    })
}

/// The command text a stage runs nested inside it: the body of a subshell
/// word (`(sh x.sh)`) and of every substitution the shell runs
/// ([`executed_substitution_payloads`]).
fn nested_bodies(stage: &str) -> Vec<String> {
    let mut out = executed_substitution_payloads(stage);
    for word in quote_aware_words(stage) {
        let w = word.trim_start_matches('!');
        if !w.starts_with('(') {
            continue;
        }
        let chars: Vec<char> = w.chars().collect();
        // An unclosed subshell runs to the end of the word.
        let end = scan_balanced(&chars, 1, Stop::Paren, 0).unwrap_or(chars.len());
        out.push(chars[1..end].iter().collect());
    }
    out
}

/// How the stages `stages` (one pipeline, writer stages removed) may run
/// `target`, a subshell's or substitution's body included; see the module
/// documentation.
fn statement_may_run(stages: &[&str], target: &Target) -> Runs {
    let mut names = false;
    let mut runner_word = false;
    let mut python_word = false;
    // For a literal target: a python word whose program operand names the
    // file, and one whose program is stdin or cannot be determined (run of
    // the file when the pipeline names it anywhere, as before).
    let mut python_runs_named = false;
    let mut python_open = false;
    let mut code_word: Option<String> = None;
    let mut expansion_head = false;
    let mut runs = Runs::default();
    for stage in stages {
        for body in nested_bodies(stage) {
            for statement in split_statements_into_pipe_stages(&body) {
                let inner: Vec<&str> = statement.iter().map(String::as_str).collect();
                runs.add(statement_may_run(&inner, target));
            }
        }
        let words = stage_words(stage);
        let cleaned: Vec<&str> = words
            .iter()
            .map(|w| w.trim_matches(|c| matches!(c, '(' | ')' | '{' | '}' | ';')))
            .collect();
        let start = cleaned
            .iter()
            .position(|w| !w.is_empty() && !LEADING_KEYWORDS.contains(w))
            .unwrap_or(cleaned.len());
        let heads = command_candidates(&cleaned[start..]);
        for &h in &heads {
            let head = cleaned[start + h];
            if head.contains(['$', '`']) {
                expansion_head = true;
            }
            if let Target::Literal(path) = target {
                runs.shell |= word_names(head, path);
            }
        }
        for (k, w) in cleaned.iter().enumerate() {
            let n = normalized_command(w);
            if is_shell(&n) || matches!(n.as_str(), "." | "source") {
                runner_word = true;
            }
            if is_python(&n) {
                python_word = true;
                if let Target::Literal(path) = target {
                    match python_program(&cleaned[k + 1..]) {
                        PythonProgram::File(op) => match word_literal(op) {
                            Some(_) => python_runs_named |= word_names(op, path),
                            None => python_open = true,
                        },
                        PythonProgram::NoFile => {}
                        PythonProgram::StdinOrUndetermined => python_open = true,
                    }
                }
            }
            if code_word.is_none() && !is_python(&n) && is_code_interpreter(&n) {
                code_word = Some(n.clone());
            }
            if let Target::Literal(path) = target {
                names |= word_names(w, path);
            }
        }
    }
    let ran = match target {
        Target::Literal(_) => names,
        Target::Opaque(_) => true,
    };
    runs.shell |= (ran && runner_word) || (matches!(target, Target::Opaque(_)) && expansion_head);
    runs.python |= match target {
        Target::Literal(_) => python_runs_named || (names && python_open),
        Target::Opaque(_) => python_word,
    };
    if ran && runs.code.is_none() {
        runs.code = code_word;
    }
    runs
}

/// What a `python` word runs as its program, read from the words after it
/// (`args`, one stage).
enum PythonProgram<'a> {
    /// The first non-option operand: the program file. Later operands are
    /// `sys.argv` data, not the program (backlog dc27f669 / bf90fa9d).
    File(&'a str),
    /// `-c` (the program is the next argument's text) or `--version` /
    /// `--help*`: no program file is run.
    NoFile,
    /// `-` or no operand (the program is read from stdin), `-m` (the module
    /// may run a later operand as a program: `runpy`, `pdb`, `cProfile`), or
    /// an option blastguard does not know.
    StdinOrUndetermined,
}

/// Python's program operand among `args` (CPython's command line: options
/// first, then the program). Redirect words are skipped; an option it does not
/// know makes the program undetermined.
fn python_program<'a>(args: &[&'a str]) -> PythonProgram<'a> {
    let mut i = 0;
    while i < args.len() {
        let w = args[i];
        i += 1;
        if w.is_empty() {
            continue;
        }
        if let Some(redirect) = redirect_parts(w) {
            if redirect.rest.is_empty() {
                i += 1;
            }
            continue;
        }
        if w == "-" {
            return PythonProgram::StdinOrUndetermined;
        }
        if w == "--" {
            return match args.get(i) {
                Some(op) => PythonProgram::File(op),
                None => PythonProgram::StdinOrUndetermined,
            };
        }
        if let Some(long) = w.strip_prefix("--") {
            match long {
                "version" | "help" | "help-env" | "help-xoptions" | "help-all" => {
                    return PythonProgram::NoFile
                }
                "check-hash-based-pycs" => i += 1,
                l if l.starts_with("check-hash-based-pycs=") => {}
                _ => return PythonProgram::StdinOrUndetermined,
            }
            continue;
        }
        let Some(cluster) = w.strip_prefix('-') else {
            return PythonProgram::File(w);
        };
        for (pos, c) in cluster.char_indices() {
            match c {
                'c' => return PythonProgram::NoFile,
                'm' => return PythonProgram::StdinOrUndetermined,
                'W' | 'X' => {
                    if pos + 1 == cluster.len() {
                        i += 1;
                    }
                    break;
                }
                'b' | 'B' | 'd' | 'E' | 'h' | 'i' | 'I' | 'O' | 'P' | 'q' | 'R' | 's' | 'S'
                | 'u' | 'v' | 'V' | 'x' | '3' | '?' => {}
                _ => return PythonProgram::StdinOrUndetermined,
            }
        }
    }
    PythonProgram::StdinOrUndetermined
}

/// True when `word` (an input redirect prefix such as `<` / `0<` allowed)
/// names the file `path`: its literal value has `path`'s basename, or it holds
/// an expansion and its text contains that basename.
fn word_names(word: &str, path: &str) -> bool {
    let base = basename(path);
    let mut w = word;
    if let Some(pos) = w.find('<') {
        if w[..pos].chars().all(|c| c.is_ascii_digit()) {
            w = w[pos..].trim_start_matches('<');
        }
    }
    match word_literal(w) {
        Some(lit) => basename(lit.trim_end_matches('/')) == base,
        None => w.contains(base) || w.contains(['*', '?', '[']),
    }
}

/// `quote_aware_words(stage)` with a redirect glued to the end of a word split
/// off as its own word: `'rm -rf ~'>x.sh` is the argument `'rm -rf ~'` and the
/// redirect `>x.sh`, `sh<x.sh` is `sh` and `<x.sh`. A word whose unquoted
/// prefix before the operator is an fd (`2>`, `&>`) stays whole.
fn stage_words(stage: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in quote_aware_words(stage) {
        let (mut in_s, mut in_d) = (false, false);
        let mut split = None;
        let mut escaped = false;
        for (pos, c) in word.char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            match c {
                '\\' if !in_s => escaped = true,
                '\'' if !in_d => in_s = !in_s,
                '"' if !in_s => in_d = !in_d,
                '<' | '>' if !in_s && !in_d => {
                    split = Some(pos);
                    break;
                }
                _ => {}
            }
        }
        match split {
            Some(pos)
                if pos > 0
                    && !word[..pos].chars().all(|c| c.is_ascii_digit())
                    && &word[..pos] != "&" =>
            {
                out.push(word[..pos].to_string());
                out.push(word[pos..].to_string());
            }
            _ => out.push(word),
        }
    }
    out
}
