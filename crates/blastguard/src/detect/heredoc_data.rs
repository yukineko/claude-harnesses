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
//!   `eval`, `source` or wrapper). The reader may also be a COMPOUND loop
//!   (`while IFS= read -r c; do echo "$c"; done <<'EOF'`, backlog b5d6ac29)
//!   whose only commands are `read` and `echo` printing plain `$NAME`
//!   operands into plain lower-case non-special variables, the heredoc being
//!   its only redirect, at the top level or the last pipe stage, on a line
//!   with no `set` / `trap` / `exec` / `declare` / `source` … and no
//!   assignment to a special variable (`PS4`); see [`data_loop_landing`] —
//!   any other command in the loop or such state change on the line keeps
//!   the body. For every reader, a compound command that encloses it and
//!   redirects its output (`{ cat <<'EOF'; } > x.sh`) is where the body lands
//!   too ([`enclosing_landing`]): a non-data target keeps the body;
//! * where `cat` / `tee` put the body is a literal data, non-shell code or
//!   JSON / TOML / YAML file (`.md`, `.txt`, `.rs`, `.json`, `.jsonl`,
//!   `.toml`, `.yaml`, `.yml`, …; never `.sh`,
//!   extension-less or `.py`, which keep their own steps), the
//!   terminal, `/dev/null`, stderr, or a `$( )` word. Never a non-final pipe
//!   stage or a process substitution. `git commit` stores it in
//!   `COMMIT_EDITMSG`. A RELATIVE literal data / code file target after an
//!   undetermined `cd` (`cd "$WT" && cat >> docs/a.md <<'EOF'`, backlog
//!   1f705907) is such a file too: its name is known, only its directory is
//!   not (`unplaced_data_landing`). Then every literal argument of a command
//!   not exempted in the next condition may name a directory above it, so an
//!   Allow on the rest is floored at Ask as for an undetermined argument. The
//!   opener's own target is still judged where it may land by the redirect
//!   rules (an unresolvable placement is refused there);
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
//!   (by name, case-insensitively), a directory above it (`/` included),
//!   `.` / `..`; an empty word such as `sed -i ''` names no path — unless
//!   it is a bare `cd`, a bare `mkdir` (it only creates directories; backlog
//!   648a6641) or a read-only tool on `heredoc::INERT_LATER_COMMANDS`
//!   (`rustc x.rs`, `chmod +x x.rs`, `rg --pre sh . src`, an unknown word keep
//!   every body). Bare
//!   `cargo test` / `cargo build` (no `--config` / `-Z`) is not running the
//!   written file whatever it names (user ruling 2026-11-18, B-4). Nor is
//!   `git add` / `git commit` spelled with the subcommand directly after
//!   `git` and every word literal (backlog 1f705907): they copy what they
//!   name into the index / a commit and run none of it (`git_records_only`;
//!   hooks and filters git runs from existing configuration are the
//!   repository's, a residual as for a `git commit -F -` reader). An
//!   undetermined argument (`git add $X`) may or may not name one: the body
//!   is still removed (it is not shell code either way), but a verdict of
//!   Allow on the rest becomes Ask (refusal to guess) unless the line judged
//!   with every body kept is Allow too. An interpreter running a literal
//!   program FILE whose text is not in the command (`python3 probe.py <dir>`,
//!   backlog 2026-11-14), with nothing on the line feeding its stdin (a pipe
//!   or `<` keeps every body), that literally names a landing file or a directory
//!   above it is treated the same way: the previous condition has already
//!   shown it does not run the landing file itself, and whether its unseen
//!   program uses the body is as unknown as what the command alone does, so
//!   the body is removed (a `.txt` body is never read as shell) and an Allow
//!   is floored at Ask. A command that can run the landing file (`bash
//!   cmds.txt`, `. cmds.txt`, `./cmds.txt`, `sh -c`, piped stdin, `eval`, a
//!   wrapper) is refused by the previous condition and keeps every body;
//! * the text outside the removed bodies is printable ASCII, space, tab and
//!   newline only (a CR, NBSP, zero-width or look-alike character is where
//!   the parser and the executing shell may disagree), except inside an argv
//!   word the parser read as one single-quoted string `'…'` (backlog
//!   9236d343: `sed -n '/── x/p'`), where every byte is literal up to the
//!   next `'` and no character can move a word or quote boundary.
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
//! not the inert tool of the same name). A quoted heredoc INSIDE a function
//! body is never data (user ruling (a), 2026-11-20): its reader's stdout is
//! the function's, which a call can send to a file run later (`f > p.awk`), a
//! pipe or a `$(f)`, so the body stays judged exactly as without this step
//! (`ast::Script::function_body_ranges` says which commands are in a function
//! body). Only a quoted heredoc outside every function body can be removed. A
//! quoted body is never expanded, so `$(f)` in it is text, not a call of `f`.
//!
//! A command longer than `ast::MAX_PARSE_BYTES` is not parsed whole (ruling
//! 2026-11-16). There the one quoted heredoc to a non-shell code file that
//! `heredoc::quoted_code_file_here_document_parts` splits off is removed only
//! when the parser, run on the rest (which must fit the cap), finds exactly
//! one heredoc, empty, quoted, at the split point, that passes every condition
//! above.
//!
//! This module also answers, for the Python-file split of step 0b
//! (`heredoc::python_file_here_document`, which removes nothing and judges a
//! quoted `.py` body as Python), which command reads the one quoted
//! here-document when inert commands precede it on its opener line
//! ([`python_file_reader_start`], backlog 87661417).
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
    for r in &spans {
        // Overlapping spans would mean the parse is not what it seems.
        if r.start < at {
            return None;
        }
        out.push_str(cmd.get(at..r.start)?);
        at = r.end;
    }
    out.push_str(cmd.get(at..)?);
    let literal = single_quoted_interiors(&script, cmd);
    plain_outside(cmd, &spans, &literal).then_some((out, unsure))
}

/// True when the text of `cmd` outside the removed body spans `removed` and
/// outside the single-quoted word interiors `literal` holds only printable
/// ASCII, space, tab and newline. Anything else (a carriage return, NBSP,
/// zero-width or look-alike character) is where brush-parser's bash reading
/// and the executing shell may disagree, so it is undetermined and every body
/// stays in the text.
///
/// Backlog 9236d343: the check used to cover every char outside the bodies,
/// single-quoted text included, so a `sed -n '/^\/\/ ── shared fixture/,…/p'`
/// pattern (U+2500) on the same line made the whole step refuse; every
/// quoted body then fell back to the string rules, which split its lines as
/// code (`` …); `--lib`/`--doc` selectors `` became a segment whose command
/// word is a backtick expansion, an Ask). Inside a single-quoted word the
/// shell reads every byte literally up to the next `'` byte, and a UTF-8
/// multi-byte character never contains that byte, so a non-ASCII char there
/// cannot change where the word, the command or any quote ends — the only
/// thing this check guards. Which text is such an interior is taken from the
/// parser ([`single_quoted_interiors`]), never from a scan of the text.
fn plain_outside(cmd: &str, removed: &[Range<usize>], literal: &[Range<usize>]) -> bool {
    cmd.char_indices().all(|(at, c)| {
        c.is_ascii_graphic()
            || matches!(c, ' ' | '\t' | '\n')
            || removed.iter().chain(literal).any(|r| r.contains(&at))
    })
}

/// The interiors (between the quotes, byte ranges of `cmd`) of the argv words
/// of `script` that the parser read as ONE single-quoted string: quoting
/// [`ast::Quoting::Single`], the word's span in `cmd` holding exactly its raw
/// text, and that raw text `'…'` with no other `'` (so no second quoted piece
/// whose join the parser and the shell might read differently). A word the
/// parser placed inexactly, a mixed / double-quoted / ANSI-C word, an
/// assignment value or a redirect target is not an interior: its non-ASCII
/// characters stay checked by [`plain_outside`].
fn single_quoted_interiors(script: &Script, cmd: &str) -> Vec<Range<usize>> {
    script
        .commands
        .iter()
        .flat_map(|c| &c.argv)
        .filter(|w| w.quoting == ast::Quoting::Single)
        .filter(|w| cmd.get(w.span.clone()) == Some(w.raw.as_str()))
        .filter_map(|w| {
            let inner = w.raw.strip_prefix('\'')?.strip_suffix('\'')?;
            (!inner.contains('\'')).then(|| w.span.start + 1..w.span.end - 1)
        })
        .collect()
}

/// [`plain_outside`] for a text with nothing removed and no parsed word.
fn plain_text(text: &str) -> bool {
    plain_outside(text, &[], &[])
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
        // Ruling (a), 2026-11-20: a quoted heredoc INSIDE a function body is
        // never data. Its reader's stdout is the function's, which the call
        // sends anywhere (a file run later, a pipe, `$(f)` into `awk`), so its
        // body stays in the judged text exactly as without this step.
        if script.function_body_ranges.iter().any(|r| r.contains(&i)) {
            continue;
        }
        for r in &c.redirects {
            let RedirTarget::HereDoc(h) = &r.target else {
                continue;
            };
            let exact = matches!(&h.body, Value::Literal(v) if text.get(h.body_span.clone()) == Some(v.as_str()));
            if !h.quoted || !exact || !plain_delimiter(text, &r.span) || shebang(text, h) {
                continue;
            }
            let landing_paths = if c.argv.is_empty() {
                data_loop_landing(script, i, c, &r.span)
            } else {
                data_reader_landing(c)
            };
            // An enclosing compound's redirects (`{ cat <<'EOF'; } > x.sh`)
            // are where the reader's output lands too.
            let landing_paths = landing_paths.and_then(|mut paths| {
                paths.extend(enclosing_landing(script, i, c)?);
                Some(paths)
            });
            if let Some(paths) = landing_paths {
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
    // A landing file placed nowhere (`unplaced_data_landing`): which
    // directories are above it is unknown. Over-approximated from every
    // command of the line (an unplaced target that is not a landing only
    // makes this stricter).
    let landing_unplaced = script.commands.iter().any(|c| {
        c.redirects
            .iter()
            .any(|r| unplaced_data_landing(r).is_some())
    });
    let mut unsure = false;
    for (i, c) in script.commands.iter().enumerate() {
        if !runs_no_landing_file(script, i, c, &landing) {
            return None;
        }
        if !readers.contains(&i) {
            match names_landing_path(c, &landing, landing_unplaced, &script.function_names) {
                Naming::Clear => {}
                Naming::Undetermined => unsure = true,
                // An interpreter running an unseen literal program file
                // (`python3 probe.py <dir>`) may do anything with a path it is
                // handed, but `runs_no_landing_file` above has already shown it
                // is not running the landing file itself. Whether the program
                // uses the body is exactly as unknown as what the command
                // alone does, so the body (not shell code either way) is
                // removed and an Allow floored at Ask — not read as shell.
                Naming::Names if runs_unseen_program_file(c) => unsure = true,
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
/// A heredoc inside a function body is skipped before this by [`data_bodies`]
/// (never data, ruling (a)), so where a call sends its stdout does not matter
/// for a body that is removed: that reader is outside every function body.
///
/// `alias` / `hash` / `enable` / `shopt` / `setopt` / `PATH` are refused for
/// every command by `runs_no_landing_file`, and a landing path named by a body
/// command by [`names_landing_path`].
fn functions_harmless(script: &Script, readers: &[usize]) -> bool {
    let defined = |c: &SimpleCommand| is_defined_call(script, c);
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

/// True when the command word of `c` is a literal name of a function the line
/// defines (a call of that function).
fn is_defined_call(script: &Script, c: &SimpleCommand) -> bool {
    matches!(c.argv.first().map(|w| &w.value),
        Some(Value::Literal(h)) if script.function_names.contains(h))
}

/// Whether a command names a landing path ([`names_landing_path`]).
enum Naming {
    Clear,
    /// Some word is undetermined (`git add $X`): it may or may not name one.
    Undetermined,
    Names,
}

/// Commands that only change directory, create directories, or read / list /
/// print, spelled bare (`heredoc::INERT_LATER_COMMANDS`, plus `cd` and
/// `mkdir`): they may name a landing file or its directory without running
/// it. `mkdir` (backlog 648a6641) only creates directories, and no option or
/// operand of it runs a program, so `mkdir -p d/t && cd d && cat > t/mod.rs
/// <<'EOF'` names the directory above the landing file without being able to
/// run it.
fn reads_only(word: &str) -> bool {
    word == "cd" || word == "mkdir" || INERT_LATER_COMMANDS.contains(&word)
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

/// `git add` / `git commit`, the subcommand directly after `git` (no global
/// option such as `-c` / `-C` / `--exec-path`), every word literal (backlog
/// 1f705907: `cat >> docs/a.md <<'EOF' … EOF` then `git add docs/a.md &&
/// git commit -m …`). They copy what they name into the index / a commit
/// object and run none of it: neither has an option that runs a program or
/// executes a named path. They may therefore name a landing file like a
/// [`reads_only`] tool. RESIDUAL, as for a `git commit -F -` reader: programs
/// git itself runs from existing configuration (hooks, `core.fsmonitor`,
/// clean filters, the editor) are the repository's, not the body's. An
/// undetermined word (`git add "$X"`) is not this shape and is judged by the
/// word rules of [`names_landing_path`].
fn git_records_only(c: &SimpleCommand) -> bool {
    matches!(
        literal_argv(c).as_deref(),
        Ok(["git", "add" | "commit", ..])
    )
}

/// Whether `c` names a landing path (the file, an ancestor directory of it,
/// `.` / `..`) with a literal word ([`Naming::Names`]: the body is kept), may
/// name one with an undetermined word ([`Naming::Undetermined`]: the body is
/// removed and an Allow floored by [`DataView::floor`]), or names none
/// or only reads it ([`reads_only`], [`git_records_only`]). A command not
/// known to only read what it names may run it (`rustc x.rs`, `chmod +x x.rs`,
/// `rg --pre sh . src`). An undetermined or empty command word is
/// [`Naming::Names`]. An empty ARGUMENT word (`sed -i ''`) names no path
/// (backlog 6f299fb5); `/` does. When `landing_unplaced` (some landing file's
/// directory is unknown, [`unplaced_data_landing`]), every other non-empty
/// literal argument may name a directory above it and is
/// [`Naming::Undetermined`] unless it already [`Naming::Names`] one.
fn names_landing_path(
    c: &SimpleCommand,
    landing: &[String],
    landing_unplaced: bool,
    functions: &[String],
) -> Naming {
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
    if !shadowed && (reads_only(head) || cargo_test_or_build(c) || git_records_only(c)) {
        return Naming::Clear;
    }
    let lower: Vec<String> = landing.iter().map(|l| l.to_ascii_lowercase()).collect();
    let mut naming = Naming::Clear;
    for w in &c.argv[1..] {
        match &w.value {
            Value::Undetermined(_) => naming = Naming::Undetermined,
            // The EMPTY word (`sed -i '' …`, BSD sed's no-backup suffix;
            // backlog 6f299fb5) names no path: POSIX path resolution of ""
            // fails (ENOENT) for open / stat / chdir / execve, so it reaches
            // no file. A program that itself maps "" to its cwd (`git -C ''`)
            // operates on what it would reach with the word absent, and a
            // command naming nothing is `Clear` here already. Only a
            // non-empty word that trims to "" (`/`, `//`: the root, above
            // every landing file) names a landing path.
            Value::Literal(s) if s.is_empty() => {}
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
                if landing_unplaced {
                    naming = Naming::Undetermined;
                }
            }
        }
    }
    naming
}

/// True when `c` is an interpreter whose program is a literal FILE operand
/// (`python3 probe.py …`, `bash probe.sh …`): its program text is not in the
/// command. Called only after [`runs_no_landing_file`] accepted `c`, so that
/// file is neither a landing file nor named by another command. Its stdin
/// must be inherited (nothing on the line feeds it): a piped or redirected
/// stdin (`cat cmds.txt | python3 probe.py <dir>`, `< cmds.txt`, `<&N`) may
/// hand the body to the program, so it is not this shape. An inline program
/// (`-c`), a program read from stdin, `eval`, `source` / `.`, a wrapper or an
/// undetermined program is not this shape either.
fn runs_unseen_program_file(c: &SimpleCommand) -> bool {
    matches!(
        program_source(c),
        Some(ProgramSource::File(Value::Literal(_)))
    ) && matches!(c.stdin, StdinSource::Inherited)
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

/// A COMPOUND command (`while IFS= read -r c; do echo "$c"; done <<'EOF'`;
/// `c` is its empty-argv redirection command) whose only commands are the
/// builtins `read` and `echo`, so the here-document is read into a variable
/// and printed. Returns its landing (empty: its OWN stdout is not redirected;
/// where an enclosing compound sends it is [`enclosing_landing`]'s, applied
/// by [`data_bodies`] to every reader), or `None` (the body stays judged as
/// shell) unless ALL hold, each taken from the parsed commands:
///
/// * the compound has no assignment and its only redirect is this
///   here-document on stdin, and its context is the top level or the LAST
///   stage of a pipeline. A non-final pipe stage or a process substitution
///   could feed a shell; a `$( )` hands the output to another word; inside a
///   subshell the parser records only `Subshell`, so whether the subshell is
///   piped on (`( loop ) | bash`) is not known from the command — all keep
///   the body;
/// * every other command inside the compound's span is `read` (literal `-r`
///   and literal [`inert_variable`] names, an `IFS=` prefix at most) or
///   `echo` (no redirect, no assignment), and the name is not a function the
///   line defines. Any other emitted command (`bash`, `eval`, `$c`, `[`, the
///   command of a `$( )`, a redirected nested compound, an assignment-only
///   command, an undetermined command word) keeps the body: what the loop
///   does with the variable cannot then be shown to be data-only. A nested
///   compound WITHOUT a redirect emits no command of its own and is not
///   refused here: `if` / `while` conditions and subshell members are
///   emitted commands and meet the rule above; `[[ ]]` and `(( ))` are
///   evaluated by `ast::analyze`, which refuses to produce a `Known` script
///   at all when arithmetic would evaluate a run-time value such as the read
///   variable (`arith::check_value` / `check_text`: an `Undetermined` value
///   is an error), so `(( c ))` or `[[ $c -eq 1 ]]` never reaches this step;
///   `[[ -n $c ]]` and `(( 1 ))` do, and evaluate nothing from the body;
/// * every `echo` operand is a literal without `$`, backquote or process
///   substitution, or exactly a plain `$NAME` / `${NAME}` (optionally
///   double-quoted): `${c@P}`, `$((c))` or `${!c}` can evaluate the value;
/// * at least one `read` exists, so the body is consumed by something;
/// * no command of the whole line (function bodies included) can make the
///   shell itself evaluate a read value or reroute the loop's output: every
///   command word is literal and not on [`SHELL_STATE_CHANGERS`] (`set -x`
///   expands `PS4` when tracing, `trap … DEBUG` runs code before every
///   command, `exec > >(bash)` reroutes stdout, `declare -n` / `-i` make a
///   read assign elsewhere or evaluate arithmetic, …), has no
///   `program_source` (`source` / `.` / `eval` / an interpreter could do the
///   same), and every assignment, anywhere on the line, is to an
///   [`inert_variable`] name (not `PS4`, `BASH_ENV`, `IFS`, …; the `IFS=`
///   prefix of a `read` in the loop excepted).
///
/// RESIDUAL: shell state inherited from outside the line (an xtrace or trap
/// already set in the Bash tool's shell) is not in the text and is not seen.
fn data_loop_landing(
    script: &Script,
    i: usize,
    c: &SimpleCommand,
    opener: &Range<usize>,
) -> Option<Vec<String>> {
    if !c.assignments.is_empty() {
        return None;
    }
    match c.context {
        Context::TopLevel => {}
        Context::Pipeline { pos, len } if pos + 1 == len => {}
        _ => return None,
    }
    match c.redirects.as_slice() {
        [r] if matches!(r.op, RedirOp::HereDoc)
            && matches!(r.fd, None | Some(0))
            && r.span == *opener => {}
        _ => return None,
    }
    let mut reads = 0usize;
    for (j, k) in script.commands.iter().enumerate() {
        let in_loop = j != i && k.span.start >= c.span.start && k.span.end <= c.span.end;
        if !in_loop {
            if !line_command_inert(k) {
                return None;
            }
            continue;
        }
        if !k.redirects.is_empty() || program_source(k).is_some() {
            return None;
        }
        let Some(Value::Literal(name)) = k.argv.first().map(|w| &w.value) else {
            return None;
        };
        if script.function_names.iter().any(|f| f == name) {
            return None;
        }
        match name.as_str() {
            "read" => {
                let args_ok = k.argv.iter().skip(1).all(
                    |w| matches!(&w.value, Value::Literal(a) if a == "-r" || inert_variable(a)),
                );
                if !args_ok || k.assignments.iter().any(|(n, _)| n != "IFS") {
                    return None;
                }
                reads += 1;
            }
            "echo" => {
                if !k.assignments.is_empty() || !k.argv.iter().skip(1).all(plain_echo_word) {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (reads > 0).then(Vec::new)
}

/// Builtins that change how the shell itself expands, traces, traps, routes
/// output or assigns variables for the REST of the line (bash and zsh). Any
/// of them anywhere on the line keeps the body of a `read` loop
/// ([`data_loop_landing`]).
const SHELL_STATE_CHANGERS: &[&str] = &[
    "set",
    "trap",
    "shopt",
    "setopt",
    "unsetopt",
    "emulate",
    "alias",
    "unalias",
    "enable",
    "disable",
    "builtin",
    "command",
    "exec",
    "declare",
    "typeset",
    "local",
    "export",
    "readonly",
    "integer",
    "float",
    "zmodload",
    "autoload",
    "functions",
    "bindkey",
    "zle",
];

/// True when `k`, a command of the line outside the loop, cannot change the
/// shell's state in a way that makes it evaluate a read value or reroute the
/// loop's output (see [`data_loop_landing`]). An empty-argv command (a
/// compound's redirects; an assignment-only command) has only its
/// assignments checked: where a compound sends its output is judged by
/// [`enclosing_landing`].
fn line_command_inert(k: &SimpleCommand) -> bool {
    if !k.assignments.iter().all(|(n, _)| inert_variable(n)) {
        return false;
    }
    let Some(first) = k.argv.first() else {
        return true;
    };
    let Value::Literal(word) = &first.value else {
        return false;
    };
    !SHELL_STATE_CHANGERS.contains(&word.as_str()) && program_source(k).is_none()
}

/// Lower-case parameters with a special meaning in zsh (`zshparam(1)`, the
/// `zsh/parameter`, `zsh/mapfile`, `zsh/system`, `zsh/terminfo` modules).
/// Assigning one changes the shell (`path` is `PATH`, `mapfile` writes files,
/// `precmd_functions` names functions to run). Every special parameter of
/// bash is upper-case or punctuation, which [`inert_variable`] refuses.
const ZSH_LOWER_SPECIALS: &[&str] = &[
    "argv",
    "status",
    "pipestatus",
    "path",
    "fpath",
    "cdpath",
    "manpath",
    "mailpath",
    "module_path",
    "psvar",
    "prompt",
    "histchars",
    "signals",
    "watch",
    "options",
    "parameters",
    "commands",
    "functions",
    "functions_source",
    "aliases",
    "galiases",
    "saliases",
    "builtins",
    "dis_aliases",
    "dis_builtins",
    "dis_functions",
    "dis_galiases",
    "dis_reswords",
    "dis_saliases",
    "jobstates",
    "jobtexts",
    "jobdirs",
    "nameddirs",
    "userdirs",
    "usergroups",
    "reswords",
    "modules",
    "dirstack",
    "history",
    "historywords",
    "widgets",
    "keymaps",
    "terminfo",
    "termcap",
    "errnos",
    "sysparams",
    "funcstack",
    "funcfiletrace",
    "funcsourcetrace",
    "functrace",
    "mapfile",
    "epochtime",
    "epochrealtime",
    "patchars",
    "dis_patchars",
];

/// A variable name that is a plain lower-case identifier (`[a-z_][a-z0-9_]*`)
/// with no special meaning to bash or zsh: not `_`, not on
/// [`ZSH_LOWER_SPECIALS`], not a zsh hook array (`*_functions`) or `zsh_*`
/// parameter. An allowlist: an upper-case name (`PS4`, `IFS`, `BASH_ENV`,
/// `PROMPT_COMMAND`, `SHELLOPTS`, …) is refused whatever it is.
fn inert_variable(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && s != "_"
        && !ZSH_LOWER_SPECIALS.contains(&s)
        && !s.ends_with("_functions")
        && !s.starts_with("zsh_")
}

/// The landing of the output of the reader `c` (command `i` of `script`)
/// through every COMPOUND command that encloses it (`{ …; } > f`,
/// `( … ) > f`, `done > f`: an empty-argv command whose span contains the
/// reader's), by [`stdout_landing`] of each: their literal data / code files,
/// or `None` when one sends the output to anything else (`} > x.sh`, a
/// non-final pipe stage, a process substitution, an undetermined target).
/// A compound's own redirects are not the reader's, so without this a body
/// printed to an enclosing `> x.sh` would count as printed to the terminal.
fn enclosing_landing(script: &Script, i: usize, c: &SimpleCommand) -> Option<Vec<String>> {
    let mut names = Vec::new();
    for (j, k) in script.commands.iter().enumerate() {
        let encloses = j != i
            && k.argv.is_empty()
            && !k.redirects.is_empty()
            && k.span.start <= c.span.start
            && k.span.end >= c.span.end;
        if encloses {
            names.extend(stdout_landing(k)?);
        }
    }
    Some(names)
}

fn is_identifier(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// An `echo` operand that only prints: not an option, and its raw text is a
/// literal with no `$`, backquote or process substitution, or exactly a plain
/// parameter reference `$NAME` / `${NAME}`, optionally in double quotes.
fn plain_echo_word(w: &Word) -> bool {
    let raw = w.raw.as_str();
    if raw.starts_with('-') {
        return false;
    }
    let inner = raw
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(raw);
    let param = inner.strip_prefix('$').is_some_and(|n| {
        is_identifier(n)
            || n.strip_prefix('{')
                .and_then(|n| n.strip_suffix('}'))
                .is_some_and(is_identifier)
    });
    param
        || (!raw.contains(['$', '`']) && !raw.contains("<(") && !raw.contains(">("))
        || double_quoted_plain_params(raw)
}

/// True when `raw` is one double-quoted word whose interior is only literal
/// text (no backslash, quote, backquote or other `$` form) and plain
/// `$NAME` / `${NAME}` references (`"$a $b"`, backlog cfa4e0f2). Each such
/// reference only prints its value; none can evaluate it (`${c@P}`, `$((c))`,
/// `${!c}`, `$(…)` are refused because the name must be a bare identifier).
fn double_quoted_plain_params(raw: &str) -> bool {
    let Some(inner) = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return false;
    };
    let mut rest = inner;
    while let Some(c) = rest.chars().next() {
        if c == '$' {
            let after = &rest[1..];
            let (name, tail) = if let Some(b) = after.strip_prefix('{') {
                let Some(end) = b.find('}') else {
                    return false;
                };
                (&b[..end], &b[end + 1..])
            } else {
                let end = after
                    .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                    .unwrap_or(after.len());
                (&after[..end], &after[end..])
            };
            if !is_identifier(name) {
                return false;
            }
            rest = tail;
        } else if matches!(c, '`' | '"' | '\\') {
            return false;
        } else {
            rest = &rest[c.len_utf8()..];
        }
    }
    true
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
            _ => names.push(unplaced_data_landing(r)?.to_string()),
        }
    }
    Some(names)
}

/// A RELATIVE literal data / code file target (`>> docs/a.md`) that the
/// parser could not place because an undetermined `cd` runs before it
/// (`cd "$WT" && cat >> docs/a.md <<'EOF'`, backlog 1f705907): the target is
/// `Path(Undetermined)`, but its text ([`ast::Redirect::written`]) — and so
/// what KIND of file the body lands in — is known. Only WHERE it is is not:
/// [`data_bodies`] then counts every literal word of a command that is not
/// known to only read or record what it names as possibly naming a directory
/// above it ([`Naming::Undetermined`], an Allow floored at Ask), since which
/// directories are above it cannot be compared. An absolute target, an
/// undetermined word, or a target that is not a data / code file is `None`
/// (the body is kept).
fn unplaced_data_landing(r: &ast::Redirect) -> Option<&str> {
    match (&r.target, &r.written) {
        (RedirTarget::Path(Value::Undetermined(_)), Some(Value::Literal(p)))
            if !p.is_empty() && !p.starts_with(['/', '~']) && data_or_code_file(p) =>
        {
            Some(p)
        }
        _ => None,
    }
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

/// Backlog 87661417 (user ruling 2026-11-17: a quoted here-document body is
/// not shell lines whatever commands come before it): for
/// `heredoc::python_file_here_document` only, where the reader of the ONE
/// quoted here-document starts on its opener line when commands that the
/// string shape rule does not peel come before it (`true && cat >> x.py
/// <<'X'`, `cd /w && sed -e 's/a/b/' f > x.py && cat >> x.py <<'X'`). `heads`
/// is the byte range of the opener line not already peeled (from the first
/// unpeeled byte to the end of the line, newline excluded). Answers the byte
/// offset in `cmd` where the reader starts and the byte offset where the
/// parser ends the body (the start of the terminator line), or `None` — the
/// caller then keeps the body judged as shell lines, exactly as before.
///
/// The body is NOT removed here: the caller judges it as Python and the rest
/// of the command, the peeled commands included, with every shell rule. This
/// only decides which command reads the body, and that no command before the
/// reader can run it. Every condition is read from the parser
/// ([`ast::analyze`]; a line that defines functions is not `Known` there):
///
/// * `cmd` fits [`ast::MAX_PARSE_BYTES`]; exactly one here-document exists in
///   the whole command, QUOTED, not `<<-`, its body span equal to the parsed
///   body byte for byte, with a plain delimiter word, and its body starts on
///   the line after `heads`; the text outside that body passes
///   [`plain_outside`];
/// * its reader is a literal `cat` / `tee` at the top level (not a pipe stage,
///   a background job, a subshell or a substitution) that starts inside
///   `heads`;
/// * every other command that starts at or after `heads.start` and before the
///   body's end lies wholly in `heads` before the reader, and is
///   [`inert_opener_head`] (at least one such command); the text of `heads`
///   before the reader that is not inside one of those commands is only
///   spaces, tabs, `;` and `&` (so `&&` and `;` lists only: no `while` / `do`
///   / `{` / `(` / `!` / `|` / `||` — a loop around the reader could run what
///   an earlier iteration wrote).
///
/// Commands before the reader run once and finish before it starts, and an
/// inert head runs no program, so none of them can run the file the body
/// lands in. Lines after the close are judged as for the unwidened split
/// (`interpreter::python_file_body_verdict` reads them).
pub(super) fn python_file_reader_start(cmd: &str, heads: Range<usize>) -> Option<(usize, usize)> {
    if cmd.len() > ast::MAX_PARSE_BYTES {
        return None;
    }
    let Analysis::Known(script) = ast::analyze(cmd) else {
        return None;
    };
    let docs: Vec<(usize, &ast::HereDoc, &ast::Redirect)> = script
        .commands
        .iter()
        .enumerate()
        .flat_map(|(i, c)| {
            c.redirects.iter().filter_map(move |r| match &r.target {
                RedirTarget::HereDoc(h) => Some((i, h, r)),
                _ => None,
            })
        })
        .collect();
    let [(reader_ix, h, opener)] = docs.as_slice() else {
        return None;
    };
    let exact =
        matches!(&h.body, Value::Literal(v) if cmd.get(h.body_span.clone()) == Some(v.as_str()));
    if !h.quoted
        || h.strip_tabs
        || !exact
        || !plain_delimiter(cmd, &opener.span)
        || h.body_span.start != heads.end + 1
        || !plain_outside(
            cmd,
            std::slice::from_ref(&h.body_span),
            &single_quoted_interiors(&script, cmd),
        )
    {
        return None;
    }
    let reader = script.commands.get(*reader_ix)?;
    let reader_word = literal_argv(reader).ok()?.first().copied();
    if reader.context != Context::TopLevel
        || !matches!(reader_word, Some("cat" | "tee"))
        || !(heads.start < reader.span.start && reader.span.start < heads.end)
    {
        return None;
    }
    let mut covered: Vec<Range<usize>> = Vec::new();
    for (i, c) in script.commands.iter().enumerate() {
        if i == *reader_ix || c.span.end <= heads.start || c.span.start >= h.body_span.end {
            continue;
        }
        let before_reader = heads.start <= c.span.start && c.span.end <= reader.span.start;
        if !(before_reader && inert_opener_head(c)) {
            return None;
        }
        covered.push(c.span.clone());
    }
    if covered.is_empty() {
        return None;
    }
    let gap_ok = cmd
        .get(heads.start..reader.span.start)?
        .char_indices()
        .all(|(k, ch)| {
            matches!(ch, ' ' | '\t' | ';' | '&')
                || covered.iter().any(|s| s.contains(&(heads.start + k)))
        });
    gap_ok.then_some((reader.span.start, h.body_span.end))
}

/// A command before a quoted `.py` here-document reader on its opener line
/// that cannot run a program ([`python_file_reader_start`]): at the top level,
/// no assignment, no `program_source`, every word literal, the command word
/// spelled bare, and either on `heredoc::INERT_LATER_COMMANDS` (read-only
/// tools with no option that runs a program) or a `sed` that only substitutes
/// ([`sed_substitutes_only`]). Its redirects are only output redirects to a
/// literal path (`> f`, `>> f`, `&> f`; the target is still judged by every
/// redirect rule, the command staying in the text) or an fd duplication onto
/// a numbered fd (`2>&1`). An allowlist: `cd` (it would move where the body's
/// relative paths are placed), `find -exec`, `xargs`, `awk`, `make`, an
/// unknown word — anything else is not inert.
fn inert_opener_head(c: &SimpleCommand) -> bool {
    if c.context != Context::TopLevel || !c.assignments.is_empty() || program_source(c).is_some() {
        return false;
    }
    let Ok(argv) = literal_argv(c) else {
        return false;
    };
    let Some((&name, args)) = argv.split_first() else {
        return false;
    };
    let redirects_ok = c.redirects.iter().all(|r| match r.op {
        RedirOp::Out
        | RedirOp::Append
        | RedirOp::Clobber
        | RedirOp::OutErr
        | RedirOp::AppendOutErr => matches!(r.target, RedirTarget::Path(Value::Literal(_))),
        RedirOp::DupOut => matches!(r.target, RedirTarget::Fd(FdTarget::Number(_))),
        _ => false,
    });
    redirects_ok
        && (INERT_LATER_COMMANDS.contains(&name) || (name == "sed" && sed_substitutes_only(args)))
}

/// True when the `sed` operands `args` give it only `s` commands with no flag
/// that runs or writes anything: options only `-n`, `-E`, `-r` and `-e
/// <script>`; with no `-e`, the first operand is the script; every other
/// operand is a file (not starting with `-`). Each script is ONE `s` command
/// ([`single_plain_substitution`]). GNU sed's `e` command / `s///e` flag runs
/// a shell command and `w` writes a file, so any other script is not this
/// shape.
fn sed_substitutes_only(args: &[&str]) -> bool {
    let mut scripts: Vec<&str> = Vec::new();
    let mut operands: Vec<&str> = Vec::new();
    let mut i = 0;
    while let Some(&a) = args.get(i) {
        match a {
            "-n" | "-E" | "-r" => {}
            "-e" => match args.get(i + 1) {
                Some(s) => {
                    scripts.push(s);
                    i += 1;
                }
                None => return false,
            },
            a if a.starts_with('-') => return false,
            a => operands.push(a),
        }
        i += 1;
    }
    if scripts.is_empty() {
        if operands.is_empty() {
            return false;
        }
        scripts.push(operands.remove(0));
    }
    scripts.iter().all(|s| single_plain_substitution(s))
}

/// True when `script` is exactly one sed `s<d>regex<d>replacement<d>flags`
/// command: `<d>` an ASCII punctuation character other than `\`, a `\`
/// escaping the next character, no newline anywhere, and the flags only `g`,
/// `p`, `I` and digits (never `e`, `w`, `m`, `M`, nor a second command).
fn single_plain_substitution(script: &str) -> bool {
    let mut chars = script.chars();
    if chars.next() != Some('s') {
        return false;
    }
    let Some(delim) = chars.next() else {
        return false;
    };
    if !delim.is_ascii_punctuation() || delim == '\\' {
        return false;
    }
    let mut seen = 0;
    while seen < 2 {
        match chars.next() {
            None | Some('\n') => return false,
            Some('\\') => {
                if matches!(chars.next(), None | Some('\n')) {
                    return false;
                }
            }
            Some(c) if c == delim => seen += 1,
            Some(_) => {}
        }
    }
    chars.all(|c| matches!(c, 'g' | 'p' | 'I') || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::without_data_heredoc_bodies;
    use crate::detect::detect;
    use crate::model::Decision;

    fn bash(cmd: &str) -> Decision {
        detect("Bash", Some(&serde_json::json!({ "command": cmd })))
    }

    fn line(landing: &str, body: &str, later: &str) -> String {
        format!("cat > {landing} <<'EOF'\n{body}EOF\n{later}\n")
    }

    const SHELLISH: &str = "echo hi >&$fd\nprobe 3<<< x\n";

    /// An interpreter of an unseen literal program file that names the
    /// landing file or a directory above it: the body is removed (never read
    /// as shell) and the view is marked unsure, so an Allow is floored at Ask.
    #[test]
    fn heredoc_data_unseen_program_file_naming_landing_is_floored() {
        for later in [
            "python3 /w/probe.py /w",
            "python3 /w/probe.py /w/",
            "python3 /w/probe.py /w/cmds.txt",
            "python3 /w/probe.py cmds.txt",
            "python3 /w/probe.py .",
            "bash /w/probe.sh /w",
        ] {
            let cmd = line("/w/cmds.txt", SHELLISH, later);
            let view = without_data_heredoc_bodies(&cmd);
            assert!(view.is_some(), "body kept for `{later}`");
            let Some(view) = view else { continue };
            assert!(!view.text.contains("hi >&"), "{later}: {}", view.text);
            assert!(view.unsure, "{later}: not floored");
            let floored = view.floor(Decision::Allow, || Decision::ask("whole"));
            assert!(matches!(floored, Decision::Ask(_)), "{later}: {floored:?}");
        }
    }

    /// A command that can run the landing file (or a body by another route)
    /// keeps every body.
    #[test]
    fn heredoc_data_runner_of_landing_file_keeps_body() {
        for (landing, later) in [
            ("/w/cmds.txt", "bash /w/cmds.txt"),
            ("/w/cmds.txt", "sh /w/cmds.txt"),
            ("/w/cmds.txt", "python3 /w/cmds.txt"),
            ("/w/cmds.txt", ". /w/cmds.txt"),
            ("/w/cmds.txt", "source /w/cmds.txt"),
            ("/w/cmds.txt", "/w/cmds.txt"),
            ("cmds.txt", "./cmds.txt"),
            ("/w/cmds.txt", "sh -c 'echo hi' /w"),
            ("/w/cmds.txt", "python3 -c 'pass' /w"),
            ("/w/cmds.txt", "cat /w/cmds.txt | python3"),
            ("/w/cmds.txt", "cat /w/cmds.txt | python3 /w/probe.py /w"),
            ("/w/cmds.txt", "python3 /w/probe.py /w < /w/cmds.txt"),
            ("/w/cmds.txt", "eval echo /w"),
            ("/w/cmds.txt", "env python3 /w/probe.py /w"),
            ("/w/cmds.txt", "xargs python3 /w/probe.py /w"),
            ("/w/cmds.txt", "bash -s /w < /w/cmds.txt"),
            (
                "/w/cmds.txt",
                "python3 /w/probe.py /w; cp /w/cmds.txt /w/probe.py",
            ),
        ] {
            let cmd = line(landing, SHELLISH, later);
            assert!(
                without_data_heredoc_bodies(&cmd).is_none(),
                "body removed for `{later}`"
            );
        }
    }

    /// A system-harm body stays Deny where the landing file can be run, and a
    /// `.txt` body under an unseen-program interpreter is never Allow.
    #[test]
    fn heredoc_data_system_harm_body_verdicts() {
        let harm = "rm -rf /\n";
        for later in [
            "bash /w/cmds.txt",
            "sh /w/cmds.txt",
            ". /w/cmds.txt",
            "/w/cmds.txt",
            "eval \"$(cat /w/cmds.txt)\"",
            "cat /w/cmds.txt | sh",
            "env bash /w/cmds.txt",
        ] {
            let d = bash(&line("/w/cmds.txt", harm, later));
            assert!(d.is_deny(), "`{later}`: {d:?}");
        }
        let d = bash(&line("/w/cmds.txt", harm, "python3 /w/probe.py /w"));
        assert!(d.is_blocking(), "{d:?}");
    }
}
