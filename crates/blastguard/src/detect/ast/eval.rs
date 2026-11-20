//! Static evaluator of `detect::ast`: brush-parser AST -> [`Script`].
//!
//! The evaluator walks the program in execution order with an abstract shell
//! state ([`Env`]: variables, cwd, stdin) and emits one [`SimpleCommand`] per
//! simple command that may execute. Every value it cannot prove from the
//! command text is `Undetermined`; nothing is guessed.
//!
//! # What is tracked (and what is deliberately not)
//!
//! * Variables: only values assigned on the line from literal text
//!   (`S=/a`, `export S=/a`, `for v in a b`). Every variable the line does not
//!   assign (environment, positional / special parameters, shell dynamic
//!   variables such as `$RANDOM` / `$PWD`) is `Undetermined` — except
//!   `$HOME`, which starts as the hook environment's `HOME` when that is an
//!   absolute path (user ruling 2026-11-17, SUBSET.md ruling 12).
//!   Subshells, pipeline stages, `$( )`, `<( )` and `&` get a copy that does
//!   not leak back. Where two paths can reach a point (`&&` / `||`, `if`,
//!   `case`, loops) the states are joined: a variable keeps its value only if
//!   every path agrees, otherwise it is `Undetermined`.
//! * Anything that may change variables in a way not modelled — `eval`,
//!   `source`, `.`, `trap`, `alias`, `shopt`, `builtin`, `command`, an
//!   undetermined command word, `declare`-family options, `set` with options
//!   other than the plain ones — makes the state *opaque*: from there on every
//!   variable, the cwd and stdin are `Undetermined`. Sole exception (user
//!   ruling 2026-11-18): `. F` where `F`'s value is `<hook HOME>/.cargo/env`
//!   (rustup's PATH setup), unwrapped and without a prefix assignment, keeps
//!   the state; `source` of it and every other sourced file stay opaque.
//!   `builtin [--] w ...` and
//!   `command [-p] [--] w ...` (nested too) are unwrapped first: the wrapped
//!   command `w` has its effects and assignment checks as if it ran alone
//!   (a wrapped declaration builtin's operands keep Field mode), and then the
//!   wrapper still makes the state opaque. `command -v` / `-V` (a lookup
//!   that runs nothing), an undetermined word and an unmodelled option of
//!   the chain have no effect other than making the state opaque.
//! * cwd: `cd <literal>` moves the tracked cwd for the commands that run only
//!   if it succeeded (`cd x && ...`); after `cd x;` the cd may have failed, so
//!   the cwd is the join of both: [`Cwd::OneOf`] when every joined path has a
//!   known cwd and there are at most 4 of them (`eval/cwd.rs`
//!   `MAX_CWD_CANDIDATES`; user
//!   ruling 2026-11-18; one cwd to every reader but the judge), else
//!   `Undetermined`. `cd -`, `cd` alone,
//!   `cd` with options, `cd $(..)`, an undetermined operand, `pushd` / `popd`
//!   make it `Undetermined`. A relative `cd` from the hook's cwd, a relative
//!   `cd` while `CDPATH` may apply, and any `..` component are `Undetermined`.
//! * Unquoted expansions that would be word-split or pathname-expanded, globs
//!   and brace expansions in unquoted text, `~` other than a bare `~` that
//!   starts the word (that one is `$HOME`, ruling 12; `~user`, and a tilde
//!   after `=` / `:` in any word, e.g. `of=~/f`, stay `Undetermined`), `$((..))`, `$( )` output and parameter operations
//!   other than plain `$x` / `${x}` are `Undetermined`. Arithmetic (`((..))`,
//!   `$((..))`, `let`, `[[ a -eq b ]]` unless both operands are literal
//!   integers, a builtin's name operand `a[i]` with a non-digit `i`, e.g.
//!   `read 'a[x=8]'`) makes every tracked variable `Undetermined`.
//! * Function definitions and `coproc` make the whole analysis
//!   `Undetermined(Unsupported)` (their bodies run at a time this evaluator
//!   does not model), as does a `$( )` / backquote inside a parameter
//!   operation, an arithmetic expression or a subscript (also a builtin's
//!   quoted name operand, `read 'a[$(c)]'`; `${` too there): the commands it
//!   runs would be invisible.
//! * So does an arithmetic context (a subscript, `$(( ))`, `(( ))`, `let`,
//!   `[[ a -eq b ]]`, a `declare -i` name's value, ...) that reads a value not
//!   determined by the text (user ruling 2026-11-16): bash evaluates such a
//!   value recursively, so `x='a[$(c)]'` from the environment runs `c`. Only
//!   names bound to literals on the line are determined; `arith.rs` has the
//!   list of contexts and what an expression may hold. Variables after an
//!   arithmetic assignment are `Undetermined`, so a later read of them in
//!   arithmetic (a loop counter `i=$((i+1))`) is `Unsupported` too.
//!   `declare -i` (also `local` / `typeset`) names are remembered: a later
//!   assignment to one is arithmetic. Once the state is opaque (`eval`,
//!   `source`, `.`, an undetermined command word, ...) any name may have the
//!   integer attribute (user ruling 2026-11-16, reading A). From there on (and
//!   for a `declare -i` name) these assignments are checked as arithmetic:
//!   plain `n=v` and prefix `n=v cmd`; the operands of `export` / `declare` /
//!   `typeset` / `local` / `readonly` (with the opacity from before the
//!   command); the `printf -v` target; every literal operand of `read` /
//!   `mapfile` / `readarray` / `getopts` / `wait` plus their implicit targets
//!   (`REPLY`, `MAPFILE`, `OPTARG` / `OPTIND`); and a `for` variable (each
//!   literal word, or an undetermined value for an undetermined list); and
//!   `${n:=v}` / `${n=v}` (also `${a[i]:=v}`; an indirect `${!x:=v}` counts
//!   as possibly integer while opaque or any name is `declare -i`). The
//!   variables being `Undetermined`, a value not determined by the text or
//!   that reads a name is `Unsupported` (even a literal word such as `bar` or
//!   `/abs/path`: `bar` / `abs` name variables). The opaque flag is OR-ed
//!   where paths join. Once the text declared `_` integer (`declare -i _`,
//!   `-ri _`, `declare -i x _`, a possible `-i` option), the last word of
//!   every later simple command (the command name if alone; also behind
//!   `builtin` / `command`) is checked too, in the states after it: bash
//!   assigns it to `_`. Giving `_` the attribute checks its current value:
//!   the previous simple command's last word (tracked in `Env`), else
//!   `Undetermined` (inherited, or after `&`, a pipeline, a compound command,
//!   `[[ ]]`, an assignment-only command, opacity). Opaque alone does not
//!   make `_` integer (user ruling 2026-11-16; accepted residual).
//!   An undetermined non-assignment operand
//!   of a declaration builtin (`declare $opt n=v`) counts as a possible `-i`.
//!   The keys of a compound value are arithmetic in a declaration builtin
//!   too (`declare -a a=([i]=1)`), checked in operand order; a compound
//!   operand leaves its name `Undetermined` (it is an array, not the text
//!   `( ... )`), and a key that is not a decimal integer may assign, so it
//!   makes the tracked variables `Undetermined`.
//!
//! The whole evaluation runs under `catch_unwind` (a panic is
//! `Undetermined(Panic)`) and under a work budget (`Undetermined(Unsupported)`
//! when exceeded).
//!
//! Stateless helpers (escape decoding, path joining, fd / stdin bookkeeping,
//! `set` options, pipeline stdin) live in `support.rs`; the arithmetic checks
//! in `arith.rs`; parameter-expansion values and the final value of an
//! expanded word (`param`, `finish`) in the child module `eval/words.rs`;
//! the declaration builtins (`declaration`) in the child module `eval/decl.rs`.

use super::support::{
    apply_stdin, brace_group_stdin, conv_fd, decode_ansi_c, normalise_abs, parse_fd_target,
    parse_heredoc_contained, piped_stdin, set_is_harmless, unescape_backquote,
};
use super::{
    arith, has_subst, parse, Analysis, Cause, Context, FdTarget, HereDoc, Quoting, RedirOp,
    RedirTarget, Redirect, Script, SimpleCommand, StdinSource, Value, Word,
};
use brush_parser::ast;
use brush_parser::word::{TildeExpr, WordPiece, WordPieceWithSource};
use brush_parser::SourceSpan;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::panic::{catch_unwind, AssertUnwindSafe};
use words::{finish, param};

mod cwd;
mod decl;
mod words;

pub(super) use cwd::hook_dir;
pub(crate) use cwd::with_hook_dir;
use cwd::{cd_join, resolve, Cwd};

/// The reason carried by [`SimpleCommand::cwd`] when no `cd` applies to the
/// command: it runs in the hook's cwd, which the command text does not name.
pub(super) const HOOK_CWD: &str = "cwd: the hook's cwd (no cd applies to this command)";

/// Simple commands evaluated (including the dry passes of loop fixpoints)
/// before the analysis gives up as `Unsupported`.
const MAX_WORK: usize = 100_000;
/// Commands emitted before the analysis gives up as `Unsupported`.
const MAX_COMMANDS: usize = 10_000;
/// A `for` over more literal words than this is not unrolled; its loop
/// variable is `Undetermined` instead.
const MAX_UNROLL: usize = 256;
/// Dry passes over a loop body before every variable is made `Undetermined`.
const MAX_FIXPOINT: usize = 4;

/// Evaluate a parsed program.
pub(super) fn evaluate(command: &str, program: &ast::Program) -> Analysis {
    match catch_unwind(AssertUnwindSafe(|| run(command, program))) {
        Ok(Ok(script)) => Analysis::Known(script),
        Ok(Err(cause)) => Analysis::Undetermined(cause),
        Err(_) => Analysis::Undetermined(Cause::Panic),
    }
}

fn run(command: &str, program: &ast::Program) -> Result<Script, Cause> {
    let text = Text::new(command, Origin::root(command.len()));
    let mut ev = Ev {
        out: Vec::new(),
        emit: true,
        work: 0,
        accums: Vec::new(),
    };
    ev.program(&text, program, Env::initial(), &Context::TopLevel)?;
    Ok(Script { commands: ev.out })
}

// ---------------------------------------------------------------- spans

/// Where a piece of (possibly nested) program text sits in the original
/// command. `base: Some(b)`: local byte `i` is global byte `b + i`. `None`:
/// the text was rewritten (backquote unescaping, heredoc tab stripping), so
/// every span inside it is reported as `whole`.
#[derive(Debug, Clone)]
struct Origin {
    base: Option<usize>,
    whole: Range<usize>,
}

impl Origin {
    fn root(len: usize) -> Self {
        Origin {
            base: Some(0),
            whole: 0..len,
        }
    }

    fn map(&self, r: &Range<usize>) -> Range<usize> {
        match self.base {
            Some(b) => b + r.start..b + r.end,
            None => self.whole.clone(),
        }
    }

    fn sub(&self, r: &Range<usize>, exact: bool) -> Origin {
        let whole = self.map(r);
        let base = if exact && self.base.is_some() {
            Some(whole.start)
        } else {
            None
        };
        Origin { base, whole }
    }

    fn inexact(&self) -> Origin {
        Origin {
            base: None,
            whole: self.whole.clone(),
        }
    }
}

/// One program text (the command, or the body of a `$( )`): brush reports
/// CHARACTER indices; `c2b` turns them into byte offsets.
struct Text<'t> {
    s: &'t str,
    origin: Origin,
    c2b: Vec<usize>,
}

impl<'t> Text<'t> {
    fn new(s: &'t str, origin: Origin) -> Self {
        let mut c2b: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
        c2b.push(s.len());
        Text { s, origin, c2b }
    }

    fn local(&self, span: &SourceSpan) -> Option<Range<usize>> {
        let a = *self.c2b.get(span.start.index)?;
        let b = *self.c2b.get(span.end.index)?;
        (a <= b).then_some(a..b)
    }

    fn global(&self, span: Option<&SourceSpan>) -> Range<usize> {
        match span.and_then(|s| self.local(s)) {
            Some(r) => self.origin.map(&r),
            None => self.origin.whole.clone(),
        }
    }

    fn word_origin(&self, w: &ast::Word) -> Origin {
        match w.loc.as_ref().and_then(|l| self.local(l)) {
            Some(r) => {
                let exact = self.s.get(r.clone()) == Some(w.value.as_str());
                self.origin.sub(&r, exact)
            }
            None => self.origin.inexact(),
        }
    }
}

// ---------------------------------------------------------------- state

#[derive(Debug, Clone, PartialEq, Eq)]
struct Env {
    /// Variables assigned on the line. Absent = not assigned here (its value,
    /// if any, comes from outside the text): `Undetermined` on lookup.
    vars: BTreeMap<String, Value>,
    /// `readonly` names: a later assignment fails, so it is not trusted.
    readonly: BTreeSet<String>,
    /// Names that may have the integer attribute (`declare -i`): every
    /// assignment to one is evaluated as arithmetic.
    integer: BTreeSet<String>,
    /// Something unmodelled may have changed any variable / attribute; while
    /// set, every name may have the integer attribute (see `int_assign`).
    opaque: bool,
    cwd: Cwd,
    stdin: StdinSource,
}

/// Variables the shell itself sets or changes at run time (whitespace
/// separated).
const DYNAMIC_VARS: &str = "RANDOM SRANDOM SECONDS LINENO EPOCHSECONDS EPOCHREALTIME BASHPID \
    BASH_SUBSHELL BASH_COMMAND HISTCMD PWD OLDPWD PIPESTATUS REPLY OPTARG OPTIND FUNCNAME \
    BASH_LINENO BASH_SOURCE BASH_ARGV BASH_ARGC BASH_REMATCH GROUPS DIRSTACK UID EUID PPID \
    BASHOPTS SHELLOPTS BASH_VERSINFO _";

impl Env {
    /// The state before the first command: nothing assigned except `HOME`,
    /// which is Known from the hook's own environment (user ruling
    /// 2026-11-17, SUBSET.md ruling 12) when it is set to an absolute path.
    /// A `HOME` assigned / unset on the line rebinds it like any name; an
    /// opaque state makes it `Undetermined` like every variable.
    fn initial() -> Self {
        let mut vars = BTreeMap::new();
        if let Some(home) = hook_home() {
            vars.insert("HOME".to_string(), Value::Literal(home));
        }
        Env {
            vars,
            readonly: BTreeSet::new(),
            integer: BTreeSet::new(),
            opaque: false,
            cwd: Cwd::Hook,
            stdin: StdinSource::Inherited,
        }
    }

    fn lookup(&self, name: &str) -> Value {
        if self.opaque {
            return undet(format!(
                "${name}: an earlier command may have changed any variable"
            ));
        }
        if DYNAMIC_VARS.split_whitespace().any(|v| v == name) {
            return undet(format!("${name}: set by the shell at run time"));
        }
        match self.vars.get(name) {
            Some(v) => v.clone(),
            None => undet(format!("${name}: not assigned on this command line")),
        }
    }

    fn bind(&mut self, name: &str, v: Value) {
        let v = if self.opaque || self.readonly.contains(name) {
            undet(format!(
                "{name}: assignment not trusted (readonly / opaque)"
            ))
        } else {
            v
        };
        self.vars.insert(name.to_string(), v);
    }

    fn forget(&mut self, name: &str, why: &str) {
        self.vars
            .insert(name.to_string(), undet(format!("{name}: {why}")));
    }

    fn forget_operand(&mut self, operand: &str, why: &str) -> Result<(), Cause> {
        arith::check_operand(operand, &|n| self.lookup(n))?;
        if arith::subscript_may_assign(operand) {
            self.havoc_vars(&format!("the subscript of {operand} may assign variables"));
        }
        if let Some(base) = operand.split('[').next().filter(|b| is_ident(b)) {
            self.forget(base, why);
        }
        Ok(())
    }

    /// Assigning `v` to `name` (`name` may carry a subscript): arithmetic if
    /// `name` may have the integer attribute — it was declared `-i` here, or
    /// the state is opaque (an `eval` / `source` / ... may have given any
    /// name the attribute; user ruling 2026-11-16, reading A). The paths
    /// that go through here are listed in the module doc.
    fn int_assign(&self, name: &str, v: &Value) -> Result<(), Cause> {
        self.int_assign_if(self.opaque, name, v)
    }

    /// [`Env::int_assign`] with the "any name may be integer" flag given by
    /// the caller: a declaration builtin passes the opacity from *before* the
    /// command, since an option of the command itself (which makes the state
    /// opaque) gives the integer attribute only to the names it declares, and
    /// only with an `i` (or an undetermined option) — `int_attr`, which puts
    /// those names in `integer` first.
    fn int_assign_if(&self, any_integer: bool, name: &str, v: &Value) -> Result<(), Cause> {
        let base = name.split('[').next().unwrap_or(name);
        if any_integer || self.integer.contains(base) {
            arith::check_value(v, &|n| self.lookup(n))
        } else {
            Ok(())
        }
    }

    /// The value of `_`: the last word of the previous simple command (kept
    /// under the key `_`, which `lookup` never reads), `Undetermined` before
    /// the first one (inherited), after anything not modelled or when opaque.
    fn underscore(&self) -> Value {
        match self.vars.get("_") {
            Some(v) if !self.opaque => v.clone(),
            _ => undet("$_: inherited, or set by something not modelled"),
        }
    }

    /// The arithmetic `expr` (raw text) evaluated in this state.
    fn arith(&self, expr: &str) -> Result<(), Cause> {
        arith::check_text(expr, &|n| self.lookup(n))
    }

    fn havoc_vars(&mut self, why: &str) {
        for (k, v) in self.vars.iter_mut() {
            *v = undet(format!("{k}: {why}"));
        }
    }

    fn make_opaque(&mut self, why: &str) {
        self.opaque = true;
        self.havoc_vars(why);
        self.cwd = Cwd::Undetermined(format!("cwd: {why}"));
        self.stdin = StdinSource::Undetermined(format!("stdin: {why}"));
    }
}

/// `HOME` of the hook's own process environment, when it is a non-empty
/// absolute path made only of `[A-Za-z0-9/._+,:@%^-]` (ruling 12) — the same
/// rule the static resolver applies before it substitutes `HOME`
/// (`readings/static_bindings.rs` `hook_home`), so both layers agree on when
/// `HOME` is Known. Anything else (unset, relative, not UTF-8, a blank or
/// other special character) leaves `$HOME` / `~` `Undetermined`.
fn hook_home() -> Option<String> {
    std::env::var("HOME").ok().filter(|h| {
        h.starts_with('/')
            && h.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, '/' | '.' | '_' | '+' | ',' | ':' | '@' | '%' | '^' | '-')
            })
    })
}

/// True when `v` is the literal `<hook HOME>/.cargo/env` (r21 exemption).
fn is_cargo_env(v: &Value) -> bool {
    matches!((v, hook_home()), (Value::Literal(p), Some(h))
        if *p == format!("{}/.cargo/env", h.trim_end_matches('/')))
}

fn undet(s: impl Into<String>) -> Value {
    Value::Undetermined(s.into())
}

/// The state at a point two paths can reach.
fn join(a: Env, b: Env) -> Env {
    if a == b {
        return a;
    }
    let mut vars = BTreeMap::new();
    let keys: BTreeSet<&String> = a.vars.keys().chain(b.vars.keys()).collect();
    for k in keys {
        let v = match (a.vars.get(k), b.vars.get(k)) {
            (Some(x), Some(y)) if x == y => x.clone(),
            _ => undet(format!("{k}: differs between the paths that reach here")),
        };
        vars.insert(k.clone(), v);
    }
    let readonly = a.readonly.union(&b.readonly).cloned().collect();
    let integer = a.integer.union(&b.integer).cloned().collect();
    let cwd = if a.cwd == b.cwd {
        a.cwd
    } else {
        match (a.cwd.candidates(), b.cwd.candidates()) {
            (Ok(x), Ok(y)) => Cwd::from_candidates(x.union(&y).cloned().collect()),
            _ => Cwd::Undetermined(
                "cwd: differs between the paths that reach here (a cd may have failed)".into(),
            ),
        }
    };
    let stdin = if a.stdin == b.stdin {
        a.stdin
    } else {
        StdinSource::Undetermined("stdin: differs between the paths that reach here".into())
    };
    Env {
        vars,
        readonly,
        integer,
        opaque: a.opaque || b.opaque,
        cwd,
        stdin,
    }
}

fn join_opt(a: Option<Env>, b: Env) -> Env {
    match a {
        Some(a) => join(a, b),
        None => b,
    }
}

/// The states after a command that succeeded / failed.
struct Outcome {
    ok: Env,
    fail: Env,
}

impl Outcome {
    fn same(env: Env) -> Self {
        Outcome {
            ok: env.clone(),
            fail: env,
        }
    }
}

// ---------------------------------------------------------------- words

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// argv / for-list / redirect target: word splitting and pathname
    /// expansion apply.
    Field,
    /// Assignment value, here-string, case / `[[ ]]` operand: no splitting,
    /// no pathname expansion.
    Assign,
    /// Unquoted heredoc body.
    Heredoc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seg {
    QuotedLit,
    UnquotedLit,
    QuotedExp,
    UnquotedExp,
}

#[derive(Default)]
struct Acc {
    segs: Vec<(String, Option<Seg>)>,
    undet: Option<String>,
    unquoted: bool,
    single: bool,
    double: bool,
    ansic: bool,
    escape: bool,
    expansion: bool,
}

impl Acc {
    fn push(&mut self, s: String, seg: Seg) {
        self.segs.push((s, Some(seg)));
    }

    fn undet(&mut self, why: impl Into<String>) {
        if self.undet.is_none() {
            self.undet = Some(why.into());
        }
    }

    fn quoting(&self) -> Quoting {
        let kinds = [
            (self.unquoted, Quoting::Unquoted),
            (self.single, Quoting::Single),
            (self.double, Quoting::Double),
            (self.ansic, Quoting::AnsiC),
            (self.escape, Quoting::Mixed),
        ];
        let present: Vec<Quoting> = kinds.iter().filter(|k| k.0).map(|k| k.1).collect();
        match present.as_slice() {
            [] => Quoting::Unquoted,
            [one] => *one,
            _ => Quoting::Mixed,
        }
    }

    fn any_quote(&self) -> bool {
        self.single || self.double || self.ansic || self.escape
    }
}

fn is_ident(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(ch) if ch == '_' || ch.is_ascii_alphabetic())
        && c.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

// ---------------------------------------------------------------- paths

// `resolve` (a relative path placed against the tracked cwd) lives in
// `eval/cwd.rs` with the `Cwd` type.

// ---------------------------------------------------------------- evaluator

const DECL_BUILTINS: &[&str] = &["export", "declare", "typeset", "local", "readonly"];

/// An assignment operand of a declaration builtin, as parsed by brush.
#[derive(Clone)]
struct DeclArg {
    /// The variable name (`None` for `a[i]=v`).
    name: Option<String>,
    /// Raw keys `k` of a compound value `a=([k]=v)`: arithmetic text the
    /// shell expands and evaluates when the operand is assigned.
    keys: Vec<String>,
    /// The value is compound `( ... )`: an array, not a scalar.
    compound: bool,
}
const ASSIGNING_BUILTINS: &[&str] = &["read", "mapfile", "readarray", "getopts", "unset", "wait"];
const OPAQUE_BUILTINS: &[&str] = &[
    "eval", "source", ".", "trap", "alias", "unalias", "enable", "shopt", "builtin", "command",
];
const SPECIAL_BUILTINS: &[&str] = &[
    ":", ".", "break", "continue", "eval", "exec", "exit", "export", "readonly", "return", "set",
    "shift", "times", "trap", "unset", "source",
];

/// What a `builtin` / `command` wrapper chain at the start of `argv` runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wrapped {
    /// `argv[i]` is the command word that runs (`i == 0`: no wrapper).
    At(usize),
    /// `command -v` / `-V`: a lookup that runs nothing.
    Lookup,
    /// The chain ends (`builtin` alone), or a word / option of it is not
    /// determined or not modelled.
    Unknown,
}

/// Unwrap `builtin [--] w ...` and `command [-p] [--] w ...` (nested too).
fn unwrap_wrappers(argv: &[Word]) -> Wrapped {
    let mut i = 0;
    loop {
        let lit = |j: usize| match argv.get(j).map(|w| &w.value) {
            Some(Value::Literal(s)) => Some(s.as_str()),
            _ => None,
        };
        match lit(i) {
            Some("builtin") => {
                i += 1;
                if lit(i) == Some("--") {
                    i += 1;
                }
            }
            Some("command") => {
                i += 1;
                loop {
                    match lit(i) {
                        Some("--") => {
                            i += 1;
                            break;
                        }
                        Some(o) if o.starts_with('-') && o.len() > 1 => {
                            let o = &o[1..];
                            if o.contains(['v', 'V']) {
                                return Wrapped::Lookup;
                            }
                            if !o.chars().all(|c| c == 'p') {
                                return Wrapped::Unknown;
                            }
                            i += 1;
                        }
                        Some(_) => break,
                        None => return Wrapped::Unknown,
                    }
                }
            }
            Some(_) => return Wrapped::At(i),
            None => return Wrapped::Unknown,
        }
    }
}

struct Ev {
    out: Vec<SimpleCommand>,
    /// False during the dry passes of a loop fixpoint.
    emit: bool,
    work: usize,
    /// One slot per enclosing loop: the join of the states at `break` /
    /// `continue`.
    accums: Vec<Option<Env>>,
}

impl Ev {
    fn tick(&mut self) -> Result<(), Cause> {
        self.work += 1;
        if self.work > MAX_WORK {
            return Err(Cause::Unsupported("evaluation budget exceeded".into()));
        }
        Ok(())
    }

    fn push(&mut self, c: SimpleCommand) -> Result<(), Cause> {
        if self.emit {
            if self.out.len() >= MAX_COMMANDS {
                return Err(Cause::Unsupported("too many commands".into()));
            }
            self.out.push(c);
        }
        Ok(())
    }

    fn program(
        &mut self,
        text: &Text,
        p: &ast::Program,
        mut env: Env,
        ctx: &Context,
    ) -> Result<Env, Cause> {
        for cl in &p.complete_commands {
            env = self.list(text, cl, env, ctx)?;
        }
        Ok(env)
    }

    fn list(
        &mut self,
        text: &Text,
        l: &ast::CompoundList,
        mut env: Env,
        ctx: &Context,
    ) -> Result<Env, Cause> {
        for ast::CompoundListItem(ao, sep) in &l.0 {
            match sep {
                ast::SeparatorOperator::Async => {
                    self.and_or(text, ao, env.clone(), &Context::Background)?;
                    env.forget("_", "after a command run with &");
                }
                ast::SeparatorOperator::Sequence => env = self.and_or(text, ao, env, ctx)?,
            }
        }
        Ok(env)
    }

    fn and_or(
        &mut self,
        text: &Text,
        ao: &ast::AndOrList,
        env: Env,
        ctx: &Context,
    ) -> Result<Env, Cause> {
        let first = self.pipeline(text, &ao.first, env, ctx)?;
        let (mut ok, mut fail) = (first.ok, first.fail);
        for item in &ao.additional {
            match item {
                ast::AndOr::And(p) => {
                    let o = self.pipeline(text, p, ok, ctx)?;
                    ok = o.ok;
                    fail = join(fail, o.fail);
                }
                ast::AndOr::Or(p) => {
                    let o = self.pipeline(text, p, fail, ctx)?;
                    fail = o.fail;
                    ok = join(ok, o.ok);
                }
            }
        }
        Ok(join(ok, fail))
    }

    fn pipeline(
        &mut self,
        text: &Text,
        p: &ast::Pipeline,
        env: Env,
        ctx: &Context,
    ) -> Result<Outcome, Cause> {
        let out = match p.seq.as_slice() {
            [only] => self.command(text, only, env, ctx)?.0,
            stages => {
                let mut env = env;
                env.forget("_", "after a pipeline");
                let len = stages.len();
                let mut stdin = env.stdin.clone();
                for (pos, c) in stages.iter().enumerate() {
                    let mut st = env.clone();
                    st.stdin = stdin;
                    let before = self.out.len();
                    let (_, produced) =
                        self.command(text, c, st, &Context::Pipeline { pos, len })?;
                    stdin = match c {
                        // A brace group feeds what its members print, in order.
                        ast::Command::Compound(ast::CompoundCommand::BraceGroup(b), None) => {
                            brace_group_stdin(b, self.out.get(before..).unwrap_or(&[]))
                        }
                        _ => piped_stdin(produced.as_ref()),
                    };
                }
                Outcome::same(env)
            }
        };
        Ok(if p.bang {
            Outcome {
                ok: out.fail,
                fail: out.ok,
            }
        } else {
            out
        })
    }

    fn command(
        &mut self,
        text: &Text,
        c: &ast::Command,
        env: Env,
        ctx: &Context,
    ) -> Result<(Outcome, Option<SimpleCommand>), Cause> {
        match c {
            ast::Command::Simple(s) => {
                let (o, sc) = self.simple(text, s, env, ctx)?;
                Ok((o, Some(sc)))
            }
            ast::Command::Compound(cc, redirs) => {
                let span = text.global(compound_loc(cc));
                let mut e =
                    self.with_redirects(text, redirs.as_ref(), span, env, ctx, |ev, e| {
                        ev.compound(text, cc, e, ctx)
                    })?;
                e.forget("_", "after a compound command");
                Ok((Outcome::same(e), None))
            }
            ast::Command::Function(_) => Err(Cause::Unsupported(
                "function definition: its body is not evaluated".into(),
            )),
            ast::Command::ExtendedTest(t, redirs) => {
                let span = text.global(Some(&t.loc));
                let mut e =
                    self.with_redirects(text, redirs.as_ref(), span, env, ctx, |ev, mut e| {
                        ev.test_expr(text, &t.expr, &mut e)?;
                        Ok(e)
                    })?;
                e.forget("_", "after [[ ]]");
                Ok((Outcome::same(e), None))
            }
        }
    }

    fn test_expr(
        &mut self,
        text: &Text,
        x: &ast::ExtendedTestExpr,
        env: &mut Env,
    ) -> Result<(), Cause> {
        match x {
            ast::ExtendedTestExpr::And(a, b) | ast::ExtendedTestExpr::Or(a, b) => {
                self.test_expr(text, a, env)?;
                self.test_expr(text, b, env)
            }
            ast::ExtendedTestExpr::Not(a) | ast::ExtendedTestExpr::Parenthesized(a) => {
                self.test_expr(text, a, env)
            }
            ast::ExtendedTestExpr::UnaryTest(op, w) => {
                let v = self.word(text, w, Mode::Assign, env)?.value;
                // `-v a[i]` evaluates the subscript.
                if matches!(op, ast::UnaryPredicate::ShellVariableIsSetAndAssigned) {
                    match &v {
                        Value::Literal(s) => arith::check_operand(s, &|n| env.lookup(n))?,
                        Value::Undetermined(_) => arith::check_value(&v, &|n| env.lookup(n))?,
                    }
                }
                Ok(())
            }
            ast::ExtendedTestExpr::BinaryTest(op, a, b) => {
                let va = self.word(text, a, Mode::Assign, env)?.value;
                let vb = self.word(text, b, Mode::Assign, env)?.value;
                // -eq/-ne/-lt/... evaluate both operands as arithmetic, which
                // may assign (`x=5`, `x++`, or a name whose value does).
                if arith::arith_test_op(op) {
                    for v in [&va, &vb] {
                        arith::check_value(v, &|n| env.lookup(n))?;
                    }
                    if ![va, vb].iter().all(arith::plain_int) {
                        env.havoc_vars("arithmetic in [[ ]] may assign variables");
                    }
                }
                Ok(())
            }
        }
    }

    /// Evaluate a compound command's own redirections (emitted as one
    /// empty-argv command spanning the compound), then its body with the
    /// resulting stdin. A literal stdin is shared by every command inside,
    /// so inside it is `Undetermined`.
    fn with_redirects(
        &mut self,
        text: &Text,
        redirs: Option<&ast::RedirectList>,
        span: Range<usize>,
        mut env: Env,
        ctx: &Context,
        body: impl FnOnce(&mut Self, Env) -> Result<Env, Cause>,
    ) -> Result<Env, Cause> {
        let saved = env.stdin.clone();
        if let Some(rl) = redirs.filter(|rl| !rl.0.is_empty()) {
            self.tick()?;
            let mut stdin = env.stdin.clone();
            let mut redirects = Vec::new();
            for r in &rl.0 {
                let red = self.redirect(text, r, &mut env, &span)?;
                apply_stdin(&mut stdin, &red);
                redirects.push(red);
            }
            self.push(SimpleCommand {
                argv: Vec::new(),
                assignments: Vec::new(),
                redirects,
                cwd: env.cwd.value(),
                cwd_candidates: env.cwd.candidate_list(),
                stdin: stdin.clone(),
                context: ctx.clone(),
                span: span.clone(),
            })?;
            env.stdin = stdin;
        }
        if matches!(env.stdin, StdinSource::Literal(_)) {
            env.stdin = StdinSource::Undetermined(
                "stdin: a literal shared by the commands of a compound command".into(),
            );
        }
        let given = env.stdin.clone();
        let mut after = body(self, env)?;
        after.stdin = if after.stdin == given {
            saved
        } else {
            StdinSource::Undetermined("stdin: changed inside a compound command".into())
        };
        Ok(after)
    }

    fn compound(
        &mut self,
        text: &Text,
        cc: &ast::CompoundCommand,
        mut env: Env,
        ctx: &Context,
    ) -> Result<Env, Cause> {
        use ast::CompoundCommand as C;
        match cc {
            C::Arithmetic(a) => {
                env.arith(&a.expr.value)?;
                env.havoc_vars("arithmetic may assign variables");
                Ok(env)
            }
            C::ArithmeticForClause(f) => {
                // Each expression is checked in the state it runs in: the
                // initializer once, the condition before and the updater
                // after every pass of the body.
                let step = |e: &mut Env, x: &Option<ast::UnexpandedArithmeticExpr>| {
                    if let Some(x) = x {
                        e.arith(&x.value)?;
                    }
                    e.havoc_vars("arithmetic may assign variables");
                    Ok::<(), Cause>(())
                };
                step(&mut env, &f.initializer)?;
                self.fix_loop(env, |ev, mut e| {
                    step(&mut e, &f.condition)?;
                    let mut e = ev.list(text, &f.body.list, e, ctx)?;
                    step(&mut e, &f.updater)?;
                    Ok(e)
                })
            }
            C::BraceGroup(b) => self.list(text, &b.list, env, ctx),
            C::Subshell(s) => {
                self.list(text, &s.list, env.clone(), &Context::Subshell)?;
                Ok(env)
            }
            C::ForClause(f) => self.for_clause(text, f, env, ctx),
            C::CaseClause(c) => self.case_clause(text, c, env, ctx),
            C::IfClause(i) => self.if_clause(text, i, env, ctx),
            C::WhileClause(w) | C::UntilClause(w) => {
                let j = self.fix_loop(env, |ev, e| {
                    let c = ev.list(text, &w.0, e, ctx)?;
                    ev.list(text, &w.1.list, c, ctx)
                })?;
                // The loop ends after a final evaluation of the condition.
                let saved = self.emit;
                self.emit = false;
                let exit = self.list(text, &w.0, j.clone(), ctx);
                self.emit = saved;
                Ok(join(exit?, j))
            }
            C::Coprocess(_) => Err(Cause::Unsupported("coproc is not evaluated".into())),
        }
    }

    /// Run a loop body to a fixpoint without emitting, then once emitting.
    /// Returns a state that covers every iteration count (including 0) and
    /// every `break` / `continue`.
    fn fix_loop(
        &mut self,
        entry: Env,
        mut iter: impl FnMut(&mut Self, Env) -> Result<Env, Cause>,
    ) -> Result<Env, Cause> {
        let saved = self.emit;
        self.emit = false;
        let mut j = entry;
        let mut converged = false;
        for _ in 0..MAX_FIXPOINT {
            self.accums.push(None);
            let end = iter(self, j.clone());
            let acc = self.accums.pop().flatten();
            let end = match end {
                Ok(e) => e,
                Err(c) => {
                    self.emit = saved;
                    return Err(c);
                }
            };
            let next = join_opt(acc, join(j.clone(), end));
            if next == j {
                converged = true;
                break;
            }
            j = next;
        }
        if !converged {
            j.make_opaque("loop state did not converge");
        }
        self.emit = saved;
        self.accums.push(None);
        let end = iter(self, j.clone());
        let acc = self.accums.pop().flatten();
        Ok(join_opt(acc, join(j, end?)))
    }

    fn for_clause(
        &mut self,
        text: &Text,
        f: &ast::ForClauseCommand,
        mut env: Env,
        ctx: &Context,
    ) -> Result<Env, Cause> {
        let mut values = Some(Vec::new());
        match &f.values {
            None => values = None,
            Some(words) => {
                for w in words {
                    let ev = self.word(text, w, Mode::Field, &mut env)?;
                    match (&mut values, ev.value) {
                        (Some(vs), Value::Literal(s)) => vs.push(s),
                        _ => values = None,
                    }
                }
            }
        }
        let var = f.variable_name.as_str();
        match values.filter(|v| v.len() <= MAX_UNROLL) {
            Some(vs) => {
                let mut exits: Option<Env> = None;
                for v in vs {
                    // `declare -i v; for v in 1+1` assigns 2: arithmetic.
                    let v = Value::Literal(v);
                    env.int_assign(var, &v)?;
                    env.bind(var, v);
                    self.accums.push(None);
                    let end = self.list(text, &f.body.list, env.clone(), ctx);
                    let acc = self.accums.pop().flatten();
                    let end = end?;
                    env = match acc {
                        Some(a) => {
                            exits = Some(join_opt(exits, a.clone()));
                            join(end, a)
                        }
                        None => end,
                    };
                }
                Ok(join_opt(exits, env))
            }
            None => self.fix_loop(env, |ev, mut e| {
                let why = "loop variable over a list not determined from the text";
                e.int_assign(var, &undet(format!("{var}: {why}")))?;
                e.forget(var, why);
                ev.list(text, &f.body.list, e, ctx)
            }),
        }
    }

    fn case_clause(
        &mut self,
        text: &Text,
        c: &ast::CaseClauseCommand,
        mut env: Env,
        ctx: &Context,
    ) -> Result<Env, Cause> {
        self.word(text, &c.value, Mode::Assign, &mut env)?;
        let mut result: Option<Env> = None;
        let mut carry: Option<Env> = None;
        for item in &c.cases {
            for p in &item.patterns {
                self.word(text, p, Mode::Assign, &mut env)?;
            }
            let entry = join_opt(carry.take(), env.clone());
            let end = match &item.cmd {
                Some(l) => self.list(text, l, entry, ctx)?,
                None => entry,
            };
            result = Some(join_opt(result, end.clone()));
            carry = match item.post_action {
                ast::CaseItemPostAction::ExitCase => None,
                _ => Some(end),
            };
        }
        Ok(join_opt(result, env))
    }

    fn if_clause(
        &mut self,
        text: &Text,
        i: &ast::IfClauseCommand,
        env: Env,
        ctx: &Context,
    ) -> Result<Env, Cause> {
        let mut cur = self.list(text, &i.condition, env, ctx)?;
        let mut result = self.list(text, &i.then, cur.clone(), ctx)?;
        let mut has_else = false;
        for e in i.elses.iter().flatten() {
            match &e.condition {
                Some(cond) => {
                    cur = self.list(text, cond, cur, ctx)?;
                    let b = self.list(text, &e.body, cur.clone(), ctx)?;
                    result = join(result, b);
                }
                None => {
                    let b = self.list(text, &e.body, cur.clone(), ctx)?;
                    result = join(result, b);
                    has_else = true;
                }
            }
        }
        Ok(if has_else { result } else { join(result, cur) })
    }

    // ------------------------------------------------------------ simple

    fn simple(
        &mut self,
        text: &Text,
        s: &ast::SimpleCommand,
        mut env: Env,
        ctx: &Context,
    ) -> Result<(Outcome, SimpleCommand), Cause> {
        use ast::CommandPrefixOrSuffixItem as I;
        self.tick()?;
        let prefix: &[I] = s.prefix.as_ref().map_or(&[], |p| p.0.as_slice());
        let suffix: &[I] = s.suffix.as_ref().map_or(&[], |p| p.0.as_slice());
        let mut spans: Vec<Range<usize>> = Vec::new();

        // 1. Words (command word, then arguments), in the current state.
        let mut argv = Vec::new();
        if let Some(w) = &s.word_or_name {
            argv.push(self.word(text, w, Mode::Field, &mut env)?);
        }
        let decl = matches!(argv.first().map(|w| &w.value),
            Some(Value::Literal(n)) if DECL_BUILTINS.contains(&n.as_str()));
        // argv index -> assignment operand, for declaration builtins (also
        // one behind `builtin` / `command`, whose operands keep Field mode).
        let mut decl_names: BTreeMap<usize, DeclArg> = BTreeMap::new();
        for item in suffix {
            match item {
                I::Word(w) => argv.push(self.word(text, w, Mode::Field, &mut env)?),
                I::AssignmentWord(a, w) => {
                    let mode = if decl { Mode::Assign } else { Mode::Field };
                    let wrapped_decl = matches!(unwrap_wrappers(&argv),
                        Wrapped::At(i) if i > 0 && matches!(&argv[i].value,
                            Value::Literal(n) if DECL_BUILTINS.contains(&n.as_str())));
                    if decl || wrapped_decl {
                        let name = match &a.name {
                            ast::AssignmentName::VariableName(n) => Some(n.clone()),
                            ast::AssignmentName::ArrayElementName(..) => None,
                        };
                        let (keys, compound) = match &a.value {
                            ast::AssignmentValue::Array(elems) => (
                                elems
                                    .iter()
                                    .filter_map(|(k, _)| k.as_ref().map(|k| k.value.clone()))
                                    .collect(),
                                true,
                            ),
                            ast::AssignmentValue::Scalar(_) => (Vec::new(), false),
                        };
                        decl_names.insert(
                            argv.len(),
                            DeclArg {
                                name,
                                keys,
                                compound,
                            },
                        );
                    }
                    argv.push(self.word(text, w, mode, &mut env)?);
                }
                I::ProcessSubstitution(_, sub) => {
                    argv.push(self.proc_subst_word(text, sub, &env)?);
                }
                I::IoRedirect(_) => {}
            }
        }
        spans.extend(argv.iter().map(|w| w.span.clone()));

        // brush puts only assignments and redirections in a prefix; anything
        // else would be a word this evaluator did not expand.
        if prefix
            .iter()
            .any(|i| matches!(i, I::Word(_) | I::ProcessSubstitution(..)))
        {
            return Err(Cause::Unsupported(
                "word or process substitution in a command prefix".into(),
            ));
        }

        // 2. Redirections, in source order.
        let fallback = text.origin.whole.clone();
        let mut redirects = Vec::new();
        let mut stdin = env.stdin.clone();
        for item in prefix.iter().chain(suffix.iter()) {
            if let I::IoRedirect(r) = item {
                let red = self.redirect(text, r, &mut env, &fallback)?;
                apply_stdin(&mut stdin, &red);
                redirects.push(red);
            }
        }

        // 3. Prefix assignments.
        let mut assignments = Vec::new();
        let assignment_only = argv.is_empty();
        let mut hidden: Vec<String> = Vec::new();
        let mut scratch = env.clone();
        for item in prefix {
            if let I::AssignmentWord(a, w) = item {
                let (name, base, word, bound) = if assignment_only {
                    self.assignment(text, a, w, &mut env)?
                } else {
                    for h in &hidden {
                        scratch.forget(h, "assigned earlier in the same prefix");
                    }
                    self.assignment(text, a, w, &mut scratch)?
                };
                // Before the binding: the value is evaluated in the old state.
                let before = if assignment_only { &env } else { &scratch };
                before.int_assign(&name, &word.value)?;
                spans.push(text.global(w.loc.as_ref()));
                if assignment_only {
                    match bound {
                        Some(v) => env.bind(&base, v),
                        None => env.forget(&base, "array / indexed assignment"),
                    }
                } else {
                    hidden.push(base);
                }
                assignments.push((name, word));
            }
        }
        if !assignment_only {
            // Side effects of evaluating the prefix values (arithmetic,
            // `${x:=}`) still happen in this shell.
            for (k, v) in &scratch.vars {
                if matches!(v, Value::Undetermined(_))
                    && !hidden.contains(k)
                    && matches!(env.vars.get(k), Some(Value::Literal(_)))
                {
                    env.forget(k, "changed while expanding a prefix assignment");
                }
            }
        }
        spans.extend(redirects.iter().map(|r| r.span.clone()));

        let span = match (
            spans.iter().map(|r| r.start).min(),
            spans.iter().map(|r| r.end).max(),
        ) {
            (Some(a), Some(b)) if a <= b => a..b,
            _ => fallback,
        };
        let sc = SimpleCommand {
            argv,
            assignments,
            redirects,
            cwd: env.cwd.value(),
            cwd_candidates: env.cwd.candidate_list(),
            stdin,
            context: ctx.clone(),
            span,
        };

        // 4. Effects on the shell state. Afterwards bash assigns the last
        // word to `_`: arithmetic if the text declared `_` integer before
        // this command (not merely opaque), read in the states after it.
        // The new value of `_` is remembered (an assignment-only command's
        // effect on `_` is not modelled).
        let underscore_int = env.integer.contains("_");
        let mut outcome = self.effects(&sc, &decl_names, env)?;
        for after in [&mut outcome.ok, &mut outcome.fail] {
            match sc.argv.last() {
                Some(last) => {
                    if underscore_int {
                        arith::check_value(&last.value, &|n| after.lookup(n))?;
                    }
                    after.bind("_", last.value.clone());
                }
                None => after.forget("_", "after an assignment-only command"),
            }
        }
        self.push(sc.clone())?;
        Ok((outcome, sc))
    }

    fn effects(
        &mut self,
        sc: &SimpleCommand,
        decl_names: &BTreeMap<usize, DeclArg>,
        mut env: Env,
    ) -> Result<Outcome, Cause> {
        let name = match sc.argv.first().map(|w| &w.value) {
            None => return Ok(Outcome::same(env)),
            Some(Value::Undetermined(_)) => {
                env.make_opaque("an undetermined command word ran");
                return Ok(Outcome::same(env));
            }
            Some(Value::Literal(n)) => n.as_str(),
        };
        if name == "builtin" || name == "command" {
            // The wrapped command's effects (and assignment checks) apply as
            // if it ran unwrapped. The wrapper still makes the state opaque:
            // that `command` drops special-builtin semantics and that the
            // modelling of every wrapped builtin is unchanged by the wrapper
            // is not verified here, so the earlier, restrictive effect stays.
            let mut out = match unwrap_wrappers(&sc.argv) {
                Wrapped::At(i) => match &sc.argv[i].value {
                    Value::Literal(w) => {
                        let shifted: BTreeMap<usize, DeclArg> = decl_names
                            .range(i + 1..)
                            .map(|(k, d)| (k - i, d.clone()))
                            .collect();
                        let args = sc.argv.get(i + 1..).unwrap_or(&[]);
                        self.builtin_effects(sc, w, args, &shifted, env)?
                    }
                    Value::Undetermined(_) => Outcome::same(env),
                },
                Wrapped::Lookup | Wrapped::Unknown => Outcome::same(env),
            };
            out.ok.make_opaque(&format!("`{name}` ran"));
            out.fail.make_opaque(&format!("`{name}` ran"));
            return Ok(out);
        }
        let args = sc.argv.get(1..).unwrap_or(&[]);
        self.builtin_effects(sc, name, args, decl_names, env)
    }

    /// The effects of the builtin (or other command) `name` with `args`.
    fn builtin_effects(
        &mut self,
        sc: &SimpleCommand,
        name: &str,
        args: &[Word],
        decl_names: &BTreeMap<usize, DeclArg>,
        mut env: Env,
    ) -> Result<Outcome, Cause> {
        if SPECIAL_BUILTINS.contains(&name) {
            for (n, _) in &sc.assignments {
                env.forget_operand(n, "prefix assignment of a special builtin")?;
            }
        }
        match name {
            "cd" => return Ok(self.cd(sc, args, env)),
            "pushd" | "popd" => {
                env.cwd = Cwd::Undetermined(format!("cwd: changed by {name}"));
            }
            "break" | "continue" => {
                for a in self.accums.iter_mut() {
                    *a = Some(join_opt(a.take(), env.clone()));
                }
            }
            "let" => {
                for a in args {
                    arith::check_value(&a.value, &|n| env.lookup(n))?;
                }
                env.havoc_vars("`let` may assign variables");
            }
            "exec" if args.is_empty() => {
                if sc.stdin != env.stdin {
                    env.stdin =
                        StdinSource::Undetermined("stdin: redirected by a bare `exec`".into());
                }
            }
            "set" if !set_is_harmless(args) => env.make_opaque("`set` with an unmodelled option"),
            "printf" => {
                let mut it = args.iter();
                while let Some(a) = it.next() {
                    match &a.value {
                        Value::Literal(s) if s == "-v" => match it.next().map(|w| &w.value) {
                            Some(Value::Literal(n)) => {
                                env.int_assign(n, &undet("output of printf -v"))?;
                                env.forget_operand(n, "set by printf -v")?;
                            }
                            _ => env.make_opaque("printf -v with an undetermined name"),
                        },
                        Value::Literal(_) => {}
                        Value::Undetermined(_) => {
                            env.make_opaque("printf with an undetermined option");
                            break;
                        }
                    }
                }
            }
            n if DECL_BUILTINS.contains(&n) => self.declaration(n, args, decl_names, &mut env)?,
            n if ASSIGNING_BUILTINS.contains(&n) => {
                // The variables these set without naming them (bare `read`
                // sets REPLY, bare `mapfile` MAPFILE, getopts OPTARG /
                // OPTIND): which operand is a name is not modelled, so they
                // count as assigned whatever the operands are (superset).
                let implicit: &[&str] = match n {
                    "read" => &["REPLY"],
                    "mapfile" | "readarray" => &["MAPFILE"],
                    "getopts" => &["OPTARG", "OPTIND"],
                    _ => &[],
                };
                for t in implicit {
                    env.int_assign(t, &undet(format!("a value set by {n}")))?;
                    env.forget(t, &format!("set by {n}"));
                }
                for a in args {
                    match &a.value {
                        Value::Literal(s) => {
                            if n != "unset" {
                                env.int_assign(s, &undet(format!("a value read by {n}")))?;
                            }
                            env.forget_operand(s, &format!("assigned by {n}"))?;
                        }
                        Value::Undetermined(_) => {
                            env.make_opaque(&format!("{n} with an undetermined operand"));
                        }
                    }
                }
            }
            // User ruling 2026-11-18 (r21): `. "$HOME/.cargo/env"` (the
            // hook's HOME, unwrapped, no prefix assignment) is rustup's PATH
            // setup only and keeps the state; any other `.` / `source` does not.
            "." if sc.assignments.is_empty() && matches!(args, [w] if is_cargo_env(&w.value)) => {}
            n if OPAQUE_BUILTINS.contains(&n) => env.make_opaque(&format!("`{n}` ran")),
            _ => {}
        }
        Ok(Outcome::same(env))
    }

    fn cd(&mut self, sc: &SimpleCommand, args: &[Word], env: Env) -> Outcome {
        let new = match args {
            [] => Cwd::Undetermined("cwd: cd without an operand goes to $HOME".into()),
            [one] => match &one.value {
                Value::Undetermined(r) => {
                    Cwd::Undetermined(format!("cwd: cd to an undetermined dir ({r})"))
                }
                Value::Literal(t) if t.starts_with('-') || t.starts_with('+') || t.is_empty() => {
                    Cwd::Undetermined(format!("cwd: `cd {t}` is not a literal directory"))
                }
                Value::Literal(t) if t.starts_with('/') => match normalise_abs(t) {
                    Some(c) => Cwd::Known(c),
                    None => cd_join("/", t.trim_start_matches('/'), t),
                },
                Value::Literal(t) => {
                    let cdpath = !sc.assignments.is_empty()
                        || env.opaque
                        || env.vars.contains_key("CDPATH")
                        || env.vars.contains_key("cdpath")
                        || std::env::var_os("CDPATH").is_some_and(|v| !v.is_empty());
                    match &env.cwd {
                        _ if cdpath => {
                            Cwd::Undetermined(format!("cwd: `cd {t}` may search CDPATH"))
                        }
                        // User ruling 2026-11-19 ①: the hook's cwd is known.
                        Cwd::Hook => match cwd::hook_dir() {
                            Some(h) => cd_join(&h, t, t),
                            None => Cwd::Undetermined(format!(
                                "cwd: `cd {t}` is relative to the hook's cwd"
                            )),
                        },
                        Cwd::Undetermined(r) => Cwd::Undetermined(r.clone()),
                        Cwd::OneOf(_) => Cwd::Undetermined(format!(
                            "cwd: `cd {t}` is relative to a cwd that differs between the paths \
                             that reach here"
                        )),
                        Cwd::Known(c) => cd_join(c, t, t),
                    }
                }
            },
            _ => Cwd::Undetermined("cwd: cd with more than one operand".into()),
        };
        let mut ok = env.clone();
        ok.cwd = new;
        Outcome { ok, fail: env }
    }

    /// `(display name, base variable, value word, binding)`. The binding is
    /// `None` for array / indexed assignments.
    fn assignment(
        &mut self,
        text: &Text,
        a: &ast::Assignment,
        w: &ast::Word,
        env: &mut Env,
    ) -> Result<(String, String, Word, Option<Value>), Cause> {
        let (name, base) = match &a.name {
            ast::AssignmentName::VariableName(n) => (n.clone(), n.clone()),
            ast::AssignmentName::ArrayElementName(n, idx) => {
                if has_subst(idx) {
                    return Err(Cause::Unsupported(format!(
                        "command substitution in the index of {n}[{idx}]"
                    )));
                }
                env.arith(idx)?;
                env.havoc_vars("array index arithmetic may assign variables");
                (format!("{n}[{idx}]"), n.clone())
            }
        };
        let word_origin = text.word_origin(w);
        let lhs_len = w.value.find('=').map_or(0, |i| i + 1);
        let value_raw = w.value.get(lhs_len..).unwrap_or("").to_string();
        let value_range = lhs_len..w.value.len();
        let origin = word_origin.sub(&value_range, true);
        let (value, quoting) = match &a.value {
            ast::AssignmentValue::Scalar(v) => {
                let vorigin = if v.value == value_raw {
                    origin.clone()
                } else {
                    origin.inexact()
                };
                self.eval_word(&vorigin, &v.value, Mode::Assign, env)?
            }
            ast::AssignmentValue::Array(elems) => {
                for (k, v) in elems {
                    if let Some(k) = k {
                        // `[k]=v`: the key of an indexed array is arithmetic.
                        let (kv, _) =
                            self.eval_word(&origin.inexact(), &k.value, Mode::Field, env)?;
                        arith::check_value(&kv, &|n| env.lookup(n))?;
                        if !arith::plain_int(&kv) {
                            env.havoc_vars("an array key's arithmetic may assign variables");
                        }
                    }
                    self.eval_word(&origin.inexact(), &v.value, Mode::Field, env)?;
                }
                (undet("array assignment"), Quoting::Mixed)
            }
        };
        let value = if a.append {
            undet(format!("{name}+=: appends to a value"))
        } else {
            value
        };
        let bound = match (&a.name, &a.value) {
            (ast::AssignmentName::VariableName(_), ast::AssignmentValue::Scalar(_)) => {
                Some(value.clone())
            }
            _ => None,
        };
        let word = Word {
            raw: value_raw,
            value,
            quoting,
            span: origin.whole.clone(),
        };
        Ok((name, base, word, bound))
    }

    fn proc_subst_word(
        &mut self,
        text: &Text,
        sub: &ast::SubshellCommand,
        env: &Env,
    ) -> Result<Word, Cause> {
        self.list(text, &sub.list, env.clone(), &Context::ProcessSubstitution)?;
        let (raw, span) = match text.local(&sub.loc) {
            Some(r) => {
                let r = r.start.saturating_sub(1)..r.end;
                (
                    text.s.get(r.clone()).unwrap_or("").to_string(),
                    text.origin.map(&r),
                )
            }
            None => (String::new(), text.origin.whole.clone()),
        };
        Ok(Word {
            raw,
            value: undet("process substitution: a /dev/fd path"),
            quoting: Quoting::Unquoted,
            span,
        })
    }

    // ------------------------------------------------------------ redirects

    fn redirect(
        &mut self,
        text: &Text,
        r: &ast::IoRedirect,
        env: &mut Env,
        fallback: &Range<usize>,
    ) -> Result<Redirect, Cause> {
        match r {
            ast::IoRedirect::File(fd, kind, target) => {
                let fd = conv_fd(*fd)?;
                let mut op = match kind {
                    ast::IoFileRedirectKind::Read => RedirOp::In,
                    ast::IoFileRedirectKind::Write => RedirOp::Out,
                    ast::IoFileRedirectKind::Append => RedirOp::Append,
                    ast::IoFileRedirectKind::ReadAndWrite => RedirOp::InOut,
                    ast::IoFileRedirectKind::Clobber => RedirOp::Clobber,
                    ast::IoFileRedirectKind::DuplicateInput => RedirOp::DupIn,
                    ast::IoFileRedirectKind::DuplicateOutput => RedirOp::DupOut,
                };
                let (target, span) = match target {
                    ast::IoFileRedirectTarget::Filename(w) => {
                        let ew = self.word(text, w, Mode::Field, env)?;
                        let span = redir_span(text, w, fd);
                        (RedirTarget::Path(resolve(&env.cwd, ew.value)), span)
                    }
                    ast::IoFileRedirectTarget::Fd(n) => {
                        let n = u32::try_from(*n).map_err(|_| {
                            Cause::Unsupported(format!("negative fd {n} in a redirection"))
                        })?;
                        (RedirTarget::Fd(FdTarget::Number(n)), fallback.clone())
                    }
                    ast::IoFileRedirectTarget::ProcessSubstitution(_, sub) => {
                        self.list(text, &sub.list, env.clone(), &Context::ProcessSubstitution)?;
                        let span = match text.local(&sub.loc) {
                            Some(l) => text.origin.map(&(l.start.saturating_sub(1)..l.end)),
                            None => fallback.clone(),
                        };
                        (RedirTarget::ProcessSubstitution, span)
                    }
                    ast::IoFileRedirectTarget::Duplicate(w) => {
                        let ew = self.word(text, w, Mode::Field, env)?;
                        let span = redir_span(text, w, fd);
                        let t = match ew.value {
                            Value::Undetermined(r) => RedirTarget::Fd(FdTarget::Undetermined(r)),
                            Value::Literal(s) => match parse_fd_target(&s) {
                                Some(t) => RedirTarget::Fd(t),
                                // `>&word` with no fd number is `&>word`.
                                None if op == RedirOp::DupOut && fd.is_none() => {
                                    op = RedirOp::OutErr;
                                    RedirTarget::Path(resolve(&env.cwd, Value::Literal(s)))
                                }
                                None => RedirTarget::Fd(FdTarget::Undetermined(format!(
                                    "`{s}` is not an fd number"
                                ))),
                            },
                        };
                        (t, span)
                    }
                };
                Ok(Redirect {
                    fd,
                    op,
                    target,
                    span,
                })
            }
            ast::IoRedirect::HereDocument(fd, h) => {
                let fd = conv_fd(*fd)?;
                let quoted = !h.requires_expansion;
                let (body_local, exact) = heredoc_body(text, h);
                let body_span = match &body_local {
                    Some(r) => text.origin.map(r),
                    None => text.origin.whole.clone(),
                };
                let body = if quoted {
                    Value::Literal(h.doc.value.clone())
                } else {
                    let origin = match &body_local {
                        Some(r) => text.origin.sub(r, exact),
                        None => text.origin.inexact(),
                    };
                    self.eval_word(&origin, &h.doc.value, Mode::Heredoc, env)?.0
                };
                Ok(Redirect {
                    fd,
                    op: RedirOp::HereDoc,
                    target: RedirTarget::HereDoc(HereDoc {
                        quoted,
                        strip_tabs: h.remove_tabs,
                        body,
                        body_span,
                    }),
                    span: redir_span(text, &h.here_end, fd),
                })
            }
            ast::IoRedirect::HereString(fd, w) => {
                let fd = conv_fd(*fd)?;
                let ew = self.word(text, w, Mode::Assign, env)?;
                Ok(Redirect {
                    fd,
                    op: RedirOp::HereString,
                    target: RedirTarget::HereString(ew.value),
                    span: redir_span(text, w, fd),
                })
            }
            ast::IoRedirect::OutputAndError(w, append) => {
                let ew = self.word(text, w, Mode::Field, env)?;
                Ok(Redirect {
                    fd: None,
                    op: if *append {
                        RedirOp::AppendOutErr
                    } else {
                        RedirOp::OutErr
                    },
                    target: RedirTarget::Path(resolve(&env.cwd, ew.value)),
                    span: redir_span(text, w, None),
                })
            }
        }
    }

    // ------------------------------------------------------------ words

    fn word(
        &mut self,
        text: &Text,
        w: &ast::Word,
        mode: Mode,
        env: &mut Env,
    ) -> Result<Word, Cause> {
        let origin = text.word_origin(w);
        let (value, quoting) = self.eval_word(&origin, &w.value, mode, env)?;
        Ok(Word {
            raw: w.value.clone(),
            value,
            quoting,
            span: origin.whole,
        })
    }

    /// Statically expand one word. Every `$( )` / backquote / `<( )` inside it
    /// is evaluated (its commands are emitted) even when the value is already
    /// known to be `Undetermined`.
    fn eval_word(
        &mut self,
        origin: &Origin,
        raw: &str,
        mode: Mode,
        env: &mut Env,
    ) -> Result<(Value, Quoting), Cause> {
        let pieces = match mode {
            Mode::Heredoc => parse_heredoc_contained(raw)?,
            Mode::Field | Mode::Assign => parse::parse_word(raw)?,
        };
        let mut acc = Acc::default();
        self.pieces(origin, raw, &pieces, mode == Mode::Heredoc, env, &mut acc)?;
        let quoting = acc.quoting();
        Ok((finish(acc, mode, env), quoting))
    }

    fn pieces(
        &mut self,
        origin: &Origin,
        raw: &str,
        pieces: &[WordPieceWithSource],
        quoted: bool,
        env: &mut Env,
        acc: &mut Acc,
    ) -> Result<(), Cause> {
        let lit = if quoted {
            Seg::QuotedLit
        } else {
            Seg::UnquotedLit
        };
        let exp = if quoted {
            Seg::QuotedExp
        } else {
            Seg::UnquotedExp
        };
        for p in pieces {
            let prange = p.start_index..p.end_index;
            let praw = raw.get(prange.clone()).unwrap_or("");
            match &p.piece {
                WordPiece::Text(s) => {
                    if !quoted {
                        acc.unquoted = true;
                    }
                    if s.contains("\\\n") {
                        acc.undet("backslash-newline inside quotes / heredoc");
                    }
                    acc.push(s.clone(), lit);
                }
                WordPiece::SingleQuotedText(s) => {
                    acc.single = true;
                    acc.push(s.clone(), Seg::QuotedLit);
                }
                WordPiece::AnsiCQuotedText(s) => {
                    acc.ansic = true;
                    match decode_ansi_c(s) {
                        Some(d) => acc.push(d, Seg::QuotedLit),
                        None => acc.undet(format!("$'{s}': escape not decoded")),
                    }
                }
                WordPiece::DoubleQuotedSequence(inner) => {
                    acc.double = true;
                    self.pieces(origin, raw, inner, true, env, acc)?;
                }
                WordPiece::GettextDoubleQuotedSequence(inner) => {
                    acc.double = true;
                    self.pieces(origin, raw, inner, true, env, acc)?;
                    acc.undet("$\"..\" is translated at run time");
                }
                WordPiece::TildeExpansion(t) => {
                    acc.unquoted = true;
                    acc.expansion = true;
                    // A bare `~` that starts the word is `$HOME` (ruling 12;
                    // the result is not word-split or globbed). `~user`,
                    // `~+`, `~-`, the directory stack, and a tilde that does
                    // not start the word (`a=~/x`, `x:~/y` in an argv word,
                    // whose expansion depends on how bash classifies the
                    // word) stay Undetermined.
                    let home = match t {
                        TildeExpr::Home if !quoted && acc.segs.is_empty() => env.lookup("HOME"),
                        _ => undet("~ expands to a home directory at run time"),
                    };
                    match home {
                        Value::Literal(h) => acc.push(h, Seg::QuotedLit),
                        Value::Undetermined(r) => acc.undet(format!("~: {r}")),
                    }
                }
                WordPiece::ParameterExpansion(expr) => {
                    if !quoted {
                        acc.unquoted = true;
                    }
                    acc.expansion = true;
                    match param(expr, praw, env)? {
                        Value::Literal(s) => acc.push(s, exp),
                        Value::Undetermined(r) => acc.undet(r),
                    }
                }
                WordPiece::CommandSubstitution(s) => {
                    if !quoted {
                        acc.unquoted = true;
                    }
                    acc.expansion = true;
                    let inner = prange.start + 2..prange.end.saturating_sub(1);
                    let exact = raw.get(inner.clone()) == Some(s.as_str());
                    self.nested(s, origin.sub(&inner, exact), env)?;
                    acc.undet(format!("{praw}: output only known at run time"));
                }
                WordPiece::BackquotedCommandSubstitution(s) => {
                    if !quoted {
                        acc.unquoted = true;
                    }
                    acc.expansion = true;
                    let (prog, changed) = unescape_backquote(s, quoted);
                    let inner = prange.start + 1..prange.end.saturating_sub(1);
                    let exact = !changed && raw.get(inner.clone()) == Some(s.as_str());
                    self.nested(&prog, origin.sub(&inner, exact), env)?;
                    acc.undet(format!("{praw}: output only known at run time"));
                }
                WordPiece::EscapeSequence(s) => {
                    acc.escape = true;
                    let c = s.strip_prefix('\\').unwrap_or(s);
                    if !(c == "\n" && !quoted) {
                        acc.push(c.to_string(), Seg::QuotedLit);
                    }
                }
                WordPiece::ArithmeticExpression(e) => {
                    if !quoted {
                        acc.unquoted = true;
                    }
                    acc.expansion = true;
                    env.arith(&e.value)?;
                    env.havoc_vars("arithmetic may assign variables");
                    acc.undet(format!("{praw}: arithmetic is evaluated at run time"));
                }
            }
        }
        Ok(())
    }

    /// A `$( )` / backquote program: a subshell with a copy of the state.
    fn nested(&mut self, prog: &str, origin: Origin, env: &Env) -> Result<(), Cause> {
        let program = parse::parse(prog)?;
        let text = Text::new(prog, origin);
        self.program(&text, &program, env.clone(), &Context::CommandSubstitution)?;
        Ok(())
    }
}

/// The span of a redirection: the target word, extended back over the
/// operator (and the fd number when there is one). Falls back to the word.
fn redir_span(text: &Text, w: &ast::Word, fd: Option<u32>) -> Range<usize> {
    let local = match w.loc.as_ref().and_then(|l| text.local(l)) {
        Some(r) if text.origin.base.is_some() => r,
        _ => return text.word_origin(w).whole,
    };
    let b = text.s.as_bytes();
    let mut i = local.start;
    while i > 0 && matches!(b.get(i - 1), Some(b' ' | b'\t')) {
        i -= 1;
    }
    while i > 0 && matches!(b.get(i - 1), Some(b'<' | b'>' | b'&' | b'|' | b'-')) {
        i -= 1;
    }
    if let Some(n) = fd {
        for _ in 0..n.to_string().len() {
            if i > 0 && b.get(i - 1).is_some_and(u8::is_ascii_digit) {
                i -= 1;
            }
        }
    }
    text.origin.map(&(i..local.end))
}

/// The raw body of a heredoc (without its terminator line), and whether it
/// equals brush's `doc.value` byte for byte (then spans inside are exact).
fn heredoc_body(text: &Text, h: &ast::IoHereDocument) -> (Option<Range<usize>>, bool) {
    let r = match h.doc.loc.as_ref().and_then(|l| text.local(l)) {
        Some(r) => r,
        None => return (None, false),
    };
    let sl = text.s.get(r.clone()).unwrap_or("");
    let trimmed = sl.strip_suffix('\n').unwrap_or(sl);
    let last_start = trimmed.rfind('\n').map_or(0, |i| i + 1);
    let last = trimmed.get(last_start..).unwrap_or("");
    let last = if h.remove_tabs {
        last.trim_start_matches('\t')
    } else {
        last
    };
    let delim: String = h
        .here_end
        .value
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect();
    let end = if last == delim {
        r.start + last_start
    } else {
        r.end
    };
    let body = r.start..end;
    let exact = text.s.get(body.clone()) == Some(h.doc.value.as_str());
    (Some(body), exact)
}

fn compound_loc(cc: &ast::CompoundCommand) -> Option<&SourceSpan> {
    use ast::CompoundCommand as C;
    Some(match cc {
        C::Arithmetic(a) => &a.loc,
        C::ArithmeticForClause(f) => &f.loc,
        C::BraceGroup(b) => &b.loc,
        C::Subshell(s) => &s.loc,
        C::ForClause(f) => &f.loc,
        C::CaseClause(c) => &c.loc,
        C::IfClause(i) => &i.loc,
        C::WhileClause(w) | C::UntilClause(w) => &w.2,
        C::Coprocess(c) => &c.loc,
    })
}

/// [`resolve`] against a [`SimpleCommand::cwd`] value (for
/// `ProgramSource::File`).
pub(super) fn resolve_at(cwd: &Value, v: Value) -> Value {
    resolve(&Cwd::from_value(cwd), v)
}
