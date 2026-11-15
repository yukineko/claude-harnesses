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
//!   the command text alone (plus the process `HOME` for `~` / `$HOME`). It is
//!   never a guessed literal.
//!
//! # Current state (skeleton)
//!
//! This is the skeleton task of run-20261115-212659-5759: the types and
//! signatures are frozen, but nothing is implemented yet.
//!
//! * [`analyze`] returns `Analysis::Undetermined(Cause::NotImplemented)` for
//!   **every** input.
//! * [`program_source`] returns
//!   `Some(ProgramSource::Undetermined("program_source: not implemented"))`
//!   for **every** input. It deliberately does not return `None`: `None` means
//!   "this command is not a shell/interpreter", which a caller could read as
//!   "no program to inspect", i.e. clean.
//!
//! No detector calls this module yet; the migration tasks (`ast-parse`, the
//! evaluator, and the detector migrations of this run) wire it in.

// Skeleton with no caller yet: the ast-parse / evaluator / detector-migration
// tasks of run-20261115-212659-5759 wire it in and remove this allow.
#![allow(dead_code)]

mod eval;
mod parse;

use std::ops::Range;

/// Inputs longer than this many bytes are not parsed:
/// `Analysis::Undetermined(Cause::TooLong)`.
///
/// Placeholder value: the real bound is chosen by measurement in the
/// `ast-parse` task.
pub(crate) const MAX_PARSE_BYTES: usize = 64 * 1024;

/// Maximum structural nesting depth, pre-scanned before brush-parser is
/// invoked; deeper inputs yield `Analysis::Undetermined(Cause::TooDeep)`.
///
/// Placeholder value: the real bound is chosen by measurement in the
/// `ast-parse` task.
pub(crate) const MAX_PARSE_DEPTH: usize = 64;

/// Parse and statically evaluate `command`.
///
/// Skeleton: always `Analysis::Undetermined(Cause::NotImplemented)`.
pub(crate) fn analyze(command: &str) -> Analysis {
    match parse::parse(command) {
        Ok(program) => eval::evaluate(command, &program),
        Err(cause) => Analysis::Undetermined(cause),
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
    TooLong,
    TooDeep,
    Timeout,
    Parse(String),
    Panic,
    ZshOnly(String),
    Unsupported(String),
}

/// Every simple command that would execute, in source order, already
/// statically evaluated. A for-loop over N literal words yields N instances
/// (with `loop_var` bound). Commands inside $(…)/`…`/<(…) also appear,
/// with `context` saying so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Script {
    pub commands: Vec<SimpleCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SimpleCommand {
    /// `argv[0]` is the command word (after assignment prefixes).
    pub argv: Vec<Word>,
    /// Prefix assignments `X=v cmd`.
    pub assignments: Vec<(String, Word)>,
    pub redirects: Vec<Redirect>,
    /// Statically tracked cwd (Undetermined after `cd $(..)`, `cd -`,
    /// unknown var).
    pub cwd: Value,
    pub stdin: StdinSource,
    pub context: Context,
    /// BYTE offsets into the original command.
    pub span: Range<usize>,
}

/// Where a [`SimpleCommand`] sits in the script.
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
    /// Undetermined if unquoted and holds expansions.
    pub body: Value,
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

/// For shells/interpreters (bash, sh, zsh, dash, python3, perl, node, ruby):
/// the program the command runs.
///
/// Skeleton: always
/// `Some(ProgramSource::Undetermined("program_source: not implemented"))`,
/// never `None` (see the module doc).
pub(crate) fn program_source(cmd: &SimpleCommand) -> Option<ProgramSource> {
    eval::program_source(cmd)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProgramSource {
    /// `-c prog` / `-e prog` with a determined value.
    InlineLiteral(String),
    /// `bash x.sh …`: first non-option operand is the script; later operands
    /// (incl. /dev/stdin) are args.
    File(Value),
    /// `bash`, `bash -s`, `sh -`, `bash /dev/stdin`, `python3 -`.
    Stdin(StdinSource),
    Undetermined(String),
}
