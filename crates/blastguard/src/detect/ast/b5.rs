//! Stage S4 `stdin-program`: the standalone Table B.5 layer (`SUBSET.md`,
//! user rulings 2026-11-18).
//!
//! [`raise`] combines this layer's opinion with the legacy verdict as
//! `max(B.5, legacy)` under `Deny > Ask > Allow`: it never lowers a legacy
//! Deny or Ask. An `Allow` from this layer means "no B.5 objection", never
//! "safe"; it cannot lower anything.
//!
//! # What the layer decides
//!
//! It reads the AST ([`super::analyze`]) of the command. A whole-line
//! `Analysis::Undetermined` (syntax outside the subset, too long, too deep)
//! gives NO opinion: the legacy layer decides (ruling 2026-11-18 (b)). For a
//! `Known` script, every simple command (including those in `$( )`,
//! pipelines and background jobs) is classified, after unwrapping `env`
//! (also `env -i`, `NAME=V` operands), `nohup`, `timeout`, `nice`, `time`,
//! `exec` and `command`:
//!
//! * an interpreter (bash / sh / zsh / dash / ksh / python* / perl / ruby /
//!   node) running a program FILE → Undetermined (rulings 10, 2026-11-18 #2);
//!   the reason names the program file, never a later (data) operand;
//! * perl / ruby / node `-e` (any inline program) → Undetermined;
//! * a LITERAL program body — a `StdinSource::Literal` stdin, or the literal
//!   output of a process substitution (`bash <(echo hi)`, `bash < <(echo
//!   hi)`) or of the previous pipe stage (`printf '%s' x | node`) whose
//!   commands are all `echo` / `printf` literal producers (user ruling
//!   2026-11-18 (a)): a shell body is analysed by this layer recursively;
//!   a python / perl / ruby / node body gives no B.5 opinion (the legacy
//!   program-text reading judges it);
//! * any other program from stdin or a process substitution (`bash < x.sh`,
//!   a pipe from a non-literal producer, an fd, `<(cat f)`) → Undetermined,
//!   except a bare shell reading the inherited stdin, which gives no opinion
//!   (the Bash tool's stdin is empty; user ruling 2026-11-18 (b));
//! * a shell `-c PROG` with a literal PROG: PROG analysed by this layer
//!   recursively (the legacy layer judges its effects); an undetermined PROG
//!   → Undetermined;
//! * python: a program file and `-m` → Undetermined (S5 reads inline
//!   programs only); `-c` and a literal stdin program give no B.5 opinion —
//!   stage S5 (`s5.rs`) parses and judges them; any other stdin →
//!   Undetermined;
//! * `eval` with operands → Undetermined;
//! * an undetermined command word (after unwrapping) → Undetermined (Table B
//!   common rules: it may be an interpreter);
//! * direct execution of a path (`./x`, `rel/dir/x`, `target/debug/x`,
//!   `.venv/bin/x`, an absolute path whose directory is not one of
//!   [`SYSTEM_BIN_DIRS`]) → Undetermined (ruling 17); a path in one of
//!   [`SYSTEM_BIN_DIRS`] is classified by its basename like the bare name;
//! * an option of an interpreter or of an unwrapped wrapper that is not
//!   modelled → Undetermined, except an interpreter's lone version / help
//!   flag (`python3 --version`), which runs no program;
//! * everything else (bare names such as `make` / `cargo` / unknown tools,
//!   `source` / `.` — ruling 16, kept legacy) → no opinion.
//!
//! Exceptions to the program-file rows above (user rulings 2026-11-18, T1 /
//! T3):
//!
//! * **T1 — tracked and unmodified.** An interpreter file run or a direct
//!   execution, in the top-level text (not inside a `-c` body), whose
//!   program file git observes as tracked and clean ([`git_tracked_clean`]:
//!   inside the repository of the hook's cwd, plain `H` index entry, no
//!   staged / unstaged change, real path inside the work tree) AND that no
//!   other part of the line may write ([`tracked_unmodified`]) gets no B.5
//!   opinion. perl / ruby / node need the file as their first argument;
//!   `-m` stays Undetermined. Setting `PATH`, `ENV`, `IFS`, `PS4`,
//!   `SHELLOPTS` or a `PYTHON*` / `NODE*` / `PERL*` / `RUBY*` / `BASH*` /
//!   `LD_*` / `DYLD_*` variable anywhere on the line withholds it. Without a
//!   session cwd and resolver (every library consumer) git is never asked
//!   and the file run stays Undetermined.
//! * **T3 — noexec.** A shell whose options before the operand are only
//!   `-n` clusters (with `x` / `v` / `e` / `u`) gets no B.5 opinion;
//!   `-ni`, `-n -c BODY` (BODY judged as if run) and `x.sh -n` (a script
//!   argument) are read as usual.
//!
//! A program read from a file descriptor (`bash <&'$fd'`) is Undetermined
//! (ruling 2026-11-18 T2), through the stdin row above.
//!
//! Undetermined is an `Ask` here; the hook hardens it to Deny for a subagent
//! and every library consumer that calls `Decision::hardened`.
//!
//! # Written-then-run exception (ruling 2026-11-18 #5)
//!
//! When a shell runs a file (`bash x.sh`, `sh x.sh`) or the file is executed
//! directly (`./x.sh`), and an earlier top-level command of the same line
//! writes that file's whole body with a determined text (`cat > x.sh <<'EOF'`,
//! `cat > x.sh <<< lit`, `echo lit > x.sh`, `printf 'lit' > x.sh`; same
//! resolved path), and the writer, the run and everything between them
//! (only `chmod` may sit between) are joined by `&&` only, with the writer
//! itself at the start of a list or reached by `&&` only, the written body is
//! judged as the program by the full detector (`judge`), as
//! `<shell> -c '<body>'` — so a body run by `sh` gets exactly the reading
//! `sh -c` gets (ruling #7). A directly executed body uses its shebang's
//! shell, `sh` without a shebang; a non-shell shebang stays Undetermined.
//! Any other join (`;`, newline, `||`, a pipe), any other intervening
//! command, an append, or a writer this module does not model →
//! Undetermined. A python / perl / ruby / node file written then run stays
//! Undetermined (S5).
//!
//! # Known gaps (not decided here; the legacy layer alone judges them)
//!
//! Wrappers other than the ones unwrapped above (`sudo`, `xargs`, `su`,
//! `flock`, `find -exec`, …) and the programs they run; `BASH_ENV` / `ENV`
//! assignments.

use std::cell::Cell;

use super::{
    analyze, program_source, Analysis, Context, ProgramSource, RedirOp, RedirTarget, SimpleCommand,
    StdinSource, Value, Word,
};
use crate::detect::VerdictAcc;
use crate::model::Decision;

/// Directories whose binaries are judged by basename like the bare command
/// (ruling 2026-11-18 #3). Only the exact directory counts, not a
/// subdirectory of it.
pub(crate) const SYSTEM_BIN_DIRS: &[&str] = &[
    "/bin",
    "/sbin",
    "/usr/bin",
    "/usr/sbin",
    "/usr/libexec",
    "/usr/local/bin",
    "/usr/local/sbin",
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
    "/opt/local/bin",
    "/opt/local/sbin",
    "/nix/var/nix/profiles/default/bin",
    "/run/current-system/sw/bin",
    "/snap/bin",
];

/// Nesting cap of `-c` / stdin recursion inside this layer and of the
/// written-body judgements; deeper → Undetermined.
const MAX_DEPTH: usize = 8;

thread_local! {
    /// How many written-body judgements (`judge` calls) are active.
    static JUDGE_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// What the layer needs from its caller.
pub(crate) struct Probes<'a> {
    /// The full detector for a command text (used only for written-then-run
    /// bodies).
    pub judge: &'a dyn Fn(&str) -> Decision,
    /// The session's location model: the hook's cwd and the path resolver
    /// the tracked-and-unmodified probe ([`git_tracked_clean`]) needs.
    pub scope: &'a crate::scope::SafeRoots,
}

/// `max(B.5, legacy)` (`Deny > Ask > Allow`). A legacy Deny is returned
/// unchanged without consulting this layer; on equal rank the legacy verdict
/// (and its reason) is kept.
pub(crate) fn raise(legacy: Decision, command: &str, judge: &Probes<'_>) -> Decision {
    if legacy.is_deny() {
        return legacy;
    }
    match layer(command, 0, judge) {
        d @ Decision::Deny(_) => d,
        d @ Decision::Ask(_) if matches!(legacy, Decision::Allow) => d,
        _ => legacy,
    }
}

fn undetermined(reason: impl std::fmt::Display) -> Decision {
    Decision::ask(format!(
        "{reason} — blastguard cannot judge what it would do (Table B.5: Undetermined)"
    ))
}

/// This layer's opinion of `text` (`Allow` = no objection).
fn layer(text: &str, depth: usize, judge: &Probes<'_>) -> Decision {
    if depth > MAX_DEPTH {
        return undetermined("the program is nested too deeply");
    }
    let script = match analyze(text) {
        Analysis::Known(s) => s,
        // A parse failure is not a B.5 shape: no opinion, legacy decides.
        Analysis::Undetermined(_) => return Decision::Allow,
    };
    let mut acc = VerdictAcc::default();
    for (i, cmd) in script.commands.iter().enumerate() {
        let v = classify(text, &script.commands, i, cmd, depth, judge);
        if let Some(deny) = acc.record(v) {
            return deny;
        }
    }
    acc.finish()
}

/// A word's value: `Ok(literal)`, or `Err(reason)` when it is undetermined.
/// Every caller resolves `Err` to its restrictive side (not a modelled
/// option / writer / producer → Undetermined, or an Undetermined verdict).
pub(super) fn literal(w: &Word) -> Result<&str, &str> {
    match &w.value {
        Value::Literal(s) => Ok(s.as_str()),
        Value::Undetermined(r) => Err(r.as_str()),
    }
}

/// Every word of `argv` as a literal; `Err(reason)` of the first one that is
/// undetermined.
fn literals(argv: &[Word]) -> Result<Vec<&str>, &str> {
    argv.iter().map(literal).collect()
}

pub(super) fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

/// The directory part of a path (`None` without a slash).
pub(super) fn dirname(s: &str) -> Option<&str> {
    let i = s.rfind('/')?;
    Some(if i == 0 {
        "/"
    } else {
        s.get(..i).unwrap_or("")
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Family {
    Shell,
    Python,
    PerlLike,
    Node,
}

pub(super) fn family(base: &str) -> Option<Family> {
    match base {
        "bash" | "sh" | "zsh" | "dash" | "ksh" => Some(Family::Shell),
        "perl" | "ruby" => Some(Family::PerlLike),
        "node" | "nodejs" => Some(Family::Node),
        b if b
            .strip_prefix("python")
            .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.')) =>
        {
            Some(Family::Python)
        }
        _ => None,
    }
}

/// A lone flag that only prints a version / help and runs no program.
fn info_only(fam: Family, args: &[Word]) -> bool {
    let [only] = args else { return false };
    let Ok(f) = literal(only) else { return false };
    match fam {
        Family::Shell => matches!(f, "--version" | "--help"),
        Family::Python => matches!(f, "--version" | "-V" | "-VV" | "--help" | "-h"),
        Family::PerlLike => matches!(f, "-v" | "-V" | "--version" | "-h" | "--help"),
        Family::Node => matches!(f, "-v" | "--version" | "-h" | "--help"),
    }
}

/// Where unwrapping a wrapper leaves the command.
pub(super) enum Unwrapped {
    /// The wrapped command starts at this argv index.
    At(usize),
    /// Nothing is run (`env` alone, `command -v x`).
    Nothing,
    /// A wrapper option not modelled.
    Undetermined(String),
    /// `argv[k]` is not an unwrapped wrapper.
    NotWrapper,
}

/// `argv[k]` as an unwrapped wrapper (see the module doc for the set).
pub(super) fn unwrap_at(argv: &[Word], k: usize) -> Unwrapped {
    let Some(Ok(name)) = argv.get(k).map(literal) else {
        return Unwrapped::NotWrapper;
    };
    let base = basename(name);
    let word = |i: usize| argv.get(i).map(literal);
    let mut i = k + 1;
    let undet = |o: &str| Unwrapped::Undetermined(format!("`{base}` option `{o}` is not modelled"));
    match base {
        "env" => loop {
            match word(i) {
                None => return Unwrapped::Nothing,
                Some(Err(_)) => {
                    return Unwrapped::Undetermined("an `env` operand is undetermined".into())
                }
                Some(Ok(w)) => match w {
                    "-i" | "-" | "--ignore-environment" | "-0" | "--null" | "-v" | "--debug" => {
                        i += 1
                    }
                    "-u" | "--unset" => i += 2,
                    "--" => return Unwrapped::At(i + 1),
                    w if w.starts_with("--unset=") || (w.starts_with("-u") && w.len() > 2) => {
                        i += 1
                    }
                    w if w.starts_with('-') => return undet(w),
                    w if w.contains('=') => i += 1,
                    _ => return Unwrapped::At(i),
                },
            }
        },
        "nohup" => match word(i) {
            Some(Ok("--")) => Unwrapped::At(i + 1),
            Some(Ok(w)) if w.starts_with('-') => undet(w),
            _ => Unwrapped::At(i),
        },
        "timeout" => {
            loop {
                match word(i) {
                    Some(Ok("--")) => {
                        i += 1;
                        break;
                    }
                    Some(Ok(
                        "--foreground" | "--preserve-status" | "-v" | "--verbose" | "-f" | "-p",
                    )) => i += 1,
                    Some(Ok("-s" | "-k" | "--signal" | "--kill-after")) => i += 2,
                    Some(Ok(w))
                        if w.starts_with("--signal=")
                            || w.starts_with("--kill-after=")
                            || ((w.starts_with("-s") || w.starts_with("-k")) && w.len() > 2) =>
                    {
                        i += 1
                    }
                    Some(Ok(w)) if w.starts_with('-') => return undet(w),
                    Some(Err(_)) => {
                        return Unwrapped::Undetermined(
                            "a `timeout` operand is undetermined".into(),
                        )
                    }
                    _ => break,
                }
            }
            // DURATION, then the command.
            Unwrapped::At(i + 1)
        }
        "nice" => loop {
            match word(i) {
                Some(Ok("-n" | "--adjustment")) => i += 2,
                Some(Ok("--")) => return Unwrapped::At(i + 1),
                Some(Ok(w))
                    if w.starts_with("--adjustment=")
                        || (w.starts_with("-n") && w.len() > 2)
                        || (w.len() > 1
                            && w.starts_with('-')
                            && w[1..].chars().all(|c| c.is_ascii_digit())) =>
                {
                    i += 1
                }
                Some(Ok(w)) if w.starts_with('-') => return undet(w),
                _ => return Unwrapped::At(i),
            }
        },
        "time" => match word(i) {
            Some(Ok("-p")) => Unwrapped::At(i + 1),
            Some(Ok(w)) if w.starts_with('-') => undet(w),
            _ => Unwrapped::At(i),
        },
        "exec" => loop {
            match word(i) {
                Some(Ok("-a")) => i += 2,
                Some(Ok("-c" | "-l" | "-cl" | "-lc")) => i += 1,
                Some(Ok("--")) => return Unwrapped::At(i + 1),
                Some(Ok(w)) if w.starts_with('-') => return undet(w),
                _ => return Unwrapped::At(i),
            }
        },
        "command" => loop {
            match word(i) {
                Some(Ok("-p")) => i += 1,
                Some(Ok("-v" | "-V")) => return Unwrapped::Nothing,
                Some(Ok("--")) => return Unwrapped::At(i + 1),
                Some(Ok(w)) if w.starts_with('-') => return undet(w),
                _ => return Unwrapped::At(i),
            }
        },
        _ => Unwrapped::NotWrapper,
    }
}

/// The B.5 opinion of one simple command (`commands[idx]`).
fn classify(
    text: &str,
    commands: &[SimpleCommand],
    idx: usize,
    cmd: &SimpleCommand,
    depth: usize,
    judge: &Probes<'_>,
) -> Decision {
    let mut k = 0;
    loop {
        match unwrap_at(&cmd.argv, k) {
            Unwrapped::NotWrapper => break,
            Unwrapped::Nothing => return Decision::Allow,
            Unwrapped::Undetermined(r) => return undetermined(r),
            Unwrapped::At(next) => k = next,
        }
    }
    let Some(first) = cmd.argv.get(k) else {
        return Decision::Allow;
    };
    // An undetermined command word may be an interpreter (Table B common
    // rules): Undetermined.
    let name = match literal(first) {
        Ok(name) => name,
        Err(r) => {
            return undetermined(format_args!(
                "the command word is undetermined ({r}) and may run a program"
            ))
        }
    };
    if name.contains('/') && !dirname(name).is_some_and(|d| SYSTEM_BIN_DIRS.contains(&d)) {
        let target = super::eval::resolve_at(&cmd.cwd, Value::Literal(name.to_string()));
        if let Some(d) = written_then_run(text, commands, idx, &target, Runner::Direct, judge) {
            return d;
        }
        // Ruling 2026-11-18 T1: a git-tracked, unmodified program file gets
        // no B.5 opinion (the legacy verdict stands).
        if depth == 0 && tracked_unmodified(text, commands, idx, &target, judge.scope) {
            return Decision::Allow;
        }
        return undetermined(format_args!(
            "direct execution of the program file `{name}`, whose text is not in the command"
        ));
    }
    let base = basename(name);
    let args = cmd.argv.get(k + 1..).unwrap_or(&[]);
    let Some(fam) = family(base) else {
        return match base {
            "eval" if !args.is_empty() => undetermined("`eval` runs shell code built at run time"),
            _ => Decision::Allow,
        };
    };
    if info_only(fam, args) {
        return Decision::Allow;
    }
    // Ruling 2026-11-18 T3: a noexec syntax check runs no program.
    if fam == Family::Shell && noexec_only(args) {
        return Decision::Allow;
    }
    // A shell given `-c` and nothing else runs no program: it fails with
    // "-c: option requires an argument", exit 2, even with a piped stdin
    // (observed 2026-11-19, relay bgr-41bb2e87c9a7). No B.5 opinion (the
    // legacy verdict stands). Only this exact shape is covered; any other
    // `-c` spelling without an operand, and zsh / dash / ksh (not observed),
    // stay Undetermined.
    if matches!(base, "bash" | "sh") && dash_c_alone(args) {
        return Decision::Allow;
    }
    let inner = SimpleCommand {
        argv: cmd.argv.get(k..).unwrap_or(&[]).to_vec(),
        ..cmd.clone()
    };
    let Some(source) = program_source(&inner) else {
        return Decision::Allow;
    };
    // A program delivered as literal text by a process substitution or a
    // pipe (user ruling 2026-11-18 (a)) is read like a literal stdin body.
    if let Some(body) = literal_delivered_program(text, commands, idx, cmd, &source, k) {
        return literal_body(fam, &body, depth, judge);
    }
    match (fam, source) {
        (_, ProgramSource::Undetermined(r)) => undetermined(format_args!("`{base}`: {r}")),
        (Family::Shell, ProgramSource::InlineLiteral(prog)) => layer(&prog, depth + 1, judge),
        (fam, ProgramSource::Stdin(StdinSource::Literal(body))) => {
            literal_body(fam, &body, depth, judge)
        }
        // The Bash tool's stdin is empty: a bare shell reading the inherited
        // stdin gets no B.5 opinion (user ruling 2026-11-18 (b)).
        (Family::Shell, ProgramSource::Stdin(StdinSource::Inherited)) => Decision::Allow,
        (Family::Python, ProgramSource::InlineLiteral(_)) => Decision::Allow,
        (_, ProgramSource::InlineLiteral(_)) => {
            undetermined(format_args!("`{base}` runs an inline program"))
        }
        (_, ProgramSource::Stdin(src)) => undetermined(format_args!(
            "`{base}` reads its program from {}",
            stdin_phrase(&src)
        )),
        (fam, ProgramSource::File(path)) => {
            if fam == Family::Shell {
                if let Some(d) =
                    written_then_run(text, commands, idx, &path, Runner::Shell(base), judge)
                {
                    return d;
                }
            }
            // Ruling 2026-11-18 T1 (interpreter file runs; `-m` never gets
            // here: its source is `Undetermined`).
            if depth == 0
                && file_operand_first(fam, args)
                && tracked_unmodified(text, commands, idx, &path, judge.scope)
            {
                return Decision::Allow;
            }
            let shown = match &path {
                Value::Literal(p) => format!("`{p}`"),
                Value::Undetermined(r) => format!("whose path is undetermined ({r})"),
            };
            undetermined(format_args!(
                "`{base}` runs the program file {shown}, whose text is not in the command"
            ))
        }
    }
}

/// A literal program body: a shell body is read by this layer recursively;
/// any other interpreter's body gives no B.5 opinion (python: stage S5,
/// `s5.rs`, judges it; perl /
/// ruby / node: the legacy program-text reading decides, user ruling
/// 2026-11-18 (a)).
fn literal_body(fam: Family, body: &str, depth: usize, judge: &Probes<'_>) -> Decision {
    match fam {
        Family::Shell => layer(body, depth + 1, judge),
        Family::Python | Family::PerlLike | Family::Node => Decision::Allow,
    }
}

/// The text a simple command prints when it is a literal producer: `echo`
/// with literal operands (no option, no backslash), `printf` with a literal
/// format without `%` (`\n` the only escape), or `printf '%s' X` /
/// `printf '%s\n' X` with a backslash-free `X`. `None` for anything else,
/// or with a redirect or an assignment.
fn producer_output(c: &SimpleCommand) -> Option<String> {
    if !c.redirects.is_empty() || !c.assignments.is_empty() {
        return None;
    }
    let Ok(argv) = literals(&c.argv) else {
        return None; // not a literal producer: the caller stays Undetermined
    };
    let only_newline_escapes = |f: &str| !f.replace("\\n", "").contains('\\');
    match argv.as_slice() {
        ["echo", rest @ ..] => {
            if rest.first().is_some_and(|a| a.starts_with('-'))
                || rest.iter().any(|a| a.contains('\\'))
            {
                return None;
            }
            Some(format!("{}\n", rest.join(" ")))
        }
        ["printf", "%s", x] if !x.contains('\\') => Some((*x).to_string()),
        ["printf", "%s\\n", x] if !x.contains('\\') => Some(format!("{x}\n")),
        ["printf", fmt] if !fmt.contains('%') && only_newline_escapes(fmt) => {
            Some(fmt.replace("\\n", "\n"))
        }
        _ => None,
    }
}

/// The literal output of the process substitution whose `<( … )` spans
/// `range`: every command inside it is a [`producer_output`] of the
/// substitution's own list, joined by `;`, `&&` or newlines only.
fn procsub_content(
    text: &str,
    commands: &[SimpleCommand],
    range: &std::ops::Range<usize>,
) -> Option<String> {
    let open = range.start.checked_add(2)?;
    let close = range.end.checked_sub(1)?;
    if text.get(range.start..open)? != "<(" || text.get(close..range.end)? != ")" {
        return None;
    }
    let inside: Vec<&SimpleCommand> = commands
        .iter()
        .filter(|c| c.span.start >= open && c.span.end <= close)
        .collect();
    if inside.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut pos = open;
    for c in &inside {
        if c.context != Context::ProcessSubstitution || c.span.start < pos {
            return None;
        }
        let g = text.get(pos..c.span.start)?;
        let sep_ok = g
            .chars()
            .all(|ch| ch.is_whitespace() || ch == ';' || ch == '&')
            && (!g.contains('&') || g.trim() == "&&");
        if !sep_ok {
            return None;
        }
        out.push_str(&producer_output(c)?);
        pos = c.span.end;
    }
    let tail = text.get(pos..close)?;
    tail.chars()
        .all(|ch| ch.is_whitespace() || ch == ';')
        .then_some(out)
}

/// The program `cmd` (argv from `k`) reads, when it is delivered as literal
/// text by a process substitution (`bash <(echo hi)`, `bash < <(echo hi)`)
/// or by the previous stage of a pipe (`printf '%s' x | node`).
pub(super) fn literal_delivered_program(
    text: &str,
    commands: &[SimpleCommand],
    idx: usize,
    cmd: &SimpleCommand,
    source: &ProgramSource,
    k: usize,
) -> Option<String> {
    match source {
        // `bash <(…)`: the first non-option operand is the substitution.
        ProgramSource::Undetermined(_) | ProgramSource::File(Value::Undetermined(_)) => {
            let w = cmd
                .argv
                .get(k + 1..)?
                .iter()
                .find(|w| !literal(w).is_ok_and(|s| s.starts_with('-')))?;
            if !w.raw.starts_with("<(") {
                return None;
            }
            procsub_content(text, commands, &w.span)
        }
        ProgramSource::Stdin(StdinSource::Literal(_)) => None,
        ProgramSource::Stdin(_) => {
            let ins: Vec<_> = cmd
                .redirects
                .iter()
                .filter(|r| {
                    matches!(r.fd, None | Some(0))
                        && matches!(
                            r.op,
                            RedirOp::In
                                | RedirOp::InOut
                                | RedirOp::DupIn
                                | RedirOp::HereDoc
                                | RedirOp::HereString
                        )
                })
                .collect();
            if let [r] = ins.as_slice() {
                // `< <(…)` as the only stdin redirect.
                if r.op != RedirOp::In || r.target != RedirTarget::ProcessSubstitution {
                    return None;
                }
                let at = text.get(r.span.clone())?.find("<(")?;
                return procsub_content(text, commands, &(r.span.start + at..r.span.end));
            }
            if !ins.is_empty() {
                return None;
            }
            // `producer | cmd`: the previous stage is one literal producer.
            let Context::Pipeline { pos, len } = cmd.context else {
                return None;
            };
            let prev = commands.get(idx.checked_sub(1)?)?;
            if pos == 0 || prev.context != (Context::Pipeline { pos: pos - 1, len }) {
                return None;
            }
            if text.get(prev.span.end..cmd.span.start)?.trim() != "|" {
                return None;
            }
            producer_output(prev)
        }
        _ => None,
    }
}

fn stdin_phrase(src: &StdinSource) -> String {
    match src {
        StdinSource::Inherited => "the inherited stdin".into(),
        StdinSource::Literal(_) => "a literal stdin".into(),
        StdinSource::FromCommand => "another command's output".into(),
        StdinSource::Fd(_) => "a file descriptor".into(),
        StdinSource::File(Value::Literal(p)) => format!("the file `{p}`"),
        StdinSource::File(Value::Undetermined(r)) => format!("a file ({r})"),
        StdinSource::Undetermined(r) => format!("an undetermined stdin ({r})"),
    }
}

#[derive(Clone, Copy)]
enum Runner<'a> {
    /// `bash x.sh` etc.: the body is that shell's program.
    Shell(&'a str),
    /// `./x.sh`: the shebang decides.
    Direct,
}

/// A path with its `.` components removed (`./x.sh` == `x.sh`); `None` with
/// a `..` component.
fn normalise(p: &str) -> Option<String> {
    let abs = p.starts_with('/');
    let mut parts = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => return None,
            c => parts.push(c),
        }
    }
    let joined = parts.join("/");
    Some(if abs { format!("/{joined}") } else { joined })
}

fn top(c: &SimpleCommand) -> bool {
    c.context == Context::TopLevel
}

/// `text[a..b]` with every here-document body inside it removed.
fn gap(text: &str, commands: &[SimpleCommand], a: usize, b: usize) -> Option<String> {
    if a > b {
        return None;
    }
    let mut out = String::new();
    let mut pos = a;
    let mut bodies: Vec<std::ops::Range<usize>> = commands
        .iter()
        .flat_map(|c| c.redirects.iter())
        .filter_map(|r| match &r.target {
            RedirTarget::HereDoc(h) => Some(h.body_span.clone()),
            _ => None,
        })
        .filter(|s| s.start >= a && s.end <= b)
        .collect();
    bodies.sort_by_key(|s| s.start);
    for s in bodies {
        if s.start < pos {
            return None;
        }
        out.push_str(text.get(pos..s.start)?);
        pos = s.end;
    }
    out.push_str(text.get(pos..b)?);
    Some(out)
}

fn is_and_join(g: &str) -> bool {
    g.trim() == "&&"
}

/// The determined whole-file body `w` writes and the path it writes it to.
fn written_body(w: &SimpleCommand) -> Option<(String, String)> {
    let mut target = None;
    let mut heredoc_or_string = false;
    for r in &w.redirects {
        match r.op {
            RedirOp::Out | RedirOp::Clobber if matches!(r.fd, None | Some(1)) => {
                if target.is_some() {
                    return None;
                }
                match &r.target {
                    RedirTarget::Path(Value::Literal(p)) => target = Some(p.clone()),
                    _ => return None,
                }
            }
            RedirOp::HereDoc | RedirOp::HereString => heredoc_or_string = true,
            _ => return None,
        }
    }
    let target = target?;
    let Ok(argv) = literals(&w.argv) else {
        return None; // not a determined writer: no exception, Undetermined
    };
    let body = match argv.as_slice() {
        ["cat"] | ["cat", "-"] if heredoc_or_string => match &w.stdin {
            StdinSource::Literal(b) => b.clone(),
            _ => return None,
        },
        ["echo", rest @ ..] if !heredoc_or_string => {
            if rest.first().is_some_and(|a| a.starts_with('-'))
                || rest.iter().any(|a| a.contains('\\'))
            {
                return None;
            }
            format!("{}\n", rest.join(" "))
        }
        ["printf", fmt] if !heredoc_or_string => {
            if fmt.contains('%') || fmt.replace("\\n", "").contains('\\') {
                return None;
            }
            fmt.replace("\\n", "\n")
        }
        _ => return None,
    };
    Some((body, target))
}

/// True when `commands[w]` (top-level) starts a list or is reached from the
/// start of its list by `&&` only.
fn starts_and_chain(text: &str, commands: &[SimpleCommand], w: usize) -> bool {
    let Some(cur) = commands.get(w) else {
        return false;
    };
    let prev = (0..w).rev().find(|&j| commands.get(j).is_some_and(top));
    let Some(p) = prev else {
        return text
            .get(..cur.span.start)
            .is_some_and(|s| s.trim().is_empty());
    };
    let Some(pc) = commands.get(p) else {
        return false;
    };
    let Some(g) = gap(text, commands, pc.span.end, cur.span.start) else {
        return false;
    };
    if is_and_join(&g) {
        return starts_and_chain(text, commands, p);
    }
    // A list separator: `;` or a newline, with only the previous command's
    // here-document terminator lines besides.
    let delims: Vec<String> = pc
        .redirects
        .iter()
        .filter(|r| r.op == RedirOp::HereDoc)
        .filter_map(|r| text.get(r.span.clone()))
        .map(|s| {
            s.trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start_matches("<<")
                .trim_start_matches('-')
                .trim()
                .trim_matches(|c| c == '\'' || c == '"')
                .to_string()
        })
        .collect();
    let rest: String = g
        .lines()
        .filter(|l| !delims.iter().any(|d| l.trim() == d))
        .collect::<Vec<_>>()
        .join("\n");
    let t = rest.trim();
    (t.is_empty() && g.contains('\n')) || t == ";"
}

/// The written-then-run exception (module doc). `None` = it does not apply.
fn written_then_run(
    text: &str,
    commands: &[SimpleCommand],
    run: usize,
    path: &Value,
    runner: Runner<'_>,
    judge: &Probes<'_>,
) -> Option<Decision> {
    let Value::Literal(p) = path else { return None };
    let want = normalise(p)?;
    let run_cmd = commands.get(run)?;
    if !top(run_cmd) {
        return None;
    }
    let mut cur = run;
    loop {
        let prev = (0..cur).rev().find(|&j| commands.get(j).is_some_and(top))?;
        let (pc, cc) = (commands.get(prev)?, commands.get(cur)?);
        if !is_and_join(&gap(text, commands, pc.span.end, cc.span.start)?) {
            return None;
        }
        if let Some((body, target)) = written_body(pc) {
            if normalise(&target)? != want {
                return None;
            }
            if !starts_and_chain(text, commands, prev) {
                return None;
            }
            return Some(judge_body(&body, runner, judge));
        }
        let is_chmod = pc.redirects.is_empty()
            && pc.argv.first().map(literal) == Some(Ok("chmod"))
            && pc.argv.iter().all(|w| literal(w).is_ok());
        if !is_chmod {
            return None;
        }
        cur = prev;
    }
}

/// The shell a directly executed body runs under: its shebang's, `sh`
/// without one; `None` for a non-shell (or unreadable) shebang.
fn shebang_shell(body: &str) -> Option<&str> {
    let Some(line) = body.lines().next().and_then(|l| l.strip_prefix("#!")) else {
        return Some("sh");
    };
    let mut words = line.split_whitespace();
    let first = basename(words.next()?);
    let prog = if first == "env" {
        basename(words.next()?)
    } else {
        first
    };
    (family(prog) == Some(Family::Shell)).then_some(prog)
}

fn judge_body(body: &str, runner: Runner<'_>, judge: &Probes<'_>) -> Decision {
    let shell = match runner {
        Runner::Shell(s) => s,
        Runner::Direct => match shebang_shell(body) {
            Some(s) => s,
            None => {
                return undetermined(
                    "the written file is run by a non-shell interpreter named in its shebang",
                )
            }
        },
    };
    let active = JUDGE_DEPTH.with(Cell::get);
    if active >= MAX_DEPTH {
        return undetermined("written-then-run bodies are nested too deeply");
    }
    JUDGE_DEPTH.with(|d| d.set(active + 1));
    let quoted = format!("'{}'", body.replace('\'', "'\\''"));
    let verdict = (judge.judge)(&format!("{shell} -c {quoted}"));
    JUDGE_DEPTH.with(|d| d.set(active));
    verdict
}

/// `bash -c` / `sh -c` with no other word: see the caller.
fn dash_c_alone(args: &[Word]) -> bool {
    matches!(args, [only] if literal(only).is_ok_and(|v| v == "-c"))
}

// ------------------------------------------------- T3: noexec syntax check

/// True when the shell options before the first operand are only short-flag
/// clusters of `n` (noexec) and the harmless `x` / `v` / `e` / `u`, with at
/// least one `n` (ruling 2026-11-18 T3). `-ni` (bash ignores `-n` when
/// interactive), `-c`, `-s`, `+n`, `-o`, long options and anything
/// undetermined are not this shape: the caller's normal reading decides
/// (`bash -n -c BODY` judges BODY as if it ran). An `-n` AFTER the program
/// file is a script argument and is never reached here.
fn noexec_only(args: &[Word]) -> bool {
    let mut saw_n = false;
    for w in args {
        let Ok(v) = literal(w) else { return false };
        if v == "--" || v == "-" || !v.starts_with('-') {
            break;
        }
        let flags = &v[1..];
        if flags.starts_with('-') || !flags.chars().all(|c| "nxveu".contains(c)) {
            return false;
        }
        saw_n |= flags.contains('n');
    }
    saw_n
}

// ------------------------------------- T1: tracked and unmodified program file

/// For the T1 exception, the program file must be the first argument of a
/// perl / ruby / node (no option before it: `-I`, `-M`, `-r` load other
/// code); shell and python options were already validated by
/// [`program_source`].
fn file_operand_first(fam: Family, args: &[Word]) -> bool {
    match fam {
        Family::Shell | Family::Python => true,
        Family::PerlLike | Family::Node => args
            .first()
            .is_some_and(|w| literal(w).is_ok_and(|s| !s.starts_with('-'))),
    }
}

/// Environment variables that change WHICH program runs or what it loads
/// besides the file (interpreter search path, startup files, preloads).
fn injects(name: &str) -> bool {
    name == "PATH"
        || name == "ENV"
        || name == "SHELLOPTS"
        || name == "PS4"
        || name == "IFS"
        || ["PYTHON", "NODE", "PERL", "RUBY", "BASH", "LD_", "DYLD_"]
            .iter()
            .any(|p| name.starts_with(p))
}

/// Builtins / tools that set variables from `NAME=V` operands.
const DECLARERS: &[&str] = &["export", "declare", "typeset", "local", "readonly", "env"];

/// Commands that write no file except through their redirects.
const READ_ONLY: &[&str] = &[
    "echo", "printf", "pwd", "true", "false", ":", "test", "[", "ls", "cat", "head", "tail", "wc",
    "grep", "egrep", "fgrep", "rg", "cut", "tr", "diff", "cmp", "which", "type", "date", "sleep",
    "basename", "dirname", "realpath", "readlink", "stat", "cd", "unset",
];

/// Commands that write only files named in their operands.
const NAMED_WRITERS: &[&str] = &[
    "cp", "mv", "tee", "sed", "chmod", "chown", "chgrp", "ln", "install", "truncate", "touch",
    "rm", "rmdir", "mkdir",
];

/// `git` subcommands that change no work-tree file except through options
/// naming one (`--output=…`), which the operand check catches.
const GIT_READ_ONLY: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "rev-parse",
    "ls-files",
    "describe",
    "blame",
    "shortlog",
];

/// The written-file probe context.
struct Target<'a> {
    lex: String,
    real: String,
    base: &'a str,
    hook_cwd: &'a str,
    scope: &'a crate::scope::SafeRoots,
}

impl Target<'_> {
    /// Could the operand / redirect target `op` (relative to `cwd`) be the
    /// program file, an ancestor directory of it, or an alias of either?
    /// Anything not decidable is a hit.
    fn hit(&self, op: &str, cwd: &Value) -> bool {
        if op.contains(self.base) {
            return true;
        }
        let abs = if op.starts_with('/') {
            op.to_string()
        } else {
            match cwd {
                Value::Literal(c) => format!("{}/{op}", c.trim_end_matches('/')),
                Value::Undetermined(r) if r == super::eval::HOOK_CWD => {
                    format!("{}/{op}", self.hook_cwd.trim_end_matches('/'))
                }
                Value::Undetermined(_) => return true,
            }
        };
        let harness_core::verdict::Determination::Known(lex) =
            self.scope.lexical_absolute(&abs, None)
        else {
            return true;
        };
        let covers = |a: &str, p: &str| {
            a == p || a == "/" || p.starts_with(&format!("{}/", a.trim_end_matches('/')))
        };
        if covers(&lex, &self.lex) {
            return true;
        }
        match self.scope.resolve(&lex) {
            Some(real) => covers(&real, &self.real),
            None => true,
        }
    }

    /// May `c` write the program file? `run` = `c` is the run itself (only
    /// its redirects count).
    fn may_write(&self, c: &SimpleCommand, run: bool) -> bool {
        for r in &c.redirects {
            if matches!(
                r.op,
                RedirOp::Out
                    | RedirOp::Append
                    | RedirOp::Clobber
                    | RedirOp::InOut
                    | RedirOp::OutErr
                    | RedirOp::AppendOutErr
                    | RedirOp::DupOut
            ) {
                match &r.target {
                    // Already resolved against a literal cd; relative = the
                    // hook's cwd.
                    RedirTarget::Path(Value::Literal(p)) => {
                        let hook = Value::Undetermined(super::eval::HOOK_CWD.to_string());
                        if self.hit(p, &hook) {
                            return true;
                        }
                    }
                    RedirTarget::Path(Value::Undetermined(_)) => return true,
                    _ => {}
                }
            }
        }
        if run {
            return false;
        }
        let mut k = 0;
        loop {
            match unwrap_at(&c.argv, k) {
                Unwrapped::NotWrapper => break,
                Unwrapped::Nothing => return false,
                Unwrapped::Undetermined(_) => return true,
                Unwrapped::At(next) => k = next,
            }
        }
        let Some(first) = c.argv.get(k) else {
            return false;
        };
        let Ok(name) = literal(first) else {
            return true;
        };
        if name.contains('/') && !dirname(name).is_some_and(|d| SYSTEM_BIN_DIRS.contains(&d)) {
            return true;
        }
        let base = basename(name);
        let ops = c.argv.get(k + 1..).unwrap_or(&[]);
        let named = NAMED_WRITERS.contains(&base)
            || (base == "git"
                && ops
                    .iter()
                    .find_map(|w| literal(w).ok().filter(|s| !s.starts_with('-')))
                    .is_some_and(|s| GIT_READ_ONLY.contains(&s)));
        if READ_ONLY.contains(&base) {
            return false;
        }
        if !named {
            return true;
        }
        ops.iter().any(|w| match literal(w) {
            Ok(s) => self.hit(s, &c.cwd),
            Err(_) => true,
        })
    }
}

/// A variable that [`injects`] is set anywhere on the line (prefix
/// assignment, `env NAME=V`, `export NAME=V`, an assignment-only command), or
/// a declarer has an undetermined operand.
pub(super) fn sets_injecting_var(commands: &[SimpleCommand]) -> bool {
    commands.iter().any(|c| {
        if c.assignments.iter().any(|(n, _)| injects(n)) {
            return true;
        }
        let mut declarer = false;
        c.argv.iter().any(|w| match literal(w) {
            Ok(s) => {
                if DECLARERS.contains(&basename(s)) {
                    declarer = true;
                    return false;
                }
                declarer
                    && s.split_once('=')
                        .is_some_and(|(n, _)| injects(n.trim_start_matches('-')))
            }
            Err(_) => declarer,
        })
    })
}

/// The line has a loop or a function, so a command written AFTER the run in
/// the text may execute before a later iteration / call of it. Also read by
/// stage S5c (`s5.rs`, same-line import shadowing).
pub(super) fn has_loop_or_function(text: &str) -> bool {
    text.contains("()")
        || text
            .split(|c: char| c.is_whitespace() || ";&|(){}<>".contains(c))
            .any(|t| matches!(t, "for" | "while" | "until" | "select" | "function"))
}

/// Ruling 2026-11-18 T1: the program file `path` run by `commands[run]` is
/// git-tracked AND unmodified: clean in the work tree and index
/// ([`git_tracked_clean`]) and not possibly written by any other part of the
/// same line (redirects of every command, the run's own included; the
/// named-operand writers; any command not known to write only through
/// those). Commands after the run in the text are skipped only when the run
/// is top-level and the line has no loop or function. No injecting variable
/// may be set on the line ([`injects`]). Anything undecidable is `false`.
fn tracked_unmodified(
    text: &str,
    commands: &[SimpleCommand],
    run: usize,
    path: &Value,
    scope: &crate::scope::SafeRoots,
) -> bool {
    let Value::Literal(p) = path else {
        return false;
    };
    let Some(run_cmd) = commands.get(run) else {
        return false;
    };
    let Some(hook_cwd) = scope.session_cwd() else {
        return false;
    };
    if sets_injecting_var(commands) {
        return false;
    }
    let harness_core::verdict::Determination::Known(lex) =
        scope.lexical_absolute(p, Some(hook_cwd))
    else {
        return false;
    };
    let Some(real) = scope.resolve(&lex) else {
        return false;
    };
    let target = Target {
        base: basename(&lex),
        lex: lex.clone(),
        real: real.clone(),
        hook_cwd,
        scope,
    };
    if target.base.is_empty() {
        return false;
    }
    let all = run_cmd.context != Context::TopLevel || has_loop_or_function(text);
    for (j, c) in commands.iter().enumerate() {
        let relevant = j == run || all || c.span.start < run_cmd.span.end;
        if relevant && target.may_write(c, j == run) {
            return false;
        }
    }
    git_tracked_clean(scope, hook_cwd, &real)
}

/// True ONLY when git, asked from the repository that contains the hook's
/// cwd, observes `real` (a canonical path) as a regular file inside that
/// work tree, tracked with a plain `H` index entry (not assume-unchanged /
/// skip-worktree), and with no staged or unstaged change. Every probe is
/// bounded (3 s) and must exit 0; spawn failure, timeout, a non-zero exit, no
/// repository, a path outside it (a symlink leaving the repo included) or
/// unexpected output is `false`.
pub(crate) fn git_tracked_clean(
    scope: &crate::scope::SafeRoots,
    hook_cwd: &str,
    real: &str,
) -> bool {
    use harness_core::verdict::Determination;
    match std::fs::symlink_metadata(real) {
        Ok(m) if m.file_type().is_file() => {}
        Ok(_) | Err(_) => return false,
    }
    let probe = |dir: &str, args: &[&str]| -> Determination<String> {
        let mut cmd = std::process::Command::new("git");
        harness_core::git_env::scrub(&mut cmd).arg("-C").arg(dir);
        cmd.args([
            "--no-optional-locks",
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
        ]);
        cmd.args(args);
        match harness_core::boundary::run_with_timeout(
            &mut cmd,
            std::time::Duration::from_millis(3000),
        ) {
            Determination::Known(out) => out.stdout_allowing(&[0]),
            Determination::Undetermined(why) => Determination::Undetermined(why),
        }
    };
    let Determination::Known(top) = probe(hook_cwd, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    let Some(top) = scope.resolve(top.trim_end_matches('\n')) else {
        return false;
    };
    let Some(rel) = real.strip_prefix(&format!("{}/", top.trim_end_matches('/'))) else {
        return false;
    };
    if rel.is_empty() {
        return false;
    }
    match probe(
        &top,
        &["ls-files", "--error-unmatch", "-v", "-z", "--", rel],
    ) {
        Determination::Known(s) => {
            let recs: Vec<&str> = s.split('\0').filter(|r| !r.is_empty()).collect();
            if recs != [format!("H {rel}").as_str()] {
                return false;
            }
        }
        Determination::Undetermined(_) => return false,
    }
    matches!(
        probe(
            &top,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
                "--",
                rel,
            ],
        ),
        Determination::Known(s) if s.is_empty()
    )
}
