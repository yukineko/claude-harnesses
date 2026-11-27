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
//!   one): the legacy write-then-run readings keep judging the line (the
//!   files a quoted `python3 - <<'PY'` program writes are, for a Known
//!   program, taken from these same parser effects, not from the legacy
//!   token / substring-marker reading — `interpreter/python_written_files.rs`,
//!   backlog 06c51772);
//! * one of its effects creates a link (`os.symlink`, `os.link`,
//!   `Path.symlink_to`, `Path.hardlink_to`; backlog 92f4a28d): a later write
//!   of the line may land through it, and the relink layer
//!   (`relink/nested.rs`) reads the program's original text;
//! * the neutral text does not analyse to the same commands with `pass` as
//!   the program.
//!
//! # `bash -c` / `sh -c` (user ruling 2026-11-19, S5b c)
//!
//! The literal script of a plain `bash -c SCRIPT` / `sh -c SCRIPT` (after the
//! wrappers `b5` unwraps, `sudo` / `doas` included) is run through [`prepare`] itself,
//! so the Python programs in it are judged exactly as if written directly;
//! the script word is then replaced by the script's neutral text, quoted
//! ([`shell_c_script`] lists the shapes read; any other shape keeps the
//! legacy readings only).
//!
//! # Import shadowing (user ruling 2026-11-19, S5b a)
//!
//! A program that imports `M` while the directory it runs in holds `M.py`,
//! `M/`, or a compiled `M` is Undetermined; so is one whose directory cannot
//! be listed or is not known ([`shadowed_import`]).
//!
//! User ruling 2026-11-20 (S5c): so is one for which a command run before it
//! on the same line (or its own output redirect) may write such an entry into
//! that directory, and one for which such a command's write target cannot be
//! placed ([`same_line_shadow`]; modelled writes only: output redirects, the
//! creating Table B B.1 targets, `mkdir`, `touch`).
//!
//! # Known gaps
//!
//! A here-string / piped literal program is analysed but not retired;
//! wrappers other than the ones `b5` unwraps (`xargs`, `su`, …) get no S5
//! opinion; a `bash -c` outside [`shell_c_script`]'s shapes gets none either;
//! a module imported by an imported module is not detected; a shadowing entry
//! written on the same line by a program with no effect model (`git
//! checkout`, `tar`, `curl -o`, …), or outside a `bash -c` whose script runs
//! the program, is not detected.

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
        // User ruling 2026-11-19 (S5b c): the script of `bash -c` / `sh -c`
        // is judged by this stage as if it were written directly.
        match shell_c_script(commands, cmd) {
            ShellCRead::Script(sc) => {
                let (verdict, edit) = judge_shell_c(commands, idx, cmd, &sc, judge);
                if let Some(d) = acc.record(verdict) {
                    deny.get_or_insert(d);
                }
                edits.extend(edit);
                continue;
            }
            // Explicit: not read by S5, not retired; the legacy readings and
            // S4 keep judging the original text (raise-only).
            ShellCRead::LeftToLegacy(_) => continue,
            ShellCRead::NotShellC => {}
        }
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
        let (verdict, known_effects) = judge_program(&program, text, commands, idx, judge);
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
            // A wrapper's implicit shell (`sudo -s`) runs no inline Python:
            // S4 judges it as a bare shell.
            Unwrapped::Nothing | Unwrapped::Undetermined(_) | Unwrapped::ImplicitShell => {
                return Some(Found::LeftToS4)
            }
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
                    // `false`: S5 has no top-level line to check PATH and
                    // functions against, so a bare wrapper keeps its
                    // restrictive reading here (the pre-existing one).
                    b5::literal_delivered_program(text, commands, idx, cmd, &source, k, false)?,
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
    text: &str,
    commands: &[SimpleCommand],
    idx: usize,
    judge: &dyn Fn(&str) -> Decision,
) -> (Decision, Option<bool>) {
    let Some(cmd) = commands.get(idx) else {
        return (
            undetermined("the command running the Python program is not on the line"),
            None,
        );
    };
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
    // Backlog 1dad1f35 (user ruling 2026-11-21: system harm is always Deny):
    // a statement made only of names and operators (`rm -rf /usr/lib` parses
    // as `rm - rf / usr / lib`) is valid, inert Python, so the parser proves
    // nothing about it — but real programs do not contain one, and shell text
    // pasted into a program does. Its text is also judged as shell, raise-only.
    match progeffect::python::bare_operator_statements(&p.source) {
        Ok(stmts) => {
            for stmt in stmts {
                let shell = judge(&stmt);
                if !matches!(shell, Decision::Allow) {
                    return (shell, None);
                }
            }
        }
        Err(why) => {
            return (
                undetermined(format_args!("the Python program cannot be read: {why}")),
                None,
            );
        }
    }
    if let Some(why) = shadowed_import(p, text, commands, idx, cmd) {
        return (undetermined(why), None);
    }
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
    // judging the line (raise-only). Also kept when an effect creates a link
    // ([`effect_relinks`]): the relink layer reads the original text.
    // A program that creates a link while another command of the line writes
    // through a redirect: the relink layer sees the line with the program's
    // literals blanked (a heredoc body), so the Ask is decided here, where
    // the parsed program is (backlog 92f4a28d). Only a redirect that runs
    // after the program and may land through the link path asks.
    if link_paths(&effects).iter().any(|l| {
        commands.iter().enumerate().any(|(j, o)| {
            j != idx
                && o.span.start > cmd.span.start
                && o.redirects.iter().any(|r| {
                    matches!(
                        r.op,
                        RedirOp::Out
                            | RedirOp::Append
                            | RedirOp::Clobber
                            | RedirOp::InOut
                            | RedirOp::OutErr
                            | RedirOp::AppendOutErr
                    ) && match &r.target {
                        RedirTarget::Path(v) => {
                            let w = r.written.as_ref().unwrap_or(v);
                            o.cwd != cmd.cwd || may_land_through(l, w)
                        }
                        _ => false,
                    }
                })
        })
    }) {
        return (
            undetermined(
                "the Python program creates a link and a later redirect of the line may land \
                 through it",
            ),
            None,
        );
    }
    let keep_legacy = (runs && p.inline && cmd.stdin != StdinSource::Inherited)
        || effects.iter().any(effect_relinks);
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

/// An effect of the parsed program creates a link (`os.symlink`, `os.link`,
/// `Path.symlink_to`, `Path.hardlink_to`; user ruling 92f4a28d, Python only).
/// Such a program is not retired: a later write of the line may land through
/// the link, which only the program's original text still shows.
pub(crate) fn effect_relinks(e: &progeffect::Effect) -> bool {
    matches!(
        e,
        progeffect::Effect::Fs {
            op: progeffect::FsOp::Link { .. },
            ..
        }
    )
}

/// The paths of the links the program's effects create.
fn link_paths(effects: &[progeffect::Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|e| match e {
            progeffect::Effect::Fs {
                op: progeffect::FsOp::Link { link, .. },
                ..
            } => Some(link.clone()),
            _ => None,
        })
        .collect()
}

/// A write to `target` may go through the link at `link`: it is that path or
/// below it, or the two cannot be compared (one absolute, one relative, or
/// the target undetermined). The standard streams and `/dev/null` never do.
fn may_land_through(link: &str, target: &Value) -> bool {
    let Value::Literal(t) = target else {
        return true;
    };
    if matches!(
        t.as_str(),
        "/dev/null" | "/dev/stdout" | "/dev/stderr" | "/dev/tty"
    ) {
        return false;
    }
    let norm = |p: &str| p.trim_start_matches("./").trim_end_matches('/').to_string();
    let (l, t) = (norm(link), norm(t));
    if l.starts_with('/') != t.starts_with('/') {
        return true;
    }
    t == l || t.starts_with(&format!("{l}/"))
}

/// Why the program may run a module other than the one it names (user
/// ruling 2026-11-19, S5b a), or `None` when no import can be shadowed.
///
/// `python -c` and a program on stdin put the directory the interpreter runs
/// in first on `sys.path`, so an entry of that directory named after a module
/// the program imports — `M.py`, a package directory `M/`, or a compiled
/// module `M.pyc` / `M.*.so` / `M.pyd` — is loaded instead of the standard
/// library module. Any such entry (compared ignoring ASCII case, the
/// restrictive side on a case-insensitive file system) is Undetermined. So is
/// a directory this stage cannot list (an IO error, the hook's cwd unknown,
/// a cwd the text does not determine). A directory that does not exist holds
/// nothing that can shadow (`read_dir_entries_checked` → `Known(None)`).
///
/// An entry another command of the same line may create before Python runs
/// is read by [`same_line_shadow`] (user ruling 2026-11-20, S5c).
///
/// NOT covered: the modules an imported module imports in turn, `.pth` /
/// `PYTHONPATH` / site-packages entries.
fn shadowed_import(
    p: &Program,
    text: &str,
    commands: &[SimpleCommand],
    idx: usize,
    cmd: &SimpleCommand,
) -> Option<String> {
    let modules = match progeffect::python::imported_modules(&p.source) {
        Ok(m) => m,
        Err(why) => {
            return Some(format!(
                "the Python program's imports cannot be read: {why}"
            ))
        }
    };
    if modules.is_empty() {
        return None;
    }
    let shown = modules
        .iter()
        .map(|m| format!("`{m}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let dirs = match program_dirs(cmd) {
        Ok(d) => d,
        Err(why) => {
            return Some(format!(
                "the Python program imports {shown}, which a file of the directory it runs in \
                 would replace, and {why}"
            ))
        }
    };
    for dir in &dirs {
        let entries =
            match harness_core::boundary::read_dir_entries_checked(std::path::Path::new(&dir)) {
                harness_core::verdict::Determination::Known(Some(e)) => e,
                harness_core::verdict::Determination::Known(None) => continue,
                harness_core::verdict::Determination::Undetermined(why) => {
                    return Some(format!(
                        "the Python program imports {shown}, and the directory it runs in \
                         (`{dir}`) cannot be listed for a file that would replace one of them: {}",
                        why.as_str()
                    ))
                }
            };
        for entry in &entries {
            let Some(name) = entry.file_name() else {
                continue;
            };
            let name = name.to_string_lossy();
            if let Some(m) = modules.iter().find(|m| shadows(&name, m)) {
                return Some(format!(
                    "`{dir}/{name}` would be imported instead of the module `{m}` the Python \
                     program imports (the directory it runs in is first on `sys.path`)"
                ));
            }
        }
    }
    let line = Line {
        text,
        commands,
        idx,
        cmd,
    };
    same_line_shadow(&line, &modules, &dirs, &shown)
}

/// The program's command (`commands[idx]`) on its analysed line.
struct Line<'a> {
    text: &'a str,
    commands: &'a [SimpleCommand],
    idx: usize,
    cmd: &'a SimpleCommand,
}

/// User ruling 2026-11-20 (S5c): another command of the same line that runs
/// before the program (or the program's own redirects, which the shell opens
/// first) may create, in a directory the program runs in (`dirs`), an entry
/// that shadows one of `modules` — `M.py`, `M/`, a compiled `M` ([`shadows`]),
/// or anything deeper under such a name. Returns why the program is then
/// Undetermined, or `None`.
///
/// The writes read are the modelled ones only, through the existing target
/// extraction: the output redirects (Table B B.2, `RedirTarget::Path`, already
/// resolved against the command's cwd) and the creating targets of the B.1
/// rows plus `mkdir` / `touch` (`judge::entry_writes`), after the `b5`
/// wrappers, resolved by `eval::resolve_at`. Undetermined (never Allow): a
/// write target that cannot be placed (an undetermined value, a relative path
/// after an undetermined `cd`, a relative path when the hook's cwd is not
/// known), a modelled command with an unmodelled option, a wrapper that
/// cannot be unwrapped, and a write to a program directory itself (its new
/// entry's name — `cp x .` — is not read here). A target is compared with a
/// program directory both as spelled and through the real path of its
/// longest existing ancestor; directory prefixes are compared ignoring ASCII
/// case (the restrictive side on a case-insensitive file system), and a real
/// path that cannot be read is Undetermined.
///
/// Which commands run before the program: those that start before it ends in
/// the text, unless the program is not top-level (a pipeline stage, a
/// substitution, a subshell, background) or the line has a loop or a function
/// (`b5::has_loop_or_function`) — then every command of the line.
///
/// NOT covered (no effect model, as in the S3b judge): unknown programs
/// (`git checkout`, `tar`, `curl -o`, …), `xargs` wrapping `mkdir` / `touch`,
/// a `sed -i` backup suffix, and — for a program inside `bash -c` — the
/// commands of the outer line.
fn same_line_shadow(
    line: &Line,
    modules: &[String],
    dirs: &[String],
    shown: &str,
) -> Option<String> {
    let Line {
        text,
        commands,
        idx,
        cmd,
    } = *line;
    let every = cmd.context != Context::TopLevel || b5::has_loop_or_function(text);
    // (target, is_mkdir_operand): a `mkdir` operand that IS a program
    // directory creates that directory (an entry in its parent), nothing
    // inside it, so it cannot shadow a module there (FP 61b6ba67).
    let mut targets: Vec<(Value, bool)> = Vec::new();
    for (j, c) in commands.iter().enumerate() {
        if !(j == idx || every || c.span.start < cmd.span.end) {
            continue;
        }
        targets.extend(c.redirects.iter().filter_map(|r| {
            let writes = matches!(
                r.op,
                RedirOp::Out
                    | RedirOp::Append
                    | RedirOp::Clobber
                    | RedirOp::InOut
                    | RedirOp::DupOut
                    | RedirOp::OutErr
                    | RedirOp::AppendOutErr
            );
            match &r.target {
                RedirTarget::Path(v) if writes => Some((v.clone(), false)),
                // Not a file this redirect opens (a process substitution's
                // commands are commands of the line, read in this loop).
                RedirTarget::Path(_)
                | RedirTarget::Fd(_)
                | RedirTarget::HereDoc(_)
                | RedirTarget::HereString(_)
                | RedirTarget::ProcessSubstitution => None,
            }
        }));
        if j == idx {
            // The program's own argv is the Python run, read by S5 itself.
            continue;
        }
        match command_entry_writes(c) {
            Ok(t) => targets.extend(t),
            Err(why) => {
                return Some(format!(
                    "the Python program imports {shown}, and a command before it on the line may \
                     write a file that replaces one of them: {why}"
                ))
            }
        }
    }
    let dir_spellings = match dirs
        .iter()
        .map(|d| real_spellings(d).map(|s| (d, s)))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(d) => d,
        Err(why) => {
            return Some(format!(
                "the Python program imports {shown}, and the directory it runs in cannot be \
                 compared with what a command before it on the line writes: {why}"
            ))
        }
    };
    for (t, is_mkdir) in targets {
        let placed = match place(t) {
            Ok(p) => p,
            Err(why) => {
                return Some(format!(
                    "the Python program imports {shown}, and a command before it on the line \
                     writes a target that cannot be placed ({why}), which may be a file that \
                     replaces one of them"
                ))
            }
        };
        let target_spellings = match real_spellings(&placed) {
            Ok(s) => s,
            Err(why) => {
                return Some(format!(
                    "the Python program imports {shown}, and `{placed}`, which a command before \
                     it on the line writes, cannot be compared with the directory it runs in: \
                     {why}"
                ))
            }
        };
        for (dir, ds) in &dir_spellings {
            for d in ds {
                for t in &target_spellings {
                    match entry_under(t, d) {
                        EntryUnder::Outside => {}
                        EntryUnder::TheDir if is_mkdir => {}
                        EntryUnder::TheDir => {
                            return Some(format!(
                                "the Python program imports {shown}, and a command before it on \
                                 the line writes into the directory it runs in (`{dir}`, as \
                                 `{placed}`) an entry whose name is not read here, which may \
                                 replace one of them"
                            ))
                        }
                        EntryUnder::Entry(name) => {
                            if let Some(m) = modules.iter().find(|m| shadows(&name, m)) {
                                return Some(format!(
                                    "`{placed}`, which a command before the Python program on \
                                     the line may write, would make `{dir}/{name}` be imported \
                                     instead of the module `{m}` the program imports (the \
                                     directory it runs in is first on `sys.path`)"
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// The creating targets of `c`'s argv (`judge::entry_writes`, after the
/// `b5` wrappers), resolved against its cwd; `Err` when they are not
/// determined. A command without a model has none (S3b's contract).
fn command_entry_writes(c: &SimpleCommand) -> Result<Vec<(Value, bool)>, String> {
    let mut k = 0;
    loop {
        match b5::unwrap_at(&c.argv, k) {
            Unwrapped::NotWrapper => break,
            Unwrapped::Nothing => return Ok(Vec::new()),
            Unwrapped::Undetermined(why) => {
                return Err(format!("a wrapper cannot be unwrapped ({why})"))
            }
            Unwrapped::ImplicitShell => {
                return command_entry_writes(&b5::implicit_shell(c));
            }
            Unwrapped::At(next) => k = next,
        }
    }
    let inner = SimpleCommand {
        argv: c.argv.get(k..).unwrap_or(&[]).to_vec(),
        ..c.clone()
    };
    match super::judge::entry_writes(&inner) {
        super::judge::EntryWrites::NoModel => Ok(Vec::new()),
        super::judge::EntryWrites::Undetermined(why) => Err(why),
        super::judge::EntryWrites::Targets(t) => {
            let is_mkdir = matches!(
                inner.argv.first().map(|w| &w.value),
                Some(Value::Literal(n)) if n.rsplit('/').next() == Some("mkdir")
            );
            Ok(t.into_iter()
                .map(|(_, v)| (super::eval::resolve_at(&c.cwd, v), is_mkdir))
                .collect())
        }
    }
}

/// `v` (a resolved target) as an absolute path: a relative literal is under
/// the hook's cwd (`..` taken on the real path, `eval::resolve_at`).
fn place(v: Value) -> Result<String, String> {
    let p = match v {
        Value::Literal(p) => p,
        Value::Undetermined(why) => return Err(why),
    };
    if p.starts_with('/') {
        return super::support::normalise_abs(&p)
            .ok_or_else(|| format!("`{p}` has a `..` component"));
    }
    let Some(hook) = super::eval::hook_dir() else {
        return Err(format!(
            "relative path `{p}` and the hook's cwd (the directory it runs in) is not known"
        ));
    };
    match super::eval::resolve_at(&Value::Literal(hook), Value::Literal(p)) {
        Value::Literal(abs) => Ok(abs),
        Value::Undetermined(why) => Err(why),
    }
}

/// `p` as spelled and through the real path of its longest existing ancestor
/// (the rest joined lexically); `Err` when an existing ancestor's real path
/// cannot be read.
fn real_spellings(p: &str) -> Result<Vec<String>, String> {
    let path = std::path::Path::new(p);
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    let mut at = Some(path);
    while let Some(a) = at {
        match std::fs::symlink_metadata(a) {
            Ok(_) => {
                let mut real = match std::fs::canonicalize(a) {
                    Ok(r) => r,
                    Err(e) => return Err(format!("the real path of `{}`: {e}", a.display())),
                };
                real.extend(rest.iter().rev());
                let Some(real) = real.to_str() else {
                    return Err(format!("the real path of `{p}` is not UTF-8"));
                };
                return Ok(vec![p.to_string(), real.to_string()]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(name) = a.file_name() {
                    rest.push(name);
                }
                at = a.parent();
            }
            Err(e) => return Err(format!("`{}` cannot be read: {e}", a.display())),
        }
    }
    Err(format!("no ancestor of `{p}` exists"))
}

/// Where `target` lies relative to the directory `dir` (both absolute,
/// normalised).
enum EntryUnder {
    Outside,
    /// `target` is `dir` itself.
    TheDir,
    /// `target` is the entry `name` of `dir`, or lies under it.
    Entry(String),
}

/// See [`EntryUnder`]; the prefix is compared ignoring ASCII case.
fn entry_under(target: &str, dir: &str) -> EntryUnder {
    let t = target.trim_end_matches('/');
    let d = dir.trim_end_matches('/');
    let (tl, dl) = (t.to_ascii_lowercase(), d.to_ascii_lowercase());
    if tl == dl {
        return EntryUnder::TheDir;
    }
    let Some(rest) = tl
        .strip_prefix(&dl)
        .filter(|r| r.starts_with('/') || dl.is_empty())
    else {
        return EntryUnder::Outside;
    };
    // `rest` is lowercased; take the same byte range of the original.
    let start = t.len() - rest.len();
    let name = t
        .get(start..)
        .unwrap_or(rest)
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("");
    if name.is_empty() {
        EntryUnder::TheDir
    } else {
        EntryUnder::Entry(name.to_string())
    }
}

/// The directory entry `name` is importable as the module `m`.
fn shadows(name: &str, m: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let m = m.to_ascii_lowercase();
    if name == m {
        return true;
    }
    name.strip_prefix(&m).is_some_and(|rest| {
        rest.starts_with('.')
            && [".py", ".pyc", ".pyw", ".so", ".pyd"]
                .iter()
                .any(|ext| rest.ends_with(ext))
    })
}

/// Every directory `cmd` may run in, or why that is not determined.
fn program_dirs(cmd: &SimpleCommand) -> Result<Vec<String>, String> {
    let hook = || {
        super::eval::hook_dir()
            .ok_or_else(|| "the hook's cwd (the directory it runs in) is not known".to_string())
    };
    match (&cmd.cwd_candidates, &cmd.cwd) {
        (CwdCandidates::One, Value::Literal(d)) => Ok(vec![d.clone()]),
        (CwdCandidates::One, Value::Undetermined(r)) if r == super::eval::HOOK_CWD => {
            Ok(vec![hook()?])
        }
        (CwdCandidates::OneOf(cands), _) => cands
            .iter()
            .map(|c| match c {
                Some(d) => Ok(d.clone()),
                None => hook(),
            })
            .collect(),
        (CwdCandidates::One, Value::Undetermined(r)) | (CwdCandidates::Undetermined(r), _) => {
            Err(format!("the directory it runs in is not determined ({r})"))
        }
    }
}

/// A `bash -c SCRIPT` / `sh -c SCRIPT` whose script S5 judges as if it were
/// written directly (user ruling 2026-11-19, S5b c).
struct ShellC {
    /// The script.
    script: String,
    /// Where the script word sits in the text.
    span: Range<usize>,
    /// The words after the wrappers (`argv[k]` is the shell).
    k: usize,
    /// `cd 'DIR' && { ` when the shell runs in a literal directory (the script
    /// is judged inside that group), else empty (the hook's cwd).
    prefix: String,
}

/// Commands that change what a child shell inherits (its environment, or
/// what its words expand to) in a way the script's own analysis cannot see.
const ENV_CHANGERS: &[&str] = &[
    "export", "declare", "typeset", "local", "readonly", "env", "unset", "source", ".", "eval",
    "alias", "enable", "shopt", "set",
];

/// The `bash -c` / `sh -c` S5 reads as its script, if `cmd` is one.
///
/// Only the plain shape is read: optional unwrapped wrappers (`b5`: `env`,
/// `timeout`, `nice`, `nohup`, `sudo`, `doas`, …; a wrapper's implicit shell
/// `sudo -s` is not read), the shell `bash` / `sh` bare or
/// in a system bin directory, then exactly `-c SCRIPT` with a literal script
/// (no `$0` / positional words, no other option), nothing feeding its stdin,
/// a cwd that is the hook's or one literal directory, and a line on which no
/// command assigns a variable or runs an [`ENV_CHANGERS`] word (the script is
/// analysed on its own, so a variable the line hands it would be misread —
/// `HOME`, the one variable taken from outside the text, included). Any other
/// shape is [`ShellCRead::LeftToLegacy`]: no S5 opinion here, the legacy
/// readings (and S4) judge it as before — which only over-restricts, since
/// nothing is retired.
fn shell_c_script(commands: &[SimpleCommand], cmd: &SimpleCommand) -> ShellCRead {
    use ShellCRead::{LeftToLegacy, NotShellC};
    let mut k = 0;
    loop {
        match b5::unwrap_at(&cmd.argv, k) {
            Unwrapped::NotWrapper => break,
            // A wrapper that runs nothing runs no shell.
            Unwrapped::Nothing => return NotShellC,
            Unwrapped::Undetermined(why) => return LeftToLegacy(why),
            // A wrapper's implicit shell has no `-c SCRIPT`.
            Unwrapped::ImplicitShell => {
                return LeftToLegacy("a wrapper's implicit shell is not `-c SCRIPT`".into())
            }
            Unwrapped::At(next) => k = next,
        }
    }
    let name = match cmd.argv.get(k).map(b5::literal) {
        None => return NotShellC,
        Some(Err(why)) => return LeftToLegacy(format!("the command word is undetermined ({why})")),
        Some(Ok(name)) => name,
    };
    if name.contains('/') && !b5::dirname(name).is_some_and(|d| b5::SYSTEM_BIN_DIRS.contains(&d)) {
        return NotShellC;
    }
    if !matches!(b5::basename(name), "bash" | "sh") {
        return NotShellC;
    }
    let Some([flag, script]) = cmd.argv.get(k + 1..) else {
        return LeftToLegacy("not exactly `-c SCRIPT`".into());
    };
    match b5::literal(flag) {
        Ok("-c") => {}
        Ok(_) => return LeftToLegacy("not exactly `-c SCRIPT`".into()),
        Err(why) => return LeftToLegacy(format!("an option is undetermined ({why})")),
    }
    let text = match b5::literal(script) {
        Ok(t) => t,
        Err(why) => return LeftToLegacy(format!("the script is undetermined ({why})")),
    };
    if cmd.stdin != StdinSource::Inherited {
        return LeftToLegacy("the shell's stdin is fed".into());
    }
    let untouched = commands.iter().all(|c| {
        c.assignments.is_empty()
            && c.argv.iter().all(|w| match b5::literal(w) {
                Ok(s) => !ENV_CHANGERS.contains(&b5::basename(s)),
                Err(_) => false,
            })
    });
    if !untouched {
        return LeftToLegacy("the line may change what the shell inherits".into());
    }
    let prefix = match (&cmd.cwd_candidates, &cmd.cwd) {
        (CwdCandidates::One, Value::Literal(d)) => match progeffect::shell_quote(d) {
            Some(q) => format!("cd {q} && {{ "),
            None => return LeftToLegacy("the cwd cannot be spelt".into()),
        },
        (CwdCandidates::One, Value::Undetermined(r)) if r == super::eval::HOOK_CWD => String::new(),
        _ => return LeftToLegacy("the cwd is not one known directory".into()),
    };
    ShellCRead::Script(ShellC {
        script: text.to_string(),
        span: script.span.clone(),
        k,
        prefix,
    })
}

/// What [`shell_c_script`] reads in one command.
enum ShellCRead {
    /// Not a `bash` / `sh` command (or a wrapper that runs nothing).
    NotShellC,
    /// A `bash` / `sh` this stage does not read (the reason why): no S5
    /// opinion, the legacy readings and S4 judge the original text, nothing
    /// is retired.
    LeftToLegacy(String),
    /// A plain `-c SCRIPT` S5 judges.
    Script(ShellC),
}

/// The S5 verdict of the script of `sc` ([`prepare`] on it), and the edit
/// that retires its Known programs from the outer line's legacy readings:
/// the script word is replaced by the script's neutral text, quoted.
fn judge_shell_c(
    commands: &[SimpleCommand],
    idx: usize,
    cmd: &SimpleCommand,
    sc: &ShellC,
    judge: &dyn Fn(&str) -> Decision,
) -> (Decision, Option<Edit>) {
    let suffix = if sc.prefix.is_empty() { "" } else { "\n}" };
    let judged = format!("{}{}{suffix}", sc.prefix, sc.script);
    let inner = prepare(&judged, judge);
    let edit = (|| {
        let neutral = inner.neutral.as_deref()?;
        let n = neutral
            .strip_prefix(sc.prefix.as_str())?
            .strip_suffix(suffix)?;
        // Whatever the script's programs do, another command of the line
        // that may run a file keeps the legacy readings.
        if !site_retirable(commands, idx, cmd, true) {
            return None;
        }
        Some(Edit {
            span: sc.span.clone(),
            with: progeffect::shell_quote(n)?,
            idx,
            k: sc.k,
            expect: Some(n.to_string()),
        })
    })();
    (inner.verdict, edit)
}

/// The neutralising edit of a Known program, when it may be retired.
struct Edit {
    span: Range<usize>,
    with: String,
    idx: usize,
    k: usize,
    /// The inline program the edited command must run (`None`: `pass`).
    expect: Option<String>,
}

/// `cmd` sits where its program may be retired from the legacy readings
/// (see the module doc, "When a program is NOT retired").
fn site_retirable(
    commands: &[SimpleCommand],
    idx: usize,
    cmd: &SimpleCommand,
    has_effect: bool,
) -> bool {
    match cmd.context {
        Context::CommandSubstitution | Context::ProcessSubstitution => return false,
        Context::Pipeline { pos, len } if pos + 1 < len => return false,
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
        return false;
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
    !((has_effect || writes_out)
        && commands
            .iter()
            .enumerate()
            .any(|(j, o)| j != idx && may_run_file(o)))
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
    if !site_retirable(commands, idx, cmd, has_effect) {
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
        expect: None,
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
            // A shell whose program is not on the line may run any file.
            Unwrapped::Undetermined(_) | Unwrapped::ImplicitShell => return true,
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
/// commands with `pass` as each retired program (and, for a `bash -c` /
/// `sh -c` script, with the script's neutral text as its program).
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
        let ok = match (program_source(&inner)?, &e.expect) {
            (ProgramSource::InlineLiteral(p), Some(x)) => &p == x,
            (ProgramSource::InlineLiteral(p), None) => p == "pass",
            (ProgramSource::Stdin(StdinSource::Literal(b)), None) => b.trim_end() == "pass",
            _ => false,
        };
        if !ok {
            return None;
        }
    }
    Some(out)
}
