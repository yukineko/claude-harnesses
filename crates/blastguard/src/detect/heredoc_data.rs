//! Step 0a of `detect_bash`: QUOTED here-document bodies that the shell
//! parser (`detect::ast`, brush-parser) shows are DATA are removed before any
//! string rule reads the line (backlog f6102045, 19baba2f).
//!
//! User rulings applied (2026-11-08 / 2026-11-17 / 2026-11-18): a heredoc body
//! is judged as shell only when its target is a shell; a body fed to a
//! non-interpreter program as data (`git commit -F -`, `cat > notes.md`,
//! `cat > src/x.rs`) is data, not code, unless the same command runs the file
//! it lands in. Text and code are separated by the real parser, never by line
//! heuristics: which commands exist, which redirect is a heredoc, whether its
//! delimiter is quoted, where its body starts and ends, and what every other
//! command runs, all come from [`ast::analyze`].
//!
//! A body is data only when ALL of these hold (anything else keeps every body
//! in the text, judged by every rule exactly as before):
//!
//! * the heredoc is QUOTED (`<<'EOF'`, `<<"EOF"`, `<<\EOF`) and its body span
//!   in the text equals the parsed body byte for byte, its delimiter word is
//!   plain (`TAG`, `'TAG'`, `"TAG"`, `\TAG`; not `'EOF'EOF`), and the body
//!   does not start with a `#!` interpreter line. An unquoted heredoc is
//!   never removed: its `$(…)` / backticks execute;
//! * its command (the reader) is `cat` (no option but `-`), `tee` (only `-a` /
//!   `--append`) or `git commit` with `-F -` / `-F-` / `--file=-` /
//!   `--file -`, spelled bare. Every word is determined, except that
//!   `git commit` may carry an undetermined VALUE of `--trailer` / `--author`
//!   (the next word, or the whole `--trailer=…` / `--author=…` word) whose
//!   text runs no command (`$X`, `${X}`; never `$( )`, backquote, `<( )`,
//!   `>( )`). Whatever that value expands to (an UNQUOTED value may even split
//!   into several words, e.g. extra options), the here-document is still
//!   redirected to git's stdin only, so the body stays data; the words the
//!   value may turn into are judged by the rest of the line (`git_stage`'s
//!   whole-tree / unquoted-expansion rules). Any other undetermined word (a
//!   bare `$X` operand, the word after `-F` / `--file`, `-m`, `--date`, `-c`,
//!   `-C`, `--template`, an undetermined subcommand or command word) keeps the
//!   body. `program_source` of the reader is `None` (not an interpreter,
//!   `eval`, `source` or wrapper);
//! * where `cat` / `tee` put the body is a literal data, non-shell code or
//!   JSON / TOML / YAML file (`.md`, `.txt`, `.rs`, `.json`, `.jsonl`,
//!   `.toml`, `.yaml`, `.yml`, …; never `.sh`,
//!   extension-less or `.py`, which keep their own steps), the
//!   terminal, `/dev/null`, stderr, or a `$( )` word. Never a non-final pipe
//!   stage or a process substitution. `git commit` stores it in
//!   `COMMIT_EDITMSG`;
//! * no command of the line can run it as a program: every command whose
//!   `program_source` is `Some` must be an interpreter of a literal FILE, or
//!   an interpreter reading inherited / literal stdin, and that file's name
//!   (case-insensitively) is neither a name the body lands in nor named by any
//!   other word or redirect of the line (which could write or copy the body
//!   there); an undetermined word or redirect target of another command counts
//!   as naming it. The only `.` / `source` allowed is of the path spelled
//!   literally `$HOME/.cargo/env` (user ruling 2026-11-18: trusted like the
//!   profile); any other sourced file may redefine the reader. Any inline
//!   program (`sh -c`), piped stdin, undetermined command word, `eval`,
//!   wrapper running an interpreter, direct execution of a path so named,
//!   `alias` / `hash` / `enable` / `shopt` / `setopt`, a `PATH` assignment or
//!   a `PATH` word (`export PATH`) keeps every body;
//! * no other command names a landing path with a literal word — the file
//!   (by name, case-insensitively), a directory above it, `.` / `..` — unless
//!   it is a bare `cd` or a read-only tool on
//!   `heredoc::INERT_LATER_COMMANDS` (`rustc x.rs`, `chmod +x x.rs`,
//!   `rg --pre sh . src`, an unknown word keep every body). Bare
//!   `cargo test` / `cargo build` (no `--config` / `-Z`) is not running the
//!   written file whatever it names (user ruling 2026-11-18, B-4). An
//!   undetermined argument (`git add $X`) may or may not name one: the body
//!   is still removed (it is not shell code either way), but a verdict of
//!   Allow on the rest becomes Ask (refusal to guess) unless the line judged
//!   with every body kept is Allow too;
//! * the text outside the removed bodies is printable ASCII, space, tab and
//!   newline only (a CR, NBSP, zero-width or look-alike character is where
//!   the parser and the executing shell may disagree).
//!
//! A line that defines shell functions (`f() { echo hi; }; cat <<'EOF' …`) is
//! parsed with `ast::analyze_with_function_definitions` (backlog e79485cb):
//! the body commands of the functions are judged by the same conditions, and
//! a quoted body is removed only while the reader is the real `cat` / `tee` /
//! `git` and nothing on the line later runs the landing file. Every body is
//! kept when a defined function name equals the reader word, when any command
//! (function bodies included) has a program source (`eval`, `source`, `.`,
//! `sh -c`, an interpreter, a wrapper) or fails the checks above
//! (`alias`, `hash`, `enable`, `shopt`, `setopt`, `PATH`, a landing path
//! named), when a call of a defined function has a piped or redirected stdin,
//! or when a call of a defined function names a landing path (a function is
//! not the inert tool of the same name). A quoted body is never expanded, so
//! `$(f)` in it is text, not a call of `f`.
//!
//! A command longer than `ast::MAX_PARSE_BYTES` is not parsed whole (ruling
//! 2026-11-16). There the one quoted heredoc to a non-shell code file that
//! `heredoc::quoted_code_file_here_document_parts` splits off is removed only
//! when the parser, run on the rest (which must fit the cap), finds exactly
//! one heredoc, empty, quoted, at the split point, that passes every condition
//! above.
//!
//! RESIDUAL, as for the string steps: a data file written here can be run by a
//! LATER, separate tool call, which is judged on its own text.

use std::ops::Range;

use super::ast::{
    self, program_source, Analysis, Context, FdTarget, ProgramSource, RedirOp, RedirTarget, Script,
    SimpleCommand, StdinSource, Value, Word,
};
use super::heredoc::{
    is_data_file_target, is_non_shell_code_file_target, plain_here_document_delimiter,
    quoted_code_file_here_document_parts, INERT_LATER_COMMANDS,
};
use crate::model::Decision;

/// The command with its data heredoc bodies removed, and whether some other
/// command has an undetermined word that may name a landing file.
pub(super) struct DataView {
    pub(super) text: String,
    unsure: bool,
}

impl DataView {
    /// `verdict` (of [`DataView::text`]). When an undetermined word may name a
    /// landing file, whether the body reaches a file another command uses
    /// cannot be determined, so an Allow stands only if `whole` (the line
    /// judged with every body kept, as before this step) is Allow too; else
    /// it is Ask — a refusal to guess, not a reading of the body as shell
    /// code (which `whole` is).
    pub(super) fn floor(&self, verdict: Decision, whole: impl FnOnce() -> Decision) -> Decision {
        match verdict {
            Decision::Allow if self.unsure => match whole() {
                Decision::Allow => Decision::Allow,
                _ => Decision::ask(
                    "a here-document body is written to a file that an undetermined word of another command may name — blastguard cannot tell whether that command uses it",
                ),
            },
            v => v,
        }
    }
}

/// `cmd` with every data heredoc body removed (opener and terminator lines
/// kept), or `None` when no non-empty body may be removed.
pub(super) fn without_data_heredoc_bodies(cmd: &str) -> Option<DataView> {
    let (text, unsure) = removed_bodies(cmd)?;
    Some(DataView { text, unsure })
}

fn removed_bodies(cmd: &str) -> Option<(String, bool)> {
    if !cmd.contains("<<") {
        return None;
    }
    if cmd.len() > ast::MAX_PARSE_BYTES {
        return over_cap_view(cmd);
    }
    // The one consumer of the function-definition-aware entry point (backlog
    // e79485cb): a line that defines shell functions is Known here, with the
    // body commands emitted, and `data_bodies` keeps every body unless the
    // function rules hold.
    let Analysis::Known(script) = ast::analyze_with_function_definitions(cmd) else {
        return None;
    };
    let (spans, unsure) = data_bodies(&script, cmd)?;
    let mut spans: Vec<Range<usize>> = spans.into_iter().filter(|r| !r.is_empty()).collect();
    if spans.is_empty() {
        return None;
    }
    spans.sort_by_key(|r| (r.start, r.end));
    spans.dedup();
    let mut out = String::with_capacity(cmd.len());
    let mut at = 0;
    for r in spans {
        // Overlapping spans would mean the parse is not what it seems.
        if r.start < at {
            return None;
        }
        out.push_str(cmd.get(at..r.start)?);
        at = r.end;
    }
    out.push_str(cmd.get(at..)?);
    plain_text(&out).then_some((out, unsure))
}

/// True when `text` (the command outside the removed bodies) holds only
/// printable ASCII, space, tab and newline. Anything else (a carriage return,
/// NBSP, zero-width or look-alike character) is where brush-parser's bash
/// reading and the executing shell may disagree, so it is undetermined and
/// every body stays in the text.
fn plain_text(text: &str) -> bool {
    text.chars()
        .all(|c| c.is_ascii_graphic() || matches!(c, ' ' | '\t' | '\n'))
}

/// The over-cap path (see the module doc).
fn over_cap_view(cmd: &str) -> Option<(String, bool)> {
    let parts = quoted_code_file_here_document_parts(cmd)?;
    if parts.body.is_empty() || parts.without_body.len() > ast::MAX_PARSE_BYTES {
        return None;
    }
    let rest = parts.without_body;
    if !plain_text(&rest) {
        return None;
    }
    let Analysis::Known(script) = ast::analyze(&rest) else {
        return None;
    };
    let heredocs = script
        .commands
        .iter()
        .flat_map(|c| &c.redirects)
        .filter(|r| matches!(r.target, RedirTarget::HereDoc(_)))
        .count();
    let at = parts.head_len;
    let (spans, unsure) = data_bodies(&script, &rest)?;
    (heredocs == 1 && spans.len() == 1 && spans.first() == Some(&(at..at)))
        .then_some((rest, unsure))
}

/// The body spans of every data heredoc of `script` (possibly empty spans),
/// with whether another command has an undetermined word that may name a
/// landing path, or `None` when some command of the line may run one of them
/// or literally names a landing path.
fn data_bodies(script: &Script, text: &str) -> Option<(Vec<Range<usize>>, bool)> {
    let mut spans = Vec::new();
    let mut landing: Vec<String> = Vec::new();
    let mut readers: Vec<usize> = Vec::new();
    for (i, c) in script.commands.iter().enumerate() {
        for r in &c.redirects {
            let RedirTarget::HereDoc(h) = &r.target else {
                continue;
            };
            let exact = matches!(&h.body, Value::Literal(v) if text.get(h.body_span.clone()) == Some(v.as_str()));
            if !h.quoted || !exact || !plain_delimiter(text, &r.span) || shebang(text, h) {
                continue;
            }
            if let Some(paths) = data_reader_landing(c) {
                spans.push(h.body_span.clone());
                landing.extend(paths);
                readers.push(i);
            }
        }
    }
    if spans.is_empty() {
        return None;
    }
    if script.has_function_definition && !functions_harmless(script, &readers) {
        return None;
    }
    let mut unsure = false;
    for (i, c) in script.commands.iter().enumerate() {
        if !runs_no_landing_file(script, i, c, &landing) {
            return None;
        }
        if !readers.contains(&i) {
            match names_landing_path(c, &landing, &script.function_names) {
                Naming::Clear => {}
                Naming::Undetermined => unsure = true,
                Naming::Names => return None,
            }
        }
    }
    Some((spans, unsure))
}

/// The extra conditions when the line defines shell functions (backlog
/// e79485cb). The body commands of every function are in `script.commands`, so
/// the per-command checks of [`data_bodies`] already cover them; this adds
/// what only a function can do. `false` keeps every body when:
///
/// * a defined function name equals a reader word (`cat() { sh; }; cat <<'EOF'`
///   runs the function, not the real reader);
/// * any command of the line, function bodies included, has a
///   `program_source` (`eval`, `source`, `.`, `sh -c`, an interpreter, a
///   wrapper): it can run the landing file or redefine a command;
/// * a call of a defined function reads a redirected or piped stdin (its body
///   could pass that to something that runs it).
///
/// `alias` / `hash` / `enable` / `shopt` / `setopt` / `PATH` are refused for
/// every command by `runs_no_landing_file`, and a landing path named by a body
/// command by [`names_landing_path`].
fn functions_harmless(script: &Script, readers: &[usize]) -> bool {
    let defined = |c: &SimpleCommand| {
        matches!(c.argv.first().map(|w| &w.value),
            Some(Value::Literal(h)) if script.function_names.contains(h))
    };
    for (i, c) in script.commands.iter().enumerate() {
        if readers.contains(&i) && defined(c) {
            return false;
        }
        if program_source(c).is_some() {
            return false;
        }
        if defined(c) {
            if matches!(c.context, Context::Pipeline { pos, .. } if pos > 0) {
                return false;
            }
            let stdin_redirected = c.redirects.iter().any(|r| {
                matches!(
                    r.op,
                    RedirOp::In
                        | RedirOp::InOut
                        | RedirOp::DupIn
                        | RedirOp::HereDoc
                        | RedirOp::HereString
                )
            });
            if stdin_redirected {
                return false;
            }
        }
    }
    true
}

/// Whether a command names a landing path ([`names_landing_path`]).
enum Naming {
    Clear,
    /// Some word is undetermined (`git add $X`): it may or may not name one.
    Undetermined,
    Names,
}

/// Commands that only change directory or read / list / print, spelled bare
/// (`heredoc::INERT_LATER_COMMANDS`, plus `cd`): they may name a landing file
/// or its directory without running it.
fn reads_only(word: &str) -> bool {
    word == "cd" || INERT_LATER_COMMANDS.contains(&word)
}

/// User ruling 2026-11-18 (B-4): `cargo test` / `cargo build` after writing a
/// `.rs` file is not running that file — the same as running them over code
/// already on disk. Only that bare spelling, subcommand first, with no
/// `--config` / `-Z` option (which can install a runner or wrapper); any other
/// `cargo` invocation is judged like every other command.
fn cargo_test_or_build(c: &SimpleCommand) -> bool {
    // An undetermined word: not provably the bare spelling.
    let Ok(argv) = literal_argv(c) else {
        return false;
    };
    matches!(argv.as_slice(), ["cargo", "test" | "build", ..])
        && !argv
            .iter()
            .any(|a| a.starts_with("--config") || a.starts_with("-Z"))
}

/// Whether `c` names a landing path (the file, an ancestor directory of it,
/// `.` / `..`) with a literal word ([`Naming::Names`]: the body is kept), may
/// name one with an undetermined word ([`Naming::Undetermined`]: the body is
/// removed and an Allow floored by [`DataView::floor`]), or names none
/// or only reads it ([`reads_only`]). A command not known to only read what it
/// names may run it (`rustc x.rs`, `chmod +x x.rs`, `rg --pre sh . src`). An
/// undetermined or empty command word is [`Naming::Names`].
fn names_landing_path(c: &SimpleCommand, landing: &[String], functions: &[String]) -> Naming {
    let Some(Value::Literal(head)) = c.argv.first().map(|w| &w.value) else {
        return if c.argv.is_empty() {
            Naming::Clear
        } else {
            Naming::Names
        };
    };
    // No body lands in a file (terminal, `/dev/null`, a `$( )` word): there is
    // nothing for any word to name.
    if landing.is_empty() {
        return Naming::Clear;
    }
    // A defined function is not the inert tool of the same name: its body
    // decides what it does with its arguments (`ls() { xargs "$@"; }`).
    let shadowed = functions.iter().any(|f| f == head);
    if !shadowed && (reads_only(head) || cargo_test_or_build(c)) {
        return Naming::Clear;
    }
    let lower: Vec<String> = landing.iter().map(|l| l.to_ascii_lowercase()).collect();
    let mut naming = Naming::Clear;
    for w in &c.argv[1..] {
        match &w.value {
            Value::Undetermined(_) => naming = Naming::Undetermined,
            Value::Literal(s) => {
                let s = s.to_ascii_lowercase();
                let t = s.trim_end_matches('/');
                if t.is_empty()
                    || t == "."
                    || t == ".."
                    || lower.iter().any(|l| {
                        basename(&s) == basename(l)
                            || *l == t
                            || l.starts_with(&format!("{t}/"))
                            || l.contains(&format!("/{t}/"))
                    })
                {
                    return Naming::Names;
                }
            }
        }
    }
    naming
}

/// True when the heredoc operator at `span` (as the parser placed it) is
/// `<<` / `<<-` followed by a plain delimiter word
/// (`heredoc::plain_here_document_delimiter`: `TAG`, `'TAG'`, `"TAG"`,
/// `\TAG`). A glued or mixed spelling (`<<'EOF'EOF`) is where the parser and
/// the executing shell may disagree on the closing line.
fn plain_delimiter(text: &str, span: &Range<usize>) -> bool {
    let Some(s) = text.get(span.clone()) else {
        return false;
    };
    let s = s.trim_start_matches(|c: char| c.is_ascii_digit());
    let Some(rest) = s.strip_prefix("<<") else {
        return false;
    };
    let rest = rest.strip_prefix('-').unwrap_or(rest);
    plain_here_document_delimiter(rest.trim_start_matches([' ', '\t']))
}

/// True when the body starts with an interpreter line (`#!`, not a Rust
/// `#![…]` attribute): a script for an interpreter is code, as in
/// `heredoc::non_shell_code_file_here_document`.
fn shebang(text: &str, h: &ast::HereDoc) -> bool {
    let body = text.get(h.body_span.clone()).unwrap_or("");
    body.starts_with("#!") && !body.starts_with("#![")
}

/// The literal value of every argv word of `c`, or `Err` with the reason of
/// the first undetermined word (every caller treats that as not the
/// permitted shape: not a data reader, not a bare `cargo test`).
fn literal_argv(c: &SimpleCommand) -> Result<Vec<&str>, &str> {
    c.argv
        .iter()
        .map(|w| match &w.value {
            Value::Literal(s) => Ok(s.as_str()),
            Value::Undetermined(why) => Err(why.as_str()),
        })
        .collect()
}

fn basename(p: &str) -> &str {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or(p)
}

/// When `c` only stores or prints its stdin, the file names (basenames) that
/// stdin may land in; `None` when `c` is not such a reader.
fn data_reader_landing(c: &SimpleCommand) -> Option<Vec<String>> {
    if program_source(c).is_some() {
        return None;
    }
    // `git commit` is the one reader that may hold an undetermined word: the
    // value of `--trailer` / `--author` (see `git_commit_reads_message_from_stdin`).
    if git_commit_reads_message_from_stdin(c) {
        return Some(vec![".git/COMMIT_EDITMSG".to_string()]);
    }
    // An undetermined word: not provably a data reader, so its body is kept.
    let argv = literal_argv(c).ok()?;
    let (&name, args) = argv.split_first()?;
    match name {
        "cat" if args.iter().all(|a| *a == "-") => stdout_landing(c),
        "tee" => {
            let mut names = stdout_landing(c)?;
            for a in args {
                if matches!(*a, "-a" | "--append") {
                    continue;
                }
                if a.starts_with('-') || !data_or_code_file(a) {
                    return None;
                }
                names.push((*a).to_string());
            }
            Some(names)
        }
        _ => None,
    }
}

/// True when `c` is `git commit` whose message is read from stdin: `-F -`,
/// `-F-`, `--file=-` or `--file -`, spelled as literal words. Its undetermined
/// words may only be the VALUE of `--trailer` / `--author`: the next word, or
/// the whole `--trailer=…` / `--author=…` word, and only if its text runs no
/// command ([`reads_no_command`]). Whatever it expands to (an unquoted value
/// may split into several words), the here-document still reaches git's
/// stdin only; the resulting words are judged by the rest of the line. Any
/// other undetermined word keeps the body: a bare `$X` operand, the word after
/// `-F` / `--file` (which must be the literal `-`), after `-m`, `--date`, `-c`,
/// `-C` or `--template`, and an undetermined subcommand or command word.
fn git_commit_reads_message_from_stdin(c: &SimpleCommand) -> bool {
    let words = &c.argv;
    if words.len() < 2
        || literal_word(&words[0]) != Ok("git")
        || literal_word(&words[1]) != Ok("commit")
    {
        return false;
    }
    let mut from_stdin = false;
    let mut i = 2;
    while let Some(w) = words.get(i) {
        i += 1;
        match literal_word(w) {
            Ok("--trailer" | "--author") => match words.get(i) {
                Some(v) if literal_word(v).is_ok() || reads_no_command(&v.raw) => i += 1,
                _ => return false,
            },
            Ok("-F-" | "--file=-") => from_stdin = true,
            Ok("-F" | "--file") => {
                if words.get(i).map(literal_word) != Some(Ok("-")) {
                    return false;
                }
                from_stdin = true;
                i += 1;
            }
            Ok(_) => {}
            Err(_) => {
                let glued = w.raw.starts_with("--trailer=") || w.raw.starts_with("--author=");
                if !(glued && reads_no_command(&w.raw)) {
                    return false;
                }
            }
        }
    }
    from_stdin
}

/// The text of a literal word; `Err` with the reason of an undetermined one
/// (the same shape as [`literal_argv`]).
fn literal_word(w: &Word) -> Result<&str, &str> {
    match &w.value {
        Value::Literal(s) => Ok(s.as_str()),
        Value::Undetermined(why) => Err(why.as_str()),
    }
}

/// True when the raw text of an undetermined option value runs no command: no
/// backquote, `$(`, `<(` or `>(`. A parameter expansion (`$TASK_ID`, `${X}`)
/// only reads a value; a command substitution or process substitution runs one.
fn reads_no_command(raw: &str) -> bool {
    !raw.contains('`') && ["$(", "<(", ">("].iter().all(|p| !raw.contains(p))
}

/// A literal landing file whose body is not shell: `/dev/null`, the data and
/// non-shell code files of `heredoc`, or a `.json` / `.jsonl` / `.toml` /
/// `.yaml` / `.yml` file (backlog 9a4e07cf; user ruling 2026-11-08: only
/// `.sh` / extension-less bodies are shell; user ruling 2026-11-18: config
/// formats are data). The configuration files that make commands run
/// (`.claude/settings*.json`, `hooks.json`, `.githooks/`) are NOT exempted by
/// this: the write's TARGET path is still judged (Protected) by the rules
/// that see the redirect, whatever the body is. Whether the same command
/// runs the file is checked separately.
fn data_or_code_file(p: &str) -> bool {
    if is_data_file_target(p) || is_non_shell_code_file_target(p) {
        return true;
    }
    let shape_ok = !p.contains([
        '$', '`', '\\', '\'', '"', '*', '?', '[', ']', '{', '}', '(', ')', '<', '>', '|', ';', '&',
    ]);
    let ext = basename(p)
        .rsplit_once('.')
        .map(|(stem, ext)| (stem.is_empty(), ext));
    shape_ok
        && matches!(ext, Some((false, e))
            if ["json", "jsonl", "toml", "yaml", "yml"]
                .iter()
                .any(|d| e.eq_ignore_ascii_case(d)))
}

/// Where the stdout of a `cat` / `tee` reader goes, as landing file names;
/// `None` when it may reach something that runs it.
fn stdout_landing(c: &SimpleCommand) -> Option<Vec<String>> {
    match c.context {
        Context::Pipeline { pos, len } if pos + 1 < len => return None,
        Context::ProcessSubstitution => return None,
        _ => {}
    }
    let mut names = Vec::new();
    for r in &c.redirects {
        let stdout = matches!(r.fd, None | Some(1));
        match r.op {
            RedirOp::Out | RedirOp::Append | RedirOp::Clobber if stdout => {}
            RedirOp::OutErr | RedirOp::AppendOutErr => {}
            RedirOp::DupOut if stdout => match &r.target {
                RedirTarget::Fd(FdTarget::Number(2)) => continue,
                _ => return None,
            },
            _ => continue,
        }
        match &r.target {
            RedirTarget::Path(Value::Literal(p)) if data_or_code_file(p) => {
                names.push(p.clone());
            }
            _ => return None,
        }
    }
    Some(names)
}

/// Commands that change what a later command word runs.
const LOOKUP_CHANGERS: &[&str] = &["alias", "hash", "enable", "shopt", "setopt"];

/// True when command `i` (`c`) of `script` cannot run a file named in
/// `landing`, nor a body by any other route (see the module doc).
fn runs_no_landing_file(script: &Script, i: usize, c: &SimpleCommand, landing: &[String]) -> bool {
    if c.assignments.iter().any(|(n, _)| n == "PATH") {
        return false;
    }
    let Some(first) = c.argv.first() else {
        return true;
    };
    let Value::Literal(word) = &first.value else {
        return false;
    };
    if LOOKUP_CHANGERS.contains(&word.as_str())
        || c.argv
            .iter()
            .any(|w| matches!(&w.value, Value::Literal(s) if s == "PATH" || s.starts_with("PATH=")))
    {
        return false;
    }
    let file_ok = |p: &str| file_not_reachable(script, i, p, landing);
    if word.contains('/') && !file_ok(word) {
        return false;
    }
    match program_source(c) {
        None => true,
        Some(ProgramSource::File(Value::Literal(p)))
        | Some(ProgramSource::Stdin(StdinSource::File(Value::Literal(p)))) => file_ok(&p),
        Some(ProgramSource::Stdin(StdinSource::Inherited | StdinSource::Literal(_))) => true,
        // User ruling 2026-11-18 (B-1): only a source of `$HOME/.cargo/env`
        // is trusted like the profile (it does not redefine `cat` / `git`).
        // Any other sourced file may define a function named like the reader,
        // so every body stays judged.
        Some(ProgramSource::Undetermined(_)) if matches!(word.as_str(), "." | "source") => {
            c.argv.len() == 2
                && c.argv.get(1).is_some_and(
                    |w| matches!(&w.value, Value::Literal(p) if is_cargo_env(p) && file_ok(p)),
                )
        }
        Some(_) => false,
    }
}

/// True when `p` is `$HOME/.cargo/env` for the hook's absolute `HOME`. The
/// readings resolve `$HOME` in the text before this step runs
/// (`readings::static_bindings`, SUBSET.md ruling 12), so the spelling
/// `"$HOME/.cargo/env"` arrives here as that absolute path; the same path
/// spelled out absolutely is the same file. `HOME` unset, relative or
/// holding `..` trusts nothing.
fn is_cargo_env(p: &str) -> bool {
    match std::env::var("HOME") {
        Ok(home) if home.starts_with('/') && !home.split('/').any(|s| s == "..") => {
            p == format!("{}/.cargo/env", home.trim_end_matches('/'))
        }
        _ => false,
    }
}

/// True when the file `p` that command `i` runs is not a landing file and no
/// other command's word, assignment or redirect names a file of that name.
fn file_not_reachable(script: &Script, i: usize, p: &str, landing: &[String]) -> bool {
    // Case-insensitive: the default macOS file system is.
    let name = basename(p);
    if name.is_empty()
        || landing
            .iter()
            .any(|l| basename(l).eq_ignore_ascii_case(name))
    {
        return false;
    }
    // An undetermined word may name it (cannot determine -> restrictive side).
    let names_it = |v: &Value| match v {
        Value::Literal(s) => basename(s).eq_ignore_ascii_case(name),
        Value::Undetermined(_) => true,
    };
    !script
        .commands
        .iter()
        .enumerate()
        .filter(|(j, _)| *j != i)
        .any(|(_, o)| {
            o.argv.iter().any(|w| names_it(&w.value))
                || o.assignments.iter().any(|(_, w)| names_it(&w.value))
                || o.redirects.iter().any(|r| match &r.target {
                    RedirTarget::Path(v) => names_it(v),
                    _ => false,
                })
        })
}
