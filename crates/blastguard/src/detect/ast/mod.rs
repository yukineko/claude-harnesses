//! `detect::ast` — the single shell-command input for migrated detectors.
//!
//! The pipeline is: a pre-scan (length / structural depth), brush-parser
//! 0.4.0 (`parse`), then a static evaluator (`eval`) that turns the AST into a
//! flat [`Script`] of every simple command that would execute, with each word
//! already statically evaluated.
//!
//! # Contract (holds for every caller)
//!
//! * [`Analysis::Undetermined`] and every other `Undetermined` variant in this
//!   module ([`Value::Undetermined`], [`StdinSource::Undetermined`],
//!   [`FdTarget::Undetermined`], [`ProgramSource::Undetermined`]) mean
//!   "cannot judge". They are **never** folded into a literal, an empty
//!   string, an empty command list, or a clean verdict. A detector that
//!   receives one must resolve it to its restrictive side (Ask / Deny), never
//!   to Allow.
//! * A [`Value::Literal`] is only produced when the value is determined from
//!   the command text alone. It is never a guessed literal. The one value
//!   taken from outside the text is `HOME`: `$HOME` / `${HOME}` and a bare
//!   leading `~` are the hook process's `HOME` when it is an absolute path
//!   (user ruling 2026-11-17, SUBSET.md ruling 12), unless the line assigns
//!   it.
//!
//! # Current state
//!
//! The types and signatures are frozen (run-20261115-212659-5759). Both
//! stages are implemented: the parse stage (`parse.rs`: caps, depth
//! pre-scan, zsh-only syntax, brush-parser under `catch_unwind`) and the
//! static evaluator (`eval.rs`, which documents what it tracks, with its
//! arithmetic contexts in `arith.rs` and its stateless helpers in
//! `support.rs`).
//!
//! * [`analyze`] returns the parse stage's `Undetermined` cause
//!   (`TooLong` / `TooDeep` / `ZshOnly` / `Parse` / `Panic`) for inputs it
//!   rejects; for an input that parses, `Known(Script)`, or
//!   `Undetermined(Unsupported(_))` when the script holds something whose
//!   commands the evaluator cannot list (a function definition, `coproc`, a
//!   `$( )` inside a parameter operation or arithmetic, a subscript or
//!   arithmetic expression that reads a value not determined by the text —
//!   user ruling 2026-11-16, see `arith.rs` — the work budget
//!   exceeded), or the parse stage's cause when a word, heredoc body or
//!   `$( )` body inside it does not parse, or `Undetermined(Panic)` if the
//!   evaluator panicked. `Undetermined(NotImplemented)` is no longer
//!   produced.
//! * [`program_source`] is `None` only for a command that is neither an
//!   interpreter, nor `eval` / `source` / `.`, nor a wrapper (`env`, `sudo`,
//!   `xargs`, `exec`, `timeout`, ...) with an interpreter-named or
//!   undetermined operand; an undetermined command word is
//!   `Some(Undetermined)`.
//!
//! The path-resolution detectors read it through [`targets`] (task
//! `migrate-path` of run-20261116-020031-33040): the truncating / append
//! redirect targets, `rm` operands, the `sed -i` / `chmod` / … mutated files
//! and the `cp` / `mv` / `install` destination that the string readings
//! cannot place are judged on the values [`targets::resolve`] hands back.
//! The Table B.5 layer ([`b5`], stage S4) reads [`program_source`], the
//! stdin sources and the process-substitution / pipe producers of every
//! command, and raises the legacy verdict (`max(B.5, legacy)`).
//!
//! `judge.rs` freezes the EFFECT x TARGET-PATH judgement (`Effect`,
//! `PathClass`, `Judgement`, `judge`) specified in `SUBSET.md` (task S1
//! `subset-spec`, user rulings 2026-11-17). Stage S3b (user ruling
//! 2026-11-18) wires it into `detect` through [`table_b_verdict`] for the
//! Table B B.1 commands only (undetermined option / unresolved target → Ask);
//! `classify` is still a stub (see `judge.rs`).

// `targets`, `table_b_verdict` and `b5` (which reads `program_source` /
// `StdinSource`) are the callers so far; the rest of the contract (e.g.
// `Cause` variants no detector distinguishes) is still unread, so this allow
// stays until the later migration tasks remove it.
#![allow(dead_code)]

mod arith;
pub(super) mod b5;
mod eval;
mod judge;
mod parse;
mod support;
pub(super) mod targets;
#[cfg(test)]
mod tests;

use std::ops::Range;

/// Inputs longer than this many bytes are not parsed:
/// `Analysis::Undetermined(Cause::TooLong)`, checked before brush-parser is
/// called.
///
/// 8 KiB: user ruling 2026-11-16 (3 of the 417 commands in the blastguard
/// block log exceed it). Measured parse time at this size is ~1–3 ms release
/// (see `parse.rs`).
pub(crate) const MAX_PARSE_BYTES: usize = 8 * 1024;

/// Maximum nesting depth, pre-scanned (over-counting) before brush-parser is
/// invoked; deeper inputs yield `Analysis::Undetermined(Cause::TooDeep)`.
///
/// 16: user ruling 2026-11-16. The deepest command in the blastguard block
/// log (417 rows, plain `( { [` counter) is 13; the shallowest brush-parser
/// stack overflow measured is at depth 128 (debug build, 2 MiB stack); see
/// the table in `parse.rs`.
pub(crate) const MAX_PARSE_DEPTH: usize = 16;

/// Parse and statically evaluate `command` (see the module doc for the
/// `Undetermined` causes).
pub(crate) fn analyze(command: &str) -> Analysis {
    match parse::parse(command) {
        Ok(program) => eval::evaluate(command, &program),
        Err(cause) => Analysis::Undetermined(cause),
    }
}

/// Phrase every S3b Ask for an undetermined Table B row carries
/// ([`crate::rule_id`] keys `ast-table-b-undetermined` on it).
pub(crate) const TABLE_B_UNDETERMINED: &str = "is outside its SUBSET.md Table B row";

/// Phrase every S3b Ask for an unresolved Table B target carries
/// ([`crate::rule_id`] keys `ast-table-b-unresolved` on it).
pub(crate) const TABLE_B_UNRESOLVED: &str =
    "a target of a SUBSET.md Table B command cannot be placed";

/// Stage S3b (user ruling 2026-11-18): the [`judge::judge`] verdict of
/// `command` for the commands that have a Table B B.1 row, as a
/// [`Decision`](crate::model::Decision) the caller folds into the legacy
/// verdict raise-only (`Deny > Ask > Allow`; the legacy reason wins a tie).
///
/// * [`Analysis::Undetermined`] → `Allow` from this layer: analyze-wide
///   Undetermined handling is unchanged by S3b (the legacy layer alone
///   answers it, as before);
/// * [`judge::Judgement::Undetermined`] or [`judge::Judgement::Ask`] → `Ask`
///   (main) — the hook's `ask_available` hardens it to Deny in a subagent;
/// * a `judge` panic → `Ask` (SUBSET.md "Undetermined handling" rule 4);
/// * [`judge::Judgement::Deny`] → `Deny`, produced only by the per-candidate
///   cwd judgement (user ruling 2026-11-18): the reasons are those of the
///   detector at that candidate, so a system-harm Deny keeps its mark.
pub(crate) fn table_b_verdict(
    command: &str,
    scope: &crate::scope::SafeRoots,
) -> crate::model::Decision {
    use crate::model::Decision;
    use judge::Judgement;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    let Analysis::Known(script) = analyze(command) else {
        return Decision::Allow;
    };
    // User ruling 2026-11-18: a command whose cwd is one of a finite set is
    // judged once per candidate by the whole detector (see
    // `judge::judge_candidates`); the synthetic text it builds has one known
    // cwd, so this never recurses past one level.
    let legacy = |text: &str| super::readings::detect_bash_every_reading(text, scope);
    let ctx = judge::PathCtx {
        scope,
        hook_cwd: scope.session_cwd(),
        legacy: Some(&legacy),
    };
    let judged = catch_unwind(AssertUnwindSafe(|| judge::judge(&script, &ctx)))
        .unwrap_or(Judgement::Undetermined(Cause::Panic));
    match judged {
        Judgement::Allow => Decision::Allow,
        Judgement::Undetermined(cause) => {
            let why = match cause {
                Cause::Unsupported(s) | Cause::Parse(s) | Cause::ZshOnly(s) => s,
                other => format!("{other:?}"),
            };
            Decision::ask(format!(
                "{why}: this {TABLE_B_UNDETERMINED}, so blastguard cannot judge its effect and \
                 asks rather than guess"
            ))
        }
        Judgement::Ask(findings) => Decision::ask(format!(
            "{TABLE_B_UNRESOLVED} — {} — so blastguard cannot tell what it changes and asks \
             rather than guess",
            findings
                .iter()
                .map(|f| f.reason.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )),
        Judgement::Deny { findings, .. } => Decision::deny(
            findings
                .iter()
                .map(|f| f.reason.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        ),
    }
}

/// The result of [`analyze`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Analysis {
    Known(Script),
    /// Cannot judge. Never treat as clean.
    Undetermined(Cause),
}

/// Why an [`Analysis`] is undetermined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Cause {
    NotImplemented,
    /// Over [`MAX_PARSE_BYTES`]. Detectors resolve it to Deny, with a reason
    /// telling the caller to write the script to a file with the Write tool
    /// and run it with a short command — user ruling 2026-11-16.
    TooLong,
    /// Over [`MAX_PARSE_DEPTH`]. Detectors resolve it to Deny, with a reason
    /// telling the caller to flatten the command (split it, or write a script
    /// file with the Write tool and run that) — user ruling 2026-11-16.
    TooDeep,
    Timeout,
    Parse(String),
    Panic,
    ZshOnly(String),
    Unsupported(String),
}

/// Every simple command that may execute, already statically evaluated, in
/// evaluation order: the commands of a `$(…)` / `` `…` `` / `<(…)` in a
/// command's words come before that command (their `context` says where they
/// are). Commands on a conditional path (`&&` / `||` / `if` / `case` arms,
/// loop bodies) all appear. A for-loop over N literal words yields N
/// instances of its body (with the loop variable bound); a loop over a list
/// not determined from the text yields its body once, with the loop variable
/// `Undetermined`. A compound command's own redirections (`{ …; } > f`,
/// `done < f`) appear as an extra command with empty `argv`, spanning the
/// compound command, before the commands inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Script {
    pub commands: Vec<SimpleCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SimpleCommand {
    /// `argv[0]` is the command word (after assignment prefixes). Empty for
    /// an assignment-only or redirection-only command (`X=v`, `> f`).
    pub argv: Vec<Word>,
    /// Prefix assignments `X=v cmd` (and the assignments of an
    /// assignment-only command). Before a command word, each value is
    /// expanded without the other prefix assignments of the same command
    /// (`Undetermined` if it refers to one); an assignment-only command binds
    /// them left to right.
    pub assignments: Vec<(String, Word)>,
    pub redirects: Vec<Redirect>,
    /// Statically tracked cwd:
    ///
    /// * no `cd` applies to the command: `Undetermined` with the reason
    ///   `"cwd: the hook's cwd (no cd applies to this command)"` — the
    ///   command runs in the hook's cwd, which the text does not name;
    /// * after a literal `cd` that must have succeeded (`cd /a && cmd`):
    ///   `Literal`, absolute and normalised (`/a`); a relative `cd` is joined
    ///   to the previous literal cwd;
    /// * otherwise `Undetermined` with another reason: `cd -`, `cd` alone,
    ///   `cd $(..)`, an unbound variable, `pushd` / `popd`, a `..` component,
    ///   a relative `cd` from the hook's cwd or where `CDPATH` may apply, and
    ///   after `cd x;` (the `cd` may have failed).
    ///
    /// Relative paths in [`RedirTarget::Path`] and [`ProgramSource::File`]
    /// are already resolved against this cwd (see [`RedirTarget::Path`]);
    /// relative paths in `argv` are as written.
    pub cwd: Value,
    /// The cwd as a candidate set (user ruling 2026-11-18); read only by the
    /// judge. See [`CwdCandidates`].
    pub cwd_candidates: CwdCandidates,
    pub stdin: StdinSource,
    pub context: Context,
    /// BYTE offsets into the original command.
    pub span: Range<usize>,
}

/// The directories a [`SimpleCommand`] may run in (user ruling 2026-11-18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CwdCandidates {
    /// One cwd, the one [`SimpleCommand::cwd`] states (the hook's cwd or a
    /// literal directory).
    One,
    /// The paths that reach the command have different but known cwds
    /// (`cd x; cmd`: `x` or the hook's cwd), at most `MAX_CWD_CANDIDATES`
    /// (4, `eval/cwd.rs`) of them: each candidate (`None` = the hook's cwd).
    /// [`SimpleCommand::cwd`] is `Undetermined` for every other reader; the
    /// judge judges every candidate.
    OneOf(Vec<Option<String>>),
    /// No finite set of cwds is known (the reason, as in
    /// [`SimpleCommand::cwd`]): a relative target cannot be placed.
    Undetermined(String),
}

/// Where a [`SimpleCommand`] sits in the script: the innermost enclosing
/// construct of these kinds (`TopLevel` when there is none; brace groups,
/// `if`, `case` and loops do not count). E.g. a pipeline stage inside a
/// `$( )` is `Pipeline`, a command in a `$( )` inside a pipeline stage is
/// `CommandSubstitution`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Context {
    TopLevel,
    CommandSubstitution,
    ProcessSubstitution,
    Subshell,
    Pipeline { pos: usize, len: usize },
    Background,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Word {
    pub raw: String,
    pub value: Value,
    pub quoting: Quoting,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Quoting {
    Unquoted,
    Single,
    Double,
    AnsiC,
    Mixed,
}

/// Determined from the text alone, or not. Never a guessed literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Literal(String),
    /// Cannot judge; the string says why. Never treat as a literal.
    Undetermined(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Redirect {
    pub fd: Option<u32>,
    pub op: RedirOp,
    pub target: RedirTarget,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedirOp {
    Out,
    Append,
    Clobber,
    In,
    InOut,
    DupIn,
    DupOut,
    HereDoc,
    HereString,
    OutErr,
    AppendOutErr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RedirTarget {
    /// A file path (`> p`, `>> p`, `< p`, `&> p`, `>&word` with a non-fd
    /// word). Representation of a relative path (user ruling 2026-11-16,
    /// pinned by `tests.rs`):
    ///
    /// * no `cd` applies to the command ([`SimpleCommand::cwd`] is the
    ///   hook-cwd `Undetermined`): `Literal` as written (`echo x > f` ->
    ///   `Literal("f")`); the consumer reads it against the hook's cwd;
    /// * after a literal `cd`: joined to the tracked cwd, absolute
    ///   (`cd /tmp/w && echo x > f` -> `Literal("/tmp/w/f")`); a `..`
    ///   component makes it `Undetermined` (lexical normalisation is unsound
    ///   under symlinks);
    /// * after a `cd` whose target is undetermined (`cd -`, `cd $(..)`, an
    ///   unbound variable, or a `cd` that may have failed): `Undetermined`.
    ///
    /// So a relative `Literal` never remains after a `cd`. An absolute path
    /// stays as written. A target that would be word-split or glob-expanded
    /// is `Undetermined`.
    Path(Value),
    Fd(FdTarget),
    HereDoc(HereDoc),
    HereString(Value),
    ProcessSubstitution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FdTarget {
    Number(u32),
    Move(u32),
    Close,
    Undetermined(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HereDoc {
    pub quoted: bool,
    pub strip_tabs: bool,
    /// Undetermined if unquoted and holds expansions. With `strip_tabs`, the
    /// leading tabs are already removed.
    pub body: Value,
    /// The raw body in the command (without the terminator line, with any
    /// leading tabs).
    pub body_span: Range<usize>,
}

/// What arrives on the command's stdin, statically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StdinSource {
    /// Nothing on the line feeds it.
    Inherited,
    /// `echo lit |`, `printf 'lit' |` (simple escapes only), `<<< lit`,
    /// quoted heredoc, unquoted heredoc w/o expansions.
    Literal(String),
    /// Piped from a non-literal producer.
    FromCommand,
    /// `<&N` etc.
    Fd(FdTarget),
    /// `< path`
    File(Value),
    Undetermined(String),
}

/// For shells/interpreters (bash, sh, zsh, dash, ksh, python / python3[.N],
/// perl, ruby, node): the program the command runs. Options that are not
/// modelled make it `Undetermined`.
///
/// `None` only when the command is not one of those, not `eval` / `source` /
/// `.`, and not a wrapper (`env`, `sudo`, `xargs`, `exec`, `nohup`,
/// `timeout`, ...) with an interpreter-named or undetermined operand; also
/// `None` for an empty `argv`. Such a wrapper, `eval` / `source` / `.`, and
/// an undetermined command word are `Some(Undetermined)`.
pub(crate) fn program_source(cmd: &SimpleCommand) -> Option<ProgramSource> {
    prog::program_source(cmd)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProgramSource {
    /// `-c prog` / `-e prog` with a determined value.
    InlineLiteral(String),
    /// `bash x.sh …`: first non-option operand is the script; later operands
    /// (incl. /dev/stdin) are args. A relative path is represented as in
    /// [`RedirTarget::Path`].
    File(Value),
    /// `bash`, `bash -s`, `sh -`, `bash /dev/stdin`, `python3 -`.
    Stdin(StdinSource),
    Undetermined(String),
}

/// [`program_source`]: option parsing per interpreter family over an
/// already-evaluated [`SimpleCommand`]. (Kept here rather than in `eval.rs`,
/// which is near the repository's 2000-line file cap.)
mod prog {
    use super::{ProgramSource, SimpleCommand, Value, Word};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Family {
        Shell,
        Python,
        PerlLike,
        Node,
    }

    fn family(base: &str) -> Option<Family> {
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

    /// Commands that run another command named in their operands.
    const WRAPPERS: &str = "env sudo doas su xargs exec command builtin nohup nice ionice \
        timeout time stdbuf chrt taskset setsid caffeinate arch unbuffer flock";

    fn basename(s: &str) -> &str {
        s.rsplit('/').next().unwrap_or(s)
    }

    pub(super) fn program_source(cmd: &SimpleCommand) -> Option<ProgramSource> {
        let first = cmd.argv.first()?;
        let name = match &first.value {
            Value::Literal(s) => s.as_str(),
            Value::Undetermined(r) => {
                return Some(ps_undet(format!(
                    "command word undetermined ({r}): it may be an interpreter"
                )))
            }
        };
        let base = basename(name);
        let args = cmd.argv.get(1..).unwrap_or(&[]);
        match family(base) {
            Some(Family::Shell) => Some(shell_source(cmd, args)),
            Some(Family::Python) => Some(python_source(cmd, args)),
            Some(Family::PerlLike) => Some(perl_like_source(cmd, args)),
            Some(Family::Node) => Some(node_source(cmd, args)),
            None => {
                if matches!(base, "eval" | "source" | ".") {
                    return Some(ps_undet(format!("`{base}` runs shell code: not modelled")));
                }
                let runs_interpreter = WRAPPERS.split_whitespace().any(|w| w == base)
                    && args.iter().any(|a| match &a.value {
                        Value::Undetermined(_) => true,
                        Value::Literal(s) => {
                            family(basename(s)).is_some() || matches!(s.as_str(), "eval" | "source")
                        }
                    });
                runs_interpreter.then(|| ps_undet(format!("`{base}` may run an interpreter")))
            }
        }
    }

    fn ps_undet(s: impl Into<String>) -> ProgramSource {
        ProgramSource::Undetermined(s.into())
    }

    fn is_dev_stdin(p: &str) -> bool {
        matches!(p, "/dev/stdin" | "/dev/fd/0" | "/proc/self/fd/0")
    }

    /// The program from the first operand (or stdin when there is none).
    fn operand_source(cmd: &SimpleCommand, operand: Option<&Word>) -> ProgramSource {
        match operand.map(|w| &w.value) {
            None => ProgramSource::Stdin(cmd.stdin.clone()),
            Some(Value::Literal(p)) if is_dev_stdin(p) => ProgramSource::Stdin(cmd.stdin.clone()),
            Some(Value::Literal(p)) => {
                ProgramSource::File(super::eval::resolve_at(&cmd.cwd, Value::Literal(p.clone())))
            }
            Some(Value::Undetermined(r)) => ps_undet(format!("script operand undetermined ({r})")),
        }
    }

    fn inline(w: Option<&Word>, opt: &str) -> ProgramSource {
        match w.map(|w| &w.value) {
            Some(Value::Literal(p)) => ProgramSource::InlineLiteral(p.clone()),
            Some(Value::Undetermined(r)) => ps_undet(format!("{opt} program undetermined ({r})")),
            None => ps_undet(format!("{opt} without a program")),
        }
    }

    fn shell_source(cmd: &SimpleCommand, args: &[Word]) -> ProgramSource {
        const LONG: &str = "--norc --noprofile --posix --login --noediting --verbose --restricted";
        let (mut c_flag, mut s_flag) = (false, false);
        let mut i = 0;
        while let Some(a) = args.get(i) {
            let v = match &a.value {
                Value::Literal(s) => s.as_str(),
                Value::Undetermined(r) => {
                    return ps_undet(format!("shell option undetermined ({r})"))
                }
            };
            if v == "-" || v == "--" {
                i += 1;
                break;
            }
            if v.starts_with("--") {
                if !LONG.split_whitespace().any(|l| l == v) {
                    return ps_undet(format!("shell option {v} not modelled"));
                }
                i += 1;
                continue;
            }
            let flags = match v.strip_prefix('-').or_else(|| v.strip_prefix('+')) {
                Some(f) if !f.is_empty() => f,
                _ => break,
            };
            let mut takes_arg = false;
            for (k, ch) in flags.chars().enumerate() {
                match ch {
                    'c' => c_flag = true,
                    's' => s_flag = true,
                    'o' | 'O' if k + 1 == flags.len() => takes_arg = true,
                    c if "abefhkmnptuvxBCEHPTilr".contains(c) => {}
                    _ => return ps_undet(format!("shell option {v} not modelled")),
                }
            }
            i += 1;
            if takes_arg {
                match args.get(i).map(|w| &w.value) {
                    Some(Value::Literal(_)) => i += 1,
                    _ => return ps_undet(format!("{v} without a determined argument")),
                }
            }
        }
        let operand = args.get(i);
        if c_flag {
            return inline(operand, "-c");
        }
        if s_flag {
            return ProgramSource::Stdin(cmd.stdin.clone());
        }
        operand_source(cmd, operand)
    }

    fn python_source(cmd: &SimpleCommand, args: &[Word]) -> ProgramSource {
        let mut i = 0;
        while let Some(a) = args.get(i) {
            let v = match &a.value {
                Value::Literal(s) => s.as_str(),
                Value::Undetermined(r) => {
                    return ps_undet(format!("python option undetermined ({r})"))
                }
            };
            if v == "-" {
                return ProgramSource::Stdin(cmd.stdin.clone());
            }
            if v == "--" {
                i += 1;
                break;
            }
            if v.starts_with("--") {
                return ps_undet(format!("python option {v} not modelled"));
            }
            let flags = match v.strip_prefix('-') {
                Some(f) if !f.is_empty() => f,
                _ => break,
            };
            i += 1;
            for (k, ch) in flags.char_indices() {
                let rest = flags.get(k + ch.len_utf8()..).unwrap_or("");
                match ch {
                    'c' if !rest.is_empty() => {
                        return ProgramSource::InlineLiteral(rest.to_string())
                    }
                    'c' => return inline(args.get(i), "-c"),
                    'm' => return ps_undet("python -m runs a module"),
                    'W' | 'X' => {
                        if rest.is_empty() {
                            i += 1;
                        }
                        break;
                    }
                    c if "bBdEhiIOPqsSuvVxR".contains(c) => {}
                    _ => return ps_undet(format!("python option {v} not modelled")),
                }
            }
        }
        operand_source(cmd, args.get(i))
    }

    /// perl / ruby: `-e prog` (once), `-` (stdin), or a script operand.
    fn perl_like_source(cmd: &SimpleCommand, args: &[Word]) -> ProgramSource {
        let mut programs: Vec<ProgramSource> = Vec::new();
        let mut i = 0;
        while let Some(a) = args.get(i) {
            let v = match &a.value {
                Value::Literal(s) => s.as_str(),
                Value::Undetermined(r) => return ps_undet(format!("option undetermined ({r})")),
            };
            if v == "-" {
                return ProgramSource::Stdin(cmd.stdin.clone());
            }
            if v == "--" {
                i += 1;
                break;
            }
            let flags = match v.strip_prefix('-') {
                Some(f) if !f.is_empty() && !f.starts_with('-') => f,
                Some(_) => return ps_undet(format!("option {v} not modelled")),
                None => break,
            };
            i += 1;
            for (k, ch) in flags.char_indices() {
                let rest = flags.get(k + ch.len_utf8()..).unwrap_or("");
                match ch {
                    'e' | 'E' if !rest.is_empty() => {
                        programs.push(ProgramSource::InlineLiteral(rest.to_string()));
                        break;
                    }
                    'e' | 'E' => {
                        programs.push(inline(args.get(i), "-e"));
                        i += 1;
                        break;
                    }
                    c if "acnlpsStTuUvwW".contains(c) => {}
                    _ => return ps_undet(format!("option {v} not modelled")),
                }
            }
        }
        match programs.len() {
            0 => operand_source(cmd, args.get(i)),
            1 => programs.pop().unwrap_or_else(|| ps_undet("-e")),
            _ => ps_undet("more than one -e program"),
        }
    }

    fn node_source(cmd: &SimpleCommand, args: &[Word]) -> ProgramSource {
        // Every option node takes is either handled here or Undetermined, so
        // only the first argument needs to be looked at.
        let v = match args.first().map(|a| &a.value) {
            None => return operand_source(cmd, None),
            Some(Value::Literal(s)) => s.as_str(),
            Some(Value::Undetermined(r)) => {
                return ps_undet(format!("node option undetermined ({r})"))
            }
        };
        match v {
            "-" => ProgramSource::Stdin(cmd.stdin.clone()),
            "--" => operand_source(cmd, args.get(1)),
            "-e" | "--eval" | "-p" | "--print" => inline(args.get(1), v),
            o if o.starts_with('-') => ps_undet(format!("node option {o} not modelled")),
            _ => operand_source(cmd, args.first()),
        }
    }
}

/// `$( )` (also `$(( ))`), the legacy arithmetic `$[ ]`, backquotes or
/// process substitution in raw text.
fn has_subst(s: &str) -> bool {
    s.contains("$(") || s.contains("$[") || s.contains('`') || s.contains("<(") || s.contains(">(")
}
