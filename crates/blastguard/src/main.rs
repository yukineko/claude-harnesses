// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! blastguard — a Claude Code PreToolUse hook that denies project-destroying
//! Bash commands and file operations.
//!
//! Contract (shared by every plugin in this repo): a hook must NEVER break the
//! user's turn. We read the tool call from stdin, decide allow/deny/ask with a
//! pure function, and — on anything but an allow — print the single-line
//! PreToolUse JSON. We always exit 0.
//!
//! Two things that look alike but are NOT the same, and whose conflation was a
//! defect here:
//!
//!   * "never break the turn" = never crash the session — REQUIRED, and kept.
//!   * "never block the command" = allow even when undecided — NOT required,
//!     and was the bug.
//!
//! A silent exit 0 with no output IS an allow. So the previous contract ("on
//! any panic we stay silent and exit 0") meant that a panic anywhere in the
//! analyser silently ALLOWED the very command it had failed to analyse.
//! `#![deny(clippy::panic)]` does not prevent that: it stops explicit `panic!`
//! only, not index-out-of-bounds, non-char-boundary string slicing, arithmetic
//! overflow or unwrap-on-None. So the analysis now runs inside
//! `std::panic::catch_unwind` and a caught panic becomes a DENY, which is a
//! normal, non-breaking outcome.
//!
//! # What is still a silent allow, and what stopped being one
//!
//! This paragraph used to read "Empty/invalid input and an unmatched tool are
//! still a silent allow — those are cases where we successfully determined
//! there is nothing to judge, not cases where we failed to determine
//! anything." Two of those three were, the middle one was NOT, and the
//! sentence's own criterion is what convicts it:
//!
//!   * EMPTY stdin — nothing to judge. Determined. Silence is accurate.
//!   * an UNMATCHED TOOL (`Read`, `Grep`, …) — outside blastguard's
//!     jurisdiction. Determined. Silence is accurate, and folding it in would
//!     make the hook prompt on every file read.
//!   * INVALID input — a tool call IS being made and blastguard could not read
//!     it. That is the definition of failing to determine, filed under
//!     "successfully determined there is nothing to judge". It allowed
//!     precisely the call it had failed to read.
//!
//! So an unreadable payload now emits `Ask` (hardened to `Deny` where no human
//! can answer) rather than exiting silently, and the same applies one level in:
//! a MATCHED tool whose operand is missing or not a string is a refusal, not an
//! allow. See `detect::unreadable_operand`.
//!
//! The docstring outlived the behaviour it described by describing the
//! behaviour someone intended. CLAUDE.md 4 calls that a trap for the next
//! reviewer, and it worked as one: the analysis side of this crate had applied
//! the three-answer rule to 25-odd sub-analysers while its own front door
//! returned two, and this paragraph is why that read as deliberate.

use blastguard::model::Decision;
use blastguard::rule_id::INTERNAL_ERROR_REASON;
use blastguard::scope::SafeRoots;
use blastguard::{detect, hookio, interactive, retro, rule_id};
use harness_core::hook::{self, HookInput};
use std::process::exit;

fn main() {
    // Minimal CLI surface: version/help/retro short-circuit before touching stdin.
    let args: Vec<String> = std::env::args().skip(1).collect();
    for (i, arg) in args.iter().enumerate() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("blastguard {}", env!("CARGO_PKG_VERSION"));
                exit(0);
            }
            "--help" | "-h" => {
                print_help();
                exit(0);
            }
            "retro" => {
                exit(run_retro(&args[i + 1..]));
            }
            _ => {}
        }
    }
    // never-break-a-turn: always exit 0. Panics inside the ANALYSIS are caught
    // by `run` itself and turned into a deny; `run_hook`'s own catch remains the
    // outer backstop for anything outside that scope (stdin read, JSON print).
    hook::run_hook(run);
}

/// Parsed form of `blastguard retro`'s CLI arguments.
///
/// Split out of `run_retro` as a pure function (`Result` instead of
/// `eprintln!` + `return 2`, no printing, no filesystem access) specifically
/// so the flag-parsing logic — most notably "`--rule-id` alone implies
/// `--list`" — is reachable from a unit test. `run_retro` was previously the
/// only caller of this logic, and `main.rs` had no test module at all: that
/// rule shipped with zero coverage, and a regression to the old "print the
/// unfiltered table" behaviour would have kept the suite green.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RetroArgs {
    /// Last `--dir <path>` or resolved `--project <path>` seen, in the order
    /// the flags were given (matches the original mutable-variable behaviour:
    /// whichever of `--dir`/`--project` appears LAST wins).
    dir: Option<std::path::PathBuf>,
    /// Listing mode — already folded together with the `--rule-id`-implies-
    /// `--list` rule below, so callers never need to re-derive it.
    list: bool,
    rule_filter: Option<String>,
}

fn parse_retro_args(rest: &[String]) -> Result<RetroArgs, String> {
    let mut dir: Option<std::path::PathBuf> = None;
    let mut list = false;
    let mut rule_filter: Option<String> = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dir" => match it.next() {
                Some(v) => dir = Some(std::path::PathBuf::from(v)),
                None => return Err("blastguard retro: --dir needs a path".to_string()),
            },
            "--project" => match it.next() {
                Some(v) => dir = retro::transcript_dir_for(std::path::Path::new(v)),
                None => return Err("blastguard retro: --project needs a path".to_string()),
            },
            "--list" => list = true,
            "--rule-id" => match it.next() {
                Some(v) => rule_filter = Some(v.clone()),
                None => return Err("blastguard retro: --rule-id needs a rule id".to_string()),
            },
            other => return Err(format!("blastguard retro: unknown argument `{other}`")),
        }
    }
    // `--rule-id` only means something in listing mode. Accepting it alone and
    // silently printing the UNFILTERED table would hand the reviewer a report
    // that reads as narrowed to one rule when it is not — the same class of
    // defect as a blank command field reading as "no command".
    let list = list || rule_filter.is_some();
    Ok(RetroArgs {
        dir,
        list,
        rule_filter,
    })
}

/// `blastguard retro` — review past gate interventions and what became of them.
///
/// `--list` switches from the per-rule count table to one row per
/// intervention (gate, rule id, outcome, raw reason, recorded command); pair
/// it with `--rule-id <id>` to narrow the listing to a single rule, e.g. to
/// compare which specific interventions a fix made a rule stop raising
/// (see [`blastguard::retro::render_list`]). `--rule-id` alone (no explicit
/// `--list`) also switches to listing mode — see [`parse_retro_args`].
///
/// # Exit status is a verdict, not a formality
///
/// `2` when the corpus could not be read. A reviewer scripting this must be
/// able to tell "no gate ever stopped anything" from "I read nothing", and an
/// exit 0 printing an empty table conflates them — the same fail-open the
/// report body refuses (see [`blastguard::retro`]).
fn run_retro(rest: &[String]) -> i32 {
    let args = match parse_retro_args(rest) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return 2;
        }
    };
    let dir = match args.dir.or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|c| retro::transcript_dir_for(&c))
    }) {
        Some(d) => d,
        None => {
            eprintln!(
                "blastguard retro: could not locate a transcript directory — pass --dir explicitly"
            );
            return 2;
        }
    };
    let report = retro::build_report(retro::scan_dir(&dir));
    let out = if args.list {
        retro::render_list(&report, args.rule_filter.as_deref())
    } else {
        retro::render(&report)
    };
    print!("{out}");
    if report.is_undetermined() {
        eprintln!("(looked in {})", dir.display());
        return 2;
    }
    0
}

fn print_help() {
    println!(
        "blastguard {ver}\n\
A Claude Code PreToolUse hook that denies project-destroying operations.\n\n\
USAGE:\n  blastguard            read a hook payload from stdin (normal mode)\n  blastguard --version  print version\n  blastguard --help     this help\n\n\
It denies recursive/wildcard rm, git reset --hard, git clean -fdx, truncate,\n\
shred, mkfs, dd of=, recursive chmod/chown, find -delete, and single-> file\n\
overwrites — while exempting repo config files (.claude/**, *.toml, *.lock, …).",
        ver = env!("CARGO_PKG_VERSION")
    );
}

fn run() {
    let raw = hook::read_stdin();

    // EMPTY stdin is the case that genuinely determined there is nothing to
    // judge: no tool call was described, so silence is an accurate answer.
    if raw.trim().is_empty() {
        return;
    }

    let input = match HookInput::parse(&raw) {
        Some(i) => i,
        None => {
            // NON-EMPTY and unparseable is the opposite situation: a tool call
            // is being made and blastguard could not read it. Silence here IS
            // an allow (see the module docstring), so it would allow precisely
            // the call it failed to read. `HookInput::parse` erases the reason
            // via `.ok()`, so the two cases are indistinguishable downstream —
            // which is why they are separated HERE, at the only point that
            // still has the raw bytes.
            emit(Decision::ask(UNREADABLE_PAYLOAD), None);
            return;
        }
    };

    let decision = analyse(&input);

    emit(decision, Some(&input));
}

/// The reason attached to a payload blastguard could not parse at all.
///
/// A `const` rather than an inline literal so `rule_id` classifies it from the
/// same string the emitter uses — the drift between those two would be silent,
/// filing every schema break under `"unknown"`.
const UNREADABLE_PAYLOAD: &str =
    "blastguard could not parse this hook payload, so the tool call was never \
analysed — refusing to guess. If this recurs, the hook's payload schema has \
drifted from what Claude Code sends and needs updating.";

/// Harden, print, and only then record telemetry.
///
/// # The order is the invariant
///
/// `record_violation` used to run BEFORE the `println!`. Its own comment
/// promised the telemetry "must never change the decision, the printed JSON, or
/// the exit code" — but `let _ =` only neutralises the store write RETURNING an
/// error, not any of the event-construction steps PANICKING. A panic there
/// unwinds past this function into `hook::run_hook`, which logs and exits 0
/// with nothing on stdout, and a silent exit 0 IS an allow. So a crash in
/// purely additive telemetry could suppress a deny it was merely observing.
///
/// I could not find a panic reachable in that path today — `rule_id` is
/// `contains` comparisons on `&str`, `store::now` is `unwrap_or(0)`,
/// `cwd_or_current` is `unwrap_or_else`, and neither `build_event` nor
/// `normalize_signature` slices or unwraps. So this is a LATENT fail-open, not
/// a live one, and it is recorded as such rather than dressed up as an
/// exploitable bug. It is still worth closing: the decision should not be
/// hostage to work that has no say in it, and the next line added to that path
/// should not be able to re-open the hole silently. Printing first makes the
/// ordering enforce that, and the inner `catch_and_log` means telemetry can
/// only ever lose ITSELF.
fn emit(decision: Decision, input: Option<&HookInput>) {
    // An `Ask` is only a real question when someone can answer it. In a headless
    // or agent-driven session it is not a pause, it is a block the agent cannot
    // clear — so it is hardened to a deny instead. Never to an allow: "ask に
    // できないときは fail".
    let decision = if interactive::ask_available() {
        decision
    } else {
        decision.hardened()
    };

    let Some(line) = hookio::decision_line(&decision) else {
        return; // Allow → print nothing.
    };

    // The decision reaches Claude Code FIRST. Nothing below can retract it.
    println!("{line}");

    // Fail-soft fleet violation record for overwatch's cross-task
    // correlated-error detection. Asks are recorded too — a recurring ask is
    // the signal that some real construct is going unanalysed.
    //
    // A payload we could not parse has no `HookInput` to attribute to, so it is
    // simply not recorded; inventing a task key for it would pollute the
    // recurrence signature with a synthetic one.
    if let (Some(input), Decision::Deny(reason) | Decision::Ask(reason)) = (input, &decision) {
        let _ = hook::catch_and_log("blastguard-violation-record", || {
            record_violation(input, reason)
        });
    }
}

/// Run the detector with a panic barrier.
///
/// A panic here used to be swallowed into a silent exit 0, and a silent exit 0
/// IS an allow — so any crash in the analyser allowed the command it had just
/// failed to analyse. Catching it and returning a deny keeps the never-break-
/// the-turn contract (a deny is a normal outcome, not a broken turn) while
/// removing the fail-open.
///
/// `catch_unwind` is sound here because the closure borrows only `input` and the
/// detector holds no cross-call mutable state that a half-finished analysis
/// could leave inconsistent — the per-command analysis budget is re-seeded at
/// entry to `detect`, not carried between calls.
fn analyse(input: &HookInput) -> Decision {
    let tool = input.tool_name.clone();
    let tool_input = input.tool_input.clone();
    // Built OUTSIDE the barrier deliberately: it touches the filesystem
    // (`canonicalize`) and the environment, and neither belongs inside the
    // catch_unwind whose job is to convert an ANALYSIS crash into a deny.
    // `safe_roots` itself cannot panic — every fallible step is an
    // `Option`/`Result` resolved to the restrictive side — and if it somehow
    // did, `hook::run_hook`'s outer barrier still catches it.
    let scope = safe_roots(input);
    let result =
        std::panic::catch_unwind(move || detect::detect_scoped(&tool, tool_input.as_ref(), &scope));
    match result {
        Ok(decision) => decision,
        Err(_) => Decision::deny(INTERNAL_ERROR_REASON),
    }
}

/// The session's location model: which trees this session may destroy things
/// INSIDE without a flat refusal.
///
/// Only the binary can build this — it is the only part of the crate that may
/// read the environment or the filesystem. Everything it passes is a fact about
/// the session, never about the command being judged:
///
///   * the PreToolUse payload's `cwd` — where the session is working (a git
///     worktree, typically);
///   * `CLAUDE_PROJECT_DIR` — the project root Claude Code exports to every
///     hook, which differs from `cwd` in a worktree session and is equally
///     legitimate;
///   * `HOME` — passed only so [`SafeRoots::new`] can REFUSE to treat the home
///     directory as a root;
///   * `TMPDIR` — added to the fixed temp roots. Note the asymmetry that makes
///     this sound: reading `$TMPDIR` out of the hook's own environment is not
///     the same act as expanding the literal string `$TMPDIR` found in a
///     command, which `scope` always refuses to do.
///
/// A missing or empty `cwd` yields a model with only the temp roots in it, and
/// an unresolvable one yields no model at all — both of which simply keep the
/// pre-0.2.51 verdicts.
fn safe_roots(input: &HookInput) -> SafeRoots {
    let cwd = input.cwd.trim();
    let project = std::env::var("CLAUDE_PROJECT_DIR").ok();
    let home = std::env::var("HOME").ok();
    let tmpdir = std::env::var("TMPDIR").ok();
    SafeRoots::new(
        if cwd.is_empty() { None } else { Some(cwd) },
        project.as_deref(),
        home.as_deref(),
        tmpdir.as_deref(),
        Some(real_path),
    )
}

/// Resolve `path` to its real path — symlinked components included — WITHOUT
/// requiring that `path` itself exists.
///
/// `std::fs::canonicalize` fails outright on a missing path, and a destructive
/// target that does not exist is completely ordinary (`rm -rf target` in a
/// freshly cloned tree). So this canonicalises the deepest EXISTING ancestor and
/// re-attaches the components below it: the symlink question is answered for
/// every component that exists, which is every component that could redirect
/// the operation somewhere else.
///
/// `None` on failure, which `scope` reads as `Undetermined` — i.e. the caller
/// keeps its Deny. Notable consequences, both on the restrictive side:
///
///   * a FINAL component that is itself a symlink is resolved to its target, so
///     `rm -rf link-to-usr` is judged at `/usr` even though deleting a symlink
///     does not touch what it points at. That over-denies (`rm` of a symlink
///     inside the project reads as leaving the tree), which is exactly the
///     direction this crate errs in, and is no worse than the flat Deny that
///     shape had before;
///   * a path whose every ancestor is unreadable resolves to nothing and stays
///     refused.
fn real_path(path: &str) -> Option<String> {
    let mut cur = std::path::PathBuf::from(path);
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    // Each iteration pops exactly one component, so this terminates at `/`.
    loop {
        if let Ok(real) = cur.canonicalize() {
            let mut out = real;
            for name in tail.iter().rev() {
                out.push(name);
            }
            return out.to_str().map(str::to_string);
        }
        let name = cur.file_name()?.to_os_string();
        if !cur.pop() {
            return None;
        }
        tail.push(name);
    }
}

/// Best-effort: append a `ViolationEvent` for this denial to overwatch's
/// project-scoped violations.jsonl. `reason` is free text (may embed a
/// variable path/target) — `rule_id::rule_id` normalizes it to a stable
/// discriminator before it becomes the violation signature.
fn record_violation(input: &HookInput, reason: &str) {
    let target = input.target();
    let task_key = match &target {
        Some(t) => format!("{}:{}", input.tool_name, t),
        None => input.tool_name.clone(),
    };
    let raw = overwatch::violation::RawViolation {
        rule_id: Some(rule_id::rule_id(reason)),
        ..Default::default()
    };
    let event = overwatch::violation::build_event(
        overwatch::violation::ViolationSource::Blastguard,
        &raw,
        task_key,
        input.session_key(),
        overwatch::store::now(),
        Some(reason.to_string()),
    );
    let cwd = input.cwd_or_current();
    if let Some(event) = event {
        let _ = overwatch::store::append_violation(&cwd, &event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_id_alone_implies_listing_mode() {
        // Added after the original `retro --list` work landed, and shipped
        // with zero coverage: `run_retro` is never called by any test and
        // `main.rs` had no test module at all, so a regression to "print the
        // unfiltered table when only --rule-id is given" would have kept the
        // suite green. Reachable now only because the flag logic was pulled
        // out of `run_retro` into `parse_retro_args`, a pure function with no
        // I/O — `run_retro` itself still isn't under test (it prints to
        // stdout/stderr and touches the transcript directory), and that gap
        // is called out below rather than papered over.
        let args =
            parse_retro_args(&["--rule-id".to_string(), "some-rule".to_string()]).expect("parses");
        assert!(args.list, "bare --rule-id must imply listing mode");
        assert_eq!(args.rule_filter.as_deref(), Some("some-rule"));
    }

    #[test]
    fn explicit_list_flag_still_works_without_rule_id() {
        let args = parse_retro_args(&["--list".to_string()]).expect("parses");
        assert!(args.list);
        assert_eq!(args.rule_filter, None);
    }

    #[test]
    fn neither_flag_leaves_table_mode() {
        let args = parse_retro_args(&[]).expect("parses");
        assert!(!args.list);
    }

    #[test]
    fn rule_id_without_a_value_is_a_parse_error_not_a_panic() {
        let err = parse_retro_args(&["--rule-id".to_string()]).unwrap_err();
        assert!(err.contains("--rule-id needs a rule id"), "{err}");
    }

    #[test]
    fn an_unknown_flag_is_rejected() {
        let err = parse_retro_args(&["--bogus".to_string()]).unwrap_err();
        assert!(err.contains("unknown argument"), "{err}");
    }

    #[test]
    fn dir_and_rule_id_compose() {
        // Guards against a refactor that accidentally drops --dir when
        // --rule-id is also present (they are independent fields on
        // `RetroArgs`, filled by different match arms in the same loop).
        let args = parse_retro_args(&[
            "--dir".to_string(),
            "/tmp/some-transcripts".to_string(),
            "--rule-id".to_string(),
            "x".to_string(),
        ])
        .expect("parses");
        assert!(args.list);
        assert_eq!(
            args.dir.as_deref(),
            Some(std::path::Path::new("/tmp/some-transcripts"))
        );
    }
}
