// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! fde — Forward Deployed Engineer harness.
//!
//! Every subcommand below is implemented: `resolve` (`target.rs`, `git.rs`),
//! `begin` / `ratify` / `status` / `forget` (`ratify.rs`, `state.rs`), `gate`
//! (`gate.rs`), and `evidence` / `report` (`evidence.rs`, which documents the
//! ledger layout, the order of recording checks and the completeness rule).
//! The contract below is what the implementation must meet; tests may rely
//! on it.
//!
//! # CLI contract
//!
//! `TARGET` is always an optional positional path. Omitted, the target is the
//! **git top-level containing the invocation cwd** (found by
//! `git rev-parse --show-toplevel` run in the cwd); there is no fallback to the
//! bare cwd — a cwd outside any git work tree is undetermined (exit 2). A
//! relative `TARGET` resolves against the invocation cwd. Either way the path
//! must exist, is canonicalized (symlink-resolved), and the target is the
//! canonical **git top-level** containing it (the whole top-level; sub-project
//! markers are not consulted). All fde state for it is keyed by that canonical
//! top-level only — the key is the hex SHA-256 of the canonical path — so two
//! spellings of the same repo share one state. Inherited `GIT_DIR` /
//! `GIT_WORK_TREE` / `GIT_INDEX_FILE` (and the other repo-relocating `GIT_*`)
//! are removed from the git probe and do not change the answer.
//!
//! | command | stdout | exit |
//! |---|---|---|
//! | `fde resolve [TARGET]` | JSON `{"root": "<abs path>", "state_key": "<key>"}` | 0 resolved / 2 undetermined |
//! | `fde begin [TARGET] --spec FILE` | — | 0 registered / 1 refused (spec missing, not a file, or inside the target) / 2 undetermined |
//! | `fde ratify [TARGET] [--spec FILE]` | — | 0 recorded (with the target HEAD commit and dirty flag) / 1 refused (not registered, or FILE is not the registered spec) / 2 undetermined (incl. a target HEAD or dirty flag that cannot be determined, e.g. a repository without commits; nothing is recorded) |
//! | `fde status [TARGET]` | — | **0 ratified / 1 unratified (incl. not registered, spec changed since ratification, and a TARGET that no longer exists but is — after canonicalizing its nearest existing ancestor, or spelled exactly — the root of a registered target: stderr says `gone` and names `fde forget`) / 2 undetermined (state unreadable or corrupt; a nonexistent TARGET that is not a registered root; or, once a ratification is recorded, the spec unreadable or missing)** |
//! | `fde forget [TARGET]` (**human-only**; the gate denies it to the model) | — | 0 removed the whole state directory `<state_dir>/targets/<key>/` (registration, ratification, evidence; nothing in it is parsed, so a corrupt registration is removable) / 1 refused (no state directory for that key: not registered) / 2 undetermined (cannot resolve; cannot remove — then nothing is removed, or, if the directory was already moved out of `targets/`, the reason says it is unregistered but left on disk). Key: an existing TARGET (or the cwd when omitted) names its git top-level as above; when git cannot resolve an existing TARGET (e.g. a registered root whose `.git` was removed), its canonical path instead — so such a root is still forgettable (0) and an unregistered non-git directory is refused (1, the reason naming the git failure); a TARGET that cannot be canonicalized is 2; the omitted-TARGET (cwd) case has no fallback (2); a TARGET that does not exist names its canonical-lenient absolute path (nearest existing ancestor canonicalized), else its lexical absolute path. Never touches the target |
//! | `fde gate` | JSON `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"\|"deny","permissionDecisionReason":"..."}}` (deny reason also on stderr) | 0 allow / 2 deny (PreToolUse block) |
//! | `fde evidence red\|green\|pass --req ID --cmd CMD [TARGET]` | — | 0 recorded (with the target HEAD and dirty flag observed just before CMD ran, and the ratification HEAD as `ratified_head`) / 1 refused (not ratified incl. spec changed, ID not in the ratified spec, GREEN without a RED of the same ID and CMD, PASS after a RED, or the exit status of CMD run at the target root contradicts the kind) / 2 undetermined (store, spec or ledger unreadable/corrupt; a current-fingerprint record without `ratified_head`; a legacy ratification without a recorded HEAD (re-ratify); the current target HEAD not a descendant of or equal to the ratification HEAD, or not comparable (both commits named); HEAD or dirty flag unobservable; CMD killed by a signal, timed out, could not spawn, or exited 126/127) |
//! | `fde evidence verify [TARGET]` | — | 0 complete / 1 incomplete (missing requirements named on stderr) / 2 undetermined (not ratified, zero requirements, ledger unreadable/corrupt/inconsistent, or the history rule fails: legacy ratification without HEAD, a current-fingerprint record without `ratified_head`, a counted record without HEAD, current HEAD C unobservable, or not ratification HEAD R <= C and R <= H <= C for every counted record head H, or any comparison failed) |
//! | `fde report [TARGET]` | the report text (ratification HEAD with clean/dirty, current HEAD, per-requirement state with each record's kind, HEAD and clean/dirty, unresolved and unmeasured sections; for an unratified target, `ratification: UNRATIFIED`) | **0 done: ratified and every requirement RED then GREEN / 1 not done (unratified incl. not registered and spec changed since ratification, or a requirement unresolved, unmeasured or PASS-only; named on stderr) / 2 undetermined (store, spec or ledger unreadable/corrupt/inconsistent, zero requirements, the history rule of `verify` fails, stdout write failed)** |
//!
//! A dirty work tree is recorded and shown but does not by itself change any
//! answer above; only HEAD ancestry does (see `evidence.rs`, "History").
//! Only records of the current spec fingerprint and the current ratification
//! HEAD count: re-ratifying at a different HEAD starts a new evidence epoch in
//! which earlier records are ignored (see `evidence.rs`, "Ledger layout").
//!
//! Exit 2 is always "undetermined" (or, for `gate`, deny) and is never a pass.
//! A reason is written to stderr on every non-zero exit. Argument errors
//! (clap) also exit 2.
//!
//! `fde gate` is the PreToolUse hook (`hooks/hooks.json`, matcher
//! `Bash|Edit|Write|MultiEdit|NotebookEdit`). It reads the hook payload from
//! stdin and runs under `harness_core::hook::run_pretooluse_guarded`, so a
//! panic is also exit 2 (block). An empty or unparsable payload (or a stdin
//! read error) is undetermined, i.e. deny. It denies edits (and Bash run in,
//! or naming, a guarded area) touching a target registered by `fde begin`
//! whose status is not ratified; see `gate.rs` for the full decision order.
//! One exception (user ruling 2026-11-03): while a target is unratified, an
//! exact single Bash `fde status|report|begin` invocation is allowed even in
//! or naming it; `fde ratify` stays human-only and denied, and a target whose
//! status is undetermined gets no exception. `fde forget` is human-only too:
//! while any target is registered (gone ones included), a Bash command that
//! invokes it is denied wherever it runs. A registered target whose root no
//! longer exists ("gone") no longer blocks unrelated work; its former path
//! stays denied with a reason naming `fde forget`.
//!
//! # State directory
//!
//! Ratifications and the evidence ledger live **outside** the target, in the
//! state directory: the `FDE_STATE_DIR` environment variable when set and
//! non-empty, otherwise `.fde` under the home directory. A state directory
//! that cannot be resolved is undetermined (exit 2).

mod bashwords;
mod evidence;
mod gate;
mod git;
mod ratify;
mod state;
mod target;

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use harness_core::verdict::{Determination, Verdict};

use target::ResolvedTarget;

/// Exit code for "undetermined" (and for a PreToolUse deny).
const EXIT_UNDETERMINED: i32 = 2;
/// Exit code for a determined negative answer (violation / unratified).
const EXIT_NEGATIVE: i32 = 1;

#[derive(Parser)]
#[command(
    name = "fde",
    version,
    about = "Forward Deployed Engineer harness: target resolution, spec ratification, a PreToolUse gate, and a red/green evidence ledger"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the resolved target as JSON {"root", "state_key"}.
    Resolve {
        /// Target path (default: the git top-level of the cwd; relative paths resolve against the cwd).
        target: Option<PathBuf>,
    },
    /// Register the target with the draft spec FILE (outside the target); the target becomes unratified.
    Begin {
        /// Target path (default: the git top-level of the cwd; relative paths resolve against the cwd).
        target: Option<PathBuf>,
        /// The draft spec file; must exist and live outside the target.
        #[arg(long)]
        spec: PathBuf,
    },
    /// Ratify the registered spec of the target (content fingerprint = SHA-256 of its bytes).
    Ratify {
        /// Target path (default: the git top-level of the cwd; relative paths resolve against the cwd).
        target: Option<PathBuf>,
        /// Optional: when given, must be the registered spec, else refused (exit 1).
        #[arg(long)]
        spec: Option<PathBuf>,
    },
    /// Exit 0 ratified / 1 unratified (incl. a registered target that is gone) / 2 undetermined.
    Status {
        /// Target path (default: the git top-level of the cwd; relative paths resolve against the cwd).
        target: Option<PathBuf>,
    },
    /// Human-only: remove all fde state of the target (registration, ratification, evidence); never touches the target.
    Forget {
        /// Target path (default: the git top-level of the cwd). An existing path names its git top-level; a path that no longer exists names the registered root it was (relative paths resolve against the cwd).
        target: Option<PathBuf>,
    },
    /// PreToolUse hook entry: reads the hook payload on stdin.
    Gate,
    /// Evidence ledger operations.
    Evidence {
        #[command(subcommand)]
        op: EvidenceCmd,
    },
    /// Print the ratification and evidence report for the target.
    Report {
        /// Target path (default: the git top-level of the cwd; relative paths resolve against the cwd).
        target: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum EvidenceCmd {
    /// Record that CMD was observed failing for requirement ID.
    Red(RecordArgs),
    /// Record that CMD was observed passing after a recorded RED.
    Green(RecordArgs),
    /// Record that CMD was observed passing.
    Pass(RecordArgs),
    /// Check that every requirement has complete evidence.
    Verify {
        /// Target path (default: the git top-level of the cwd; relative paths resolve against the cwd).
        target: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
struct RecordArgs {
    /// Requirement id.
    #[arg(long)]
    req: String,
    /// Command to run.
    #[arg(long)]
    cmd: String,
    /// Target path (default: the git top-level of the cwd; relative paths resolve against the cwd).
    target: Option<PathBuf>,
}

fn main() {
    let cli = Cli::parse();
    if let Cmd::Gate = cli.cmd {
        harness_core::hook::run_pretooluse_guarded("fde", run_gate);
    }
    std::process::exit(run(cli.cmd));
}

/// Read the whole PreToolUse payload from stdin. Unlike
/// `harness_core::hook::read_stdin` (which swallows read errors), a read
/// error, a payload over `harness_core::hook::MAX_STDIN_BYTES` (it would be
/// truncated) or non-UTF-8 bytes are `Undetermined`.
#[track_caller]
fn read_payload() -> Determination<String> {
    use std::io::Read;
    let cap = harness_core::hook::MAX_STDIN_BYTES;
    let mut buf = Vec::new();
    if let Err(e) = std::io::stdin().take(cap + 1).read_to_end(&mut buf) {
        return Determination::undetermined(format!("cannot read the payload on stdin: {e}"));
    }
    if buf.len() as u64 > cap {
        return Determination::undetermined(format!("the payload on stdin exceeds {cap} bytes"));
    }
    match String::from_utf8(buf) {
        Ok(s) => Determination::known(s),
        Err(e) => Determination::undetermined(format!("the payload on stdin is not UTF-8: {e}")),
    }
}

/// PreToolUse body: print the decision JSON on stdout. Allow = return
/// normally (exit 0 from the guard). Deny exits 2 itself, with the reason on
/// stderr as well. An allow whose JSON cannot be written also exits 2: exit 0
/// must never come without the allow it stands for.
fn run_gate() {
    let verdict = match read_payload() {
        Determination::Known(payload) => gate::evaluate(&payload),
        Determination::Undetermined(why) => why.into_verdict(),
    };
    let json = gate::decision_json(&verdict);
    let written = writeln!(std::io::stdout(), "{json}").and_then(|()| std::io::stdout().flush());
    match verdict {
        Verdict::Clean(_) => {
            if let Err(e) = written {
                let _ = writeln!(
                    std::io::stderr(),
                    "fde gate: could not write the allow decision ({e}), so denied"
                );
                std::process::exit(EXIT_UNDETERMINED);
            }
        }
        Verdict::Violation(why) => {
            // The exit code carries the block; a failed stdout write does not
            // change it.
            let _ = writeln!(std::io::stderr(), "fde gate: denied: {why}");
            std::process::exit(EXIT_UNDETERMINED);
        }
        Verdict::Undetermined(why) => {
            let _ = writeln!(
                std::io::stderr(),
                "fde gate: undetermined, so denied: {why}"
            );
            std::process::exit(EXIT_UNDETERMINED);
        }
    }
}

/// Print `msg` on stderr and return [`EXIT_UNDETERMINED`]. A failed stderr
/// write does not change the exit code: the code carries the answer.
fn undetermined(msg: impl std::fmt::Display) -> i32 {
    let _ = writeln!(std::io::stderr(), "fde: undetermined: {msg}");
    EXIT_UNDETERMINED
}

/// Map a verdict to the CLI exit code: 0 clean / 1 violation / 2 undetermined.
fn verdict_exit(v: Verdict) -> i32 {
    match v {
        Verdict::Clean(_) => 0,
        Verdict::Violation(why) => {
            let _ = writeln!(std::io::stderr(), "fde: {why}");
            EXIT_NEGATIVE
        }
        Verdict::Undetermined(why) => undetermined(why),
    }
}

/// Write `text` to stdout; a failed write is undetermined (exit 2), never a
/// silent success with missing output.
fn emit(text: &str) -> i32 {
    match writeln!(std::io::stdout(), "{text}") {
        Ok(()) => 0,
        Err(e) => undetermined(format!("could not write output: {e}")),
    }
}

/// Resolve the target and the state directory, then run `f`; any
/// undetermined step short-circuits to exit 2.
fn with_target<F>(target: Option<&Path>, f: F) -> i32
where
    F: FnOnce(&ResolvedTarget, &Path) -> i32,
{
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => return undetermined(format!("cannot read the current directory: {e}")),
    };
    let resolved = match target::resolve(target, &cwd) {
        Determination::Known(r) => r,
        Determination::Undetermined(why) => return undetermined(why),
    };
    let state = match target::state_dir() {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => return undetermined(why),
    };
    f(&resolved, &state)
}

fn run(cmd: Cmd) -> i32 {
    match cmd {
        Cmd::Resolve { target } => {
            let cwd = match std::env::current_dir() {
                Ok(c) => c,
                Err(e) => return undetermined(format!("cannot read the current directory: {e}")),
            };
            match target::resolve(target.as_deref(), &cwd) {
                Determination::Known(r) => match serde_json::to_string(&r) {
                    Ok(json) => emit(&json),
                    Err(e) => undetermined(format!("cannot serialize the resolved target: {e}")),
                },
                Determination::Undetermined(why) => undetermined(why),
            }
        }
        Cmd::Begin { target, spec } => with_target(target.as_deref(), |t, state| {
            verdict_exit(ratify::begin(t, &spec, state))
        }),
        Cmd::Ratify { target, spec } => with_target(target.as_deref(), |t, state| {
            verdict_exit(ratify::ratify(t, spec.as_deref(), state))
        }),
        Cmd::Status { target } => {
            if let Some(code) = status_of_gone(target.as_deref()) {
                return code;
            }
            with_target(target.as_deref(), |t, state| {
                match ratify::status(t, state) {
                    Determination::Known(ratify::Status::Ratified { .. }) => 0,
                    Determination::Known(ratify::Status::Unratified { reason }) => {
                        let _ = writeln!(std::io::stderr(), "fde: unratified: {reason}");
                        EXIT_NEGATIVE
                    }
                    Determination::Undetermined(why) => undetermined(why),
                }
            })
        }
        Cmd::Forget { target } => run_forget(target.as_deref()),
        // `main` routes Gate to the guarded PreToolUse entry before calling
        // `run`; reaching here is a wiring error, answered restrictively.
        Cmd::Gate => undetermined("gate must run through the PreToolUse guard"),
        Cmd::Evidence { op } => match op {
            EvidenceCmd::Red(a) => record(evidence::Kind::Red, a),
            EvidenceCmd::Green(a) => record(evidence::Kind::Green, a),
            EvidenceCmd::Pass(a) => record(evidence::Kind::Pass, a),
            EvidenceCmd::Verify { target } => with_target(target.as_deref(), |t, state| {
                verdict_exit(evidence::verify(t, state))
            }),
        },
        Cmd::Report { target } => {
            with_target(target.as_deref(), |t, state| {
                match evidence::report(t, state) {
                    // The text is printed first in every determined case; a
                    // failed write is undetermined whatever the verdict.
                    Determination::Known(r) => match emit(&r.text) {
                        0 => verdict_exit(r.verdict),
                        failed => failed,
                    },
                    Determination::Undetermined(why) => undetermined(why),
                }
            })
        }
    }
}

/// `fde status TARGET` for a TARGET that does not exist: when it is (any
/// spelling of, see [`target::absent_root_candidates`]) the root of a
/// registered target that is gone, the answer is exit 1 naming `fde forget`.
/// `None` means "not that case": the ordinary resolution runs (and answers a
/// nonexistent unregistered path as undetermined, exit 2). A registration
/// that cannot be read, or a root whose existence cannot be observed, is
/// exit 2 here.
fn status_of_gone(target: Option<&Path>) -> Option<i32> {
    let t = target?;
    let start = if t.is_absolute() {
        t.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(t)
    };
    match std::fs::symlink_metadata(&start) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // Exists (or cannot be stat'ed): the ordinary path decides.
        _ => return None,
    }
    let state = match target::state_dir() {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => return Some(undetermined(why)),
    };
    let candidates = match target::absent_root_candidates(&start) {
        Determination::Known(c) => c,
        Determination::Undetermined(why) => return Some(undetermined(why)),
    };
    for cand in candidates {
        let registered = match ratify::registered_at(&state, &cand) {
            Determination::Known(Some(r)) => r,
            Determination::Known(None) => continue,
            Determination::Undetermined(why) => return Some(undetermined(why)),
        };
        match target::presence(&registered.root) {
            Determination::Known(target::Presence::Gone) => {
                let root = registered.root.display();
                let _ = writeln!(
                    std::io::stderr(),
                    "fde: unratified: registered target {root} is gone (its directory no \
                     longer exists); run `fde forget {root}` to drop its state \
                     (human-only: the model must ask the human to run it)"
                );
                return Some(EXIT_NEGATIVE);
            }
            Determination::Known(target::Presence::Present) => continue,
            Determination::Undetermined(why) => return Some(undetermined(why)),
        }
    }
    None
}

/// `fde forget [TARGET]`: remove the state directory of the first candidate
/// ([`target::forget_candidates`]) that has one. Exit 0 removed / 1 not
/// registered / 2 undetermined. A refusal for a key taken from the
/// canonical-path fallback also names the git failure behind it.
fn run_forget(target: Option<&Path>) -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => return undetermined(format!("cannot read the current directory: {e}")),
    };
    let state = match target::state_dir() {
        Determination::Known(s) => s,
        Determination::Undetermined(why) => return undetermined(why),
    };
    let plan = match target::forget_candidates(target, &cwd) {
        Determination::Known(p) => p,
        Determination::Undetermined(why) => return undetermined(why),
    };
    let mut refusal = None;
    for (root, key) in &plan.candidates {
        let v = ratify::forget(&state, root, key);
        if matches!(v, Verdict::Violation(_)) {
            // Not registered under this candidate; try the next one.
            if refusal.is_none() {
                refusal = Some(v);
            }
            continue;
        }
        return verdict_exit(v);
    }
    match (refusal, plan.git_failure) {
        (Some(Verdict::Violation(why)), Some(git_why)) => {
            verdict_exit(Verdict::violation(format!(
                "{why} (keyed by its canonical path because git could not resolve it: {git_why})"
            )))
        }
        (Some(v), _) => verdict_exit(v),
        // `forget_candidates` never answers an empty list; reaching here is a
        // wiring error, answered restrictively.
        (None, _) => undetermined("no candidate state key to forget"),
    }
}

fn record(kind: evidence::Kind, a: RecordArgs) -> i32 {
    with_target(a.target.as_deref(), |t, state| {
        verdict_exit(evidence::record(kind, &a.req, &a.cmd, t, state))
    })
}
