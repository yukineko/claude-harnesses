//! Stage S5 `python-parser`: an inline Python program (`python3 -c PROG`,
//! `python3 - <<'EOF' … EOF`, a literal program piped in) is read with a real
//! Python parser (crate `progeffect`) and its effects are judged with the
//! same effect × target-path rules as shell (`SUBSET.md` § Python (S5)).
//!
//! User rulings this stage implements (quoted in `SUBSET.md`):
//!
//! * 2026-11-17 (structural direction): "no per-issue patches; embedded
//!   Python gets a real parser and its effects are judged with the same
//!   effect × target rules as shell".
//! * Text vs code is decided by the parser: string literals, comments and the
//!   literal parts of f-strings are data; only calls are effects.
//! * Undetermined (dynamic dispatch, a parse error, a non-literal operand, an
//!   alias that cannot be followed) → Ask (main) / Deny (subagent).
//! * System harm stays Deny through any wrapper.
//! * `python3 x.py` stays B.5 Undetermined (S4); S5 covers inline programs
//!   only.
//! * Legacy string readings of the program are retired ONLY where the parser
//!   gives a Known verdict; where it is Undetermined they are kept, raise-only
//!   (the S3b pattern).
//!
//! # How it is wired
//!
//! [`prepare`] runs before the legacy readings (`readings.rs`) and returns
//!
//! * `verdict` — the S5 verdict of every inline Python program on the line
//!   (folded into the legacy verdict as `max`), and
//! * `neutral` — the text the legacy readings judge instead of the original:
//!   each program the parser read as Known is replaced by `pass` (the `-c`
//!   operand, or the here-document body), so the legacy string readings no
//!   longer read its string literals as commands. `None` (judge the
//!   original) when nothing is retired.
//!
//! Each effect is judged by the FULL detector on its shell spelling
//! (`progeffect::Effect::shell_spelling`: `os.remove('/etc/x')` → `rm --
//! '/etc/x'`, `open(p, 'w')` → `: > 'p'`, `subprocess.run([...])` → the
//! quoted argv, `os.system(s)` → `sh -c 's'`), prefixed with `cd 'DIR' && `
//! when the command's cwd is a literal directory. This is the precedent of
//! `judge_candidates` ("until `classify` lands, judge a placed path by the
//! whole detector on synthetic text"): one rule table, no drift.
//!
//! # When a program is NOT retired (legacy readings kept, raise-only)
//!
//! * its analysis is Undetermined (any reason above);
//! * it sits inside a command / process substitution, or inside a word,
//!   redirect or here-document body of another command, or is a non-last
//!   pipeline stage (its output feeds another command);
//! * it has an effect (or an output redirect) AND another command on the
//!   line may run a file (an interpreter / shell, `source` / `.` / `eval`, a
//!   path as command word, an undetermined command word, a wrapper running
//!   one): the legacy write-then-run readings keep judging the line;
//! * the neutral text does not analyse to the same commands with `pass` as
//!   the program.
//!
//! # Known gaps
//!
//! A here-string / piped literal program is analysed but not retired; Python
//! nested in `bash -c '…'` is judged by the legacy readings only; wrappers
//! other than the ones `b5` unwraps (`sudo`, `xargs`, …) get no S5 opinion;
//! a standard-library module shadowed by a file in the working directory
//! (`-c` puts the cwd first on `sys.path`) is not detected.

use std::cell::Cell;
use std::ops::Range;

use super::b5::{self, Family, Unwrapped};
use super::{
    analyze, program_source, Analysis, Context, CwdCandidates, ProgramSource, RedirOp, RedirTarget,
    SimpleCommand, StdinSource, Value, Word,
};
use crate::detect::VerdictAcc;
use crate::model::Decision;

/// The start of the reason of an effect judged by the full detector:
/// `the Python program calls `CALL` on `PATH` (judged as `SPELLING`): INNER`
/// (`rule_id` classifies INNER).
pub(crate) const S5_EFFECT_PREFIX: &str = "the Python program calls `";

/// Nesting cap of S5 → full detector → S5 recursion.
const MAX_DEPTH: usize = 4;

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

struct DepthGuard;

impl DepthGuard {
    fn enter() -> Option<DepthGuard> {
        DEPTH.with(|d| {
            if d.get() >= MAX_DEPTH {
                return None;
            }
            d.set(d.get() + 1);
            Some(DepthGuard)
        })
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// What [`prepare`] hands the readings.
pub(crate) struct Prepared {
    /// The text the legacy readings judge (`None`: the original).
    pub neutral: Option<String>,
    /// The S5 verdict (`Allow` = no objection, never "safe").
    pub verdict: Decision,
}

fn no_opinion() -> Prepared {
    Prepared {
        neutral: None,
        verdict: Decision::Allow,
    }
}

fn undetermined(reason: impl std::fmt::Display) -> Decision {
    Decision::ask(format!(
        "{reason} — blastguard cannot judge what the Python program would do (S5: Undetermined)"
    ))
}

/// See the module doc. `judge` is the full detector.
pub(crate) fn prepare(text: &str, judge: &dyn Fn(&str) -> Decision) -> Prepared {
    let Some(_guard) = DepthGuard::enter() else {
        return Prepared {
            neutral: None,
            verdict: undetermined("the command nests Python programs too deeply"),
        };
    };
    let script = match analyze(text) {
        Analysis::Known(s) => s,
        // A line outside the shell subset gives no S5 opinion: the legacy
        // readings judge it unchanged.
        Analysis::Undetermined(_) => return no_opinion(),
    };
    let commands = &script.commands;
    let mut acc = VerdictAcc::default();
    // The first Deny. The loop does not stop there: every Known program is
    // still retired, so the legacy readings do not re-read string literals
    // of a program whose real effect was already refused (the reason the
    // caller sees is that effect's, not a literal's).
    let mut deny: Option<Decision> = None;
    let mut edits: Vec<Edit> = Vec::new();
    for (idx, cmd) in commands.iter().enumerate() {
        let Some(found) = python_program(text, commands, idx, cmd) else {
            continue;
        };
        let program = match found {
            Found::Undetermined(why) => {
                if let Some(d) = acc.record(undetermined(why)) {
                    deny.get_or_insert(d);
                }
                continue;
            }
            // Not retired, no S5 verdict: the legacy readings and S4 judge it.
            Found::LeftToS4 => continue,
            Found::Program(p) => p,
        };
        let (verdict, known_effects) = judge_program(&program, commands, cmd, judge);
        if let Some(d) = acc.record(verdict) {
            deny.get_or_insert(d);
        }
        let Some(has_effect) = known_effects else {
            continue;
        };
        // A program that cannot be retired leaves the others as they are.
        if let Some(edit) = retirable(text, commands, idx, cmd, &program, has_effect) {
            edits.push(edit);
        }
    }
    Prepared {
        neutral: neutral_text(text, commands, edits),
        verdict: deny.unwrap_or_else(|| acc.finish()),
    }
}

/// The program text and how Python receives it.
struct Program {
    source: String,
    argv: Vec<Option<String>>,
    /// The words after the wrappers (`argv[k]` is the interpreter).
    k: usize,
    /// Where the program sits in the text, when it can be retired.
    site: Option<Site>,
    inline: bool,
}

enum Site {
    /// The `-c` operand word (`glued`: the `-c` flag prefix of a `-cPROG`
    /// word).
    Word {
        span: Range<usize>,
        glued: Option<String>,
    },
    /// The here-document body.
    HereDoc { span: Range<usize> },
}

enum Found {
    Program(Program),
    Undetermined(String),
    /// S5 cannot read this command's program and leaves it to S4: the text
    /// is not neutralized, so every legacy reading still judges it, and S4
    /// resolves the same states restrictively (b5 `Unwrapped::Undetermined`
    /// and `ProgramSource::Undetermined` are Undetermined there; a wrapper
    /// with nothing to run, `Unwrapped::Nothing`, runs no Python).
    LeftToS4,
}

/// How the interpreter's own arguments lay out the program.
enum Layout {
    Inline {
        word: usize,
        glued: Option<String>,
        rest: usize,
    },
    Stdin {
        dash: bool,
        rest: usize,
    },
    /// A program file, `-m`, or an option not modelled: no S5 opinion (S4
    /// judges it).
    Other,
}

fn layout(args: &[Word]) -> Result<Layout, String> {
    let mut i = 0;
    while let Some(a) = args.get(i) {
        let Ok(v) = b5::literal(a) else {
            return Ok(Layout::Other);
        };
        if v == "-" {
            return Ok(Layout::Stdin {
                dash: true,
                rest: i + 1,
            });
        }
        if v == "--" {
            i += 1;
            break;
        }
        if v.starts_with("--") {
            return Ok(Layout::Other);
        }
        let Some(flags) = v.strip_prefix('-').filter(|f| !f.is_empty()) else {
            break;
        };
        i += 1;
        for (at, ch) in flags.char_indices() {
            let end = at + ch.len_utf8();
            let rest = flags.get(end..).unwrap_or("");
            match ch {
                'c' if !rest.is_empty() => {
                    return Ok(Layout::Inline {
                        word: i - 1,
                        glued: Some(format!("-{}", flags.get(..end).unwrap_or("c"))),
                        rest: i,
                    })
                }
                'c' if i < args.len() => {
                    return Ok(Layout::Inline {
                        word: i,
                        glued: None,
                        rest: i + 1,
                    })
                }
                'c' => return Ok(Layout::Other),
                'i' => {
                    return Err("`python -i` keeps reading code from stdin after the program".into())
                }
                'x' => {
                    return Err(
                        "`python -x` skips the program's first line, so it does not run the text read here"
                            .into(),
                    )
                }
                'W' | 'X' => {
                    if rest.is_empty() {
                        i += 1;
                    }
                    break;
                }
                c if "bBdEhIOPqsSuvVR".contains(c) => {}
                _ => return Ok(Layout::Other),
            }
        }
    }
    Ok(match args.get(i) {
        None => Layout::Stdin {
            dash: false,
            rest: i,
        },
        Some(_) => Layout::Other,
    })
}

fn argv_words(words: &[Word]) -> impl Iterator<Item = Option<String>> + '_ {
    words
        .iter()
        .map(|w| b5::literal(w).ok().map(str::to_string))
}

/// The inline Python program `commands[idx]` runs, if any.
fn python_program(
    text: &str,
    commands: &[SimpleCommand],
    idx: usize,
    cmd: &SimpleCommand,
) -> Option<Found> {
    let mut k = 0;
    loop {
        match b5::unwrap_at(&cmd.argv, k) {
            Unwrapped::NotWrapper => break,
            // S4 judges a wrapper it cannot unwrap.
            Unwrapped::Nothing | Unwrapped::Undetermined(_) => return Some(Found::LeftToS4),
            Unwrapped::At(next) => k = next,
        }
    }
    let name = b5::literal(cmd.argv.get(k)?).ok()?;
    if name.contains('/') && !b5::dirname(name).is_some_and(|d| b5::SYSTEM_BIN_DIRS.contains(&d)) {
        return None;
    }
    if b5::family(b5::basename(name)) != Some(Family::Python) {
        return None;
    }
    let args = cmd.argv.get(k + 1..).unwrap_or(&[]);
    let lay = match layout(args) {
        Ok(l) => l,
        Err(why) => return Some(Found::Undetermined(why)),
    };
    let inner = SimpleCommand {
        argv: cmd.argv.get(k..).unwrap_or(&[]).to_vec(),
        ..cmd.clone()
    };
    let source = program_source(&inner)?;
    let mismatch = || {
        Some(Found::Undetermined(
            "blastguard reads this Python command line two different ways".into(),
        ))
    };
    match lay {
        Layout::Other => None,
        Layout::Inline { word, glued, rest } => {
            let ProgramSource::InlineLiteral(prog) = source else {
                return match source {
                    // An undetermined program: S4 / the legacy readings.
                    ProgramSource::Undetermined(_) => Some(Found::LeftToS4),
                    _ => mismatch(),
                };
            };
            let w = args.get(word)?;
            let expected = match &glued {
                Some(prefix) => b5::literal(w).ok()?.strip_prefix(prefix.as_str())?,
                None => b5::literal(w).ok()?,
            };
            if expected != prog {
                return mismatch();
            }
            let mut argv = vec![Some("-c".to_string())];
            argv.extend(argv_words(args.get(rest..).unwrap_or(&[])));
            Some(Found::Program(Program {
                source: prog,
                argv,
                k,
                site: Some(Site::Word {
                    span: w.span.clone(),
                    glued,
                }),
                inline: true,
            }))
        }
        Layout::Stdin { dash, rest } => {
            let ProgramSource::Stdin(src) = &source else {
                return match source {
                    ProgramSource::Undetermined(_) => Some(Found::LeftToS4),
                    _ => mismatch(),
                };
            };
            let mut argv = vec![Some(if dash { "-" } else { "" }.to_string())];
            argv.extend(argv_words(args.get(rest..).unwrap_or(&[])));
            let (body, site) = match src {
                StdinSource::Literal(body) => (body.clone(), heredoc_site(cmd, body)),
                _ => (
                    b5::literal_delivered_program(text, commands, idx, cmd, &source, k)?,
                    None,
                ),
            };
            Some(Found::Program(Program {
                source: body,
                argv,
                k,
                site,
                inline: false,
            }))
        }
    }
}

/// The here-document that is the command's only stdin redirect and holds
/// exactly `body`.
fn heredoc_site(cmd: &SimpleCommand, body: &str) -> Option<Site> {
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
    let [r] = ins.as_slice() else { return None };
    match &r.target {
        RedirTarget::HereDoc(h) if h.body == Value::Literal(body.to_string()) => {
            Some(Site::HereDoc {
                span: h.body_span.clone(),
            })
        }
        _ => None,
    }
}

/// The S5 verdict of one program, and — when its analysis is Known —
/// whether it has any effect.
fn judge_program(
    p: &Program,
    commands: &[SimpleCommand],
    cmd: &SimpleCommand,
    judge: &dyn Fn(&str) -> Decision,
) -> (Decision, Option<bool>) {
    if b5::sets_injecting_var(commands) {
        return (
            undetermined(
                "the line sets a variable (`PYTHON*`, `PATH`, …) that changes which Python runs \
                 or which modules it imports",
            ),
            None,
        );
    }
    let effects = match progeffect::python::analyze(&p.source, &p.argv) {
        progeffect::Analysis::Known(e) => e,
        progeffect::Analysis::Undetermined(why) => {
            let verdict = undetermined(format_args!("the Python program cannot be read: {why}"));
            return (verdict, None);
        }
    };
    let runs = effects.iter().any(progeffect::Effect::runs_program);
    if runs && effects.iter().any(progeffect::Effect::writes_file) {
        return (
            undetermined(
                "the Python program both writes a file and runs a program, which may run what \
                 it wrote",
            ),
            None,
        );
    }
    // A `-c` program that runs a command while the line feeds Python's stdin:
    // the command may read that stdin (`echo 'rm -rf ~' | python3 -c
    // 'os.system("bash")'`). The effects are still judged here, but the
    // program is not retired: the legacy readings of the stdin text keep
    // judging the line (raise-only).
    let keep_legacy = runs && p.inline && cmd.stdin != StdinSource::Inherited;
    let prefix = match (&cmd.cwd_candidates, &cmd.cwd) {
        (CwdCandidates::One, Value::Literal(d)) => match progeffect::shell_quote(d) {
            Some(q) => format!("cd {q} && "),
            None => return (undetermined("the cwd cannot be spelt"), None),
        },
        (CwdCandidates::One, Value::Undetermined(r)) if r == super::eval::HOOK_CWD => String::new(),
        _ if effects.is_empty() => String::new(),
        _ => {
            return (
                undetermined(
                    "the Python program has effects and the directory it runs in is not \
                     determined",
                ),
                None,
            )
        }
    };
    let mut acc = VerdictAcc::default();
    for e in &effects {
        let Some(spelling) = e.shell_spelling() else {
            return (
                undetermined(format_args!(
                    "the Python program calls `{}` with an operand that cannot be spelt",
                    e.call()
                )),
                None,
            );
        };
        // A shell command line (`os.system(CMD)`) is judged as CMD itself —
        // the command `/bin/sh -c CMD` runs — rather than through a `sh -c`
        // wrapper reading, so it gets every top-level reading a typed CMD
        // gets. With a literal cwd it runs inside a brace group after the
        // `cd` (the newline ends a trailing comment of CMD).
        let (spelling, judged) = match e {
            progeffect::Effect::Shell { command, .. } => {
                let judged = if prefix.is_empty() {
                    command.clone()
                } else {
                    format!("{prefix}{{ {command}\n}}")
                };
                (command.clone(), judged)
            }
            _ => (spelling.clone(), format!("{prefix}{spelling}")),
        };
        let on: String = e.paths().iter().map(|p| format!(" `{p}`")).collect();
        let on = if on.is_empty() {
            String::new()
        } else {
            format!(" on{on}")
        };
        let shown = format!(
            "{S5_EFFECT_PREFIX}{}`{on} (judged as `{spelling}`): ",
            e.call()
        );
        let v = match judge(&judged) {
            Decision::Allow => Decision::Allow,
            Decision::Ask(r) => Decision::Ask(format!("{shown}{r}")),
            Decision::Deny(r) => Decision::Deny(format!("{shown}{r}")),
        };
        if let Some(deny) = acc.record(v) {
            return (deny, (!keep_legacy).then_some(true));
        }
    }
    (acc.finish(), (!keep_legacy).then_some(!effects.is_empty()))
}

/// The neutralising edit of a Known program, when it may be retired.
struct Edit {
    span: Range<usize>,
    with: String,
    idx: usize,
    k: usize,
}

fn retirable(
    text: &str,
    commands: &[SimpleCommand],
    idx: usize,
    cmd: &SimpleCommand,
    p: &Program,
    has_effect: bool,
) -> Option<Edit> {
    let site = p.site.as_ref()?;
    match cmd.context {
        Context::CommandSubstitution | Context::ProcessSubstitution => return None,
        Context::Pipeline { pos, len } if pos + 1 < len => return None,
        Context::Pipeline { .. } | Context::TopLevel | Context::Subshell | Context::Background => {}
    }
    let inside = |r: &Range<usize>| r.start <= cmd.span.start && cmd.span.end <= r.end;
    let enclosed = commands.iter().enumerate().any(|(j, o)| {
        j != idx
            && (o.argv.iter().any(|w| inside(&w.span))
                || o.assignments.iter().any(|(_, w)| inside(&w.span))
                || o.redirects.iter().any(|r| {
                    inside(&r.span)
                        || matches!(&r.target, RedirTarget::HereDoc(h) if inside(&h.body_span))
                }))
    });
    if enclosed {
        return None;
    }
    let writes_out = cmd.redirects.iter().any(|r| {
        matches!(
            r.op,
            RedirOp::Out
                | RedirOp::Append
                | RedirOp::Clobber
                | RedirOp::InOut
                | RedirOp::OutErr
                | RedirOp::AppendOutErr
        ) && matches!(r.target, RedirTarget::Path(_))
    });
    if (has_effect || writes_out)
        && commands
            .iter()
            .enumerate()
            .any(|(j, o)| j != idx && may_run_file(o))
    {
        return None;
    }
    let (span, with) = match site {
        Site::Word { span, glued } => (
            span.clone(),
            format!("{}pass", glued.as_deref().unwrap_or("")),
        ),
        Site::HereDoc { span } => {
            let raw = text.get(span.clone())?;
            let nl = if raw.ends_with('\n') { "\n" } else { "" };
            (span.clone(), format!("pass{nl}"))
        }
    };
    Some(Edit {
        span,
        with,
        idx,
        k: p.k,
    })
}

/// A command that may run a file another command wrote.
fn may_run_file(c: &SimpleCommand) -> bool {
    if program_source(c).is_some() {
        return true;
    }
    let mut k = 0;
    loop {
        match b5::unwrap_at(&c.argv, k) {
            Unwrapped::NotWrapper => break,
            Unwrapped::Nothing => return false,
            Unwrapped::Undetermined(_) => return true,
            Unwrapped::At(next) => k = next,
        }
    }
    let Some(first) = c.argv.get(k) else {
        return false;
    };
    let Ok(name) = b5::literal(first) else {
        return true;
    };
    name.contains('/')
        || matches!(b5::basename(name), "source" | "." | "eval" | "exec")
        || c.argv.iter().any(|w| match b5::literal(w) {
            Ok(s) => b5::family(b5::basename(s)).is_some() || matches!(s, "source" | "eval" | "."),
            Err(_) => false,
        })
}

/// The text with every edit applied, checked to analyse to the same
/// commands with `pass` as each retired program.
fn neutral_text(text: &str, commands: &[SimpleCommand], mut edits: Vec<Edit>) -> Option<String> {
    edits.sort_by_key(|e| (e.span.start, e.span.end));
    edits.dedup_by(|a, b| a.span == b.span && a.with == b.with);
    if edits.is_empty() {
        return None;
    }
    if edits.windows(2).any(|w| w[0].span.end > w[1].span.start) {
        return None;
    }
    let mut out = text.to_string();
    for e in edits.iter().rev() {
        if !out.is_char_boundary(e.span.start) || !out.is_char_boundary(e.span.end) {
            return None;
        }
        out.replace_range(e.span.clone(), &e.with);
    }
    let Analysis::Known(after) = analyze(&out) else {
        return None;
    };
    if after.commands.len() != commands.len() {
        return None;
    }
    for e in &edits {
        let c = after.commands.get(e.idx)?;
        let inner = SimpleCommand {
            argv: c.argv.get(e.k..).unwrap_or(&[]).to_vec(),
            ..c.clone()
        };
        let ok = match program_source(&inner)? {
            ProgramSource::InlineLiteral(p) => p == "pass",
            ProgramSource::Stdin(StdinSource::Literal(b)) => b.trim_end() == "pass",
            _ => false,
        };
        if !ok {
            return None;
        }
    }
    Some(out)
}
