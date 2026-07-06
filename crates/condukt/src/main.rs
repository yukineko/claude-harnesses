//! condukt — deterministic orchestration engine for Claude Code.
//!
//! The LLM (the `/condukt` skill + interpreter/worker/verifier agents) does the
//! judgement work: interpret a request, decompose it into tasks, implement and
//! verify each. This binary does the deterministic work the LLM should not
//! eyeball: schedule tasks into parallel/serial batches by file-conflict
//! analysis, manage the git-worktree lifecycle, track run state, and gate
//! completion. Hooks (restore/statusline) never break a turn — they exit 0.

mod checkpoint;
mod ci;
mod circuit;
mod claim;
mod config;
mod consensus;
mod editgate;
mod escalate;
mod gate_exec;
mod gatelog;
mod hooks;
mod install;
mod lock;
mod model;
mod oracle;
mod policy;
mod pr;
mod replan;
mod run_policy;
mod schedule;
mod state;
mod status;
mod store;
mod verify;
mod worktree;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "condukt",
    version,
    about = "Deterministic orchestration engine for Claude Code"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// SessionStart hook: remind about unfinished runs / orphan worktrees.
    Restore,
    /// One-line progress readout for the statusLine.
    Statusline,
    /// PostToolUse hook: after a worker's Edit/Write/MultiEdit to a Rust file
    /// inside a live condukt worktree, deterministically decide (via the
    /// edit-time compile gate) whether the edit left the crate broken. On a real
    /// non-fallback broken verdict it prints one line of JSON
    /// `{"decision":"block","reason":<diagnostics>}` so the worker fixes it in
    /// the same turn. Fail-soft everywhere else: any other outcome or error
    /// prints nothing and exits 0 (a hook must never break a turn).
    Editgate,
    /// Compute a schedule from a decomposition JSON (stdin or --file).
    Schedule {
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Validate a decomposition JSON (stdin or --file).
    Validate {
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// git worktree lifecycle.
    Worktree {
        #[command(subcommand)]
        action: WtAction,
    },
    /// Run-state tracking and the completion gate.
    State {
        #[command(subcommand)]
        action: StateAction,
    },
    /// Central graded autonomy policy: map a decision's risk x reversibility x
    /// confidence to auto|escalate|block.
    Policy {
        #[command(subcommand)]
        action: PolicyAction,
    },
    /// Deterministic verifier-stage helpers (formatting only; the fix DECISION
    /// stays with the LLM worker).
    Verify {
        #[command(subcommand)]
        action: VerifyAction,
    },
    /// Deterministic reflux-cascade helpers: classify a failing task's reflux
    /// facts into "escalate the model" vs "replan" (formatting/classification
    /// only; the fix/re-decomposition DECISION stays with the LLM).
    Replan {
        #[command(subcommand)]
        action: ReplanAction,
    },
    /// Deterministic RUN-POLICY gate: decide the next verify→docker→ship
    /// pipeline stage from a cheap-verify result, production-divergence, and
    /// change-risk (formatting/classification only; acting on the verdict —
    /// actually launching a Docker re-verify or proceeding to ship — stays
    /// the `/condukt` skill orchestration's job).
    RunPolicy {
        #[command(subcommand)]
        action: RunPolicyAction,
    },
    /// Deterministic CIRCUIT-BREAKER stop-condition gate: gather the three
    /// signals (consecutive-failure streak, budget-over-cap, idle/stall) for a
    /// run — all FAIL-SOFT — run the pure `decide_circuit` core, print the
    /// verdict + signals as JSON, journal it, and EXIT NONZERO when the breaker
    /// trips. The flow/condukt loops consult this each iteration
    /// (`if ! condukt circuit check --run RID; then stop; fi`).
    Circuit {
        #[command(subcommand)]
        action: CircuitAction,
    },
    /// Deterministic GATE-EXEC decision: for a gated task, decide whether it may
    /// auto-execute or must escalate. Classifies the task's action text (graded
    /// risk + reversibility), reads the autonomy policy — all FAIL-SOFT — runs
    /// the pure `decide_gate_exec` core, and EXITS NONZERO on escalate. On
    /// auto-exec it checkpoints the run first (recoverable) then journals. A
    /// caller does `if ! condukt gate check --run RID --task T; then escalate; fi`.
    Gate {
        #[command(subcommand)]
        action: GateAction,
    },
    /// Opt-in worker sandboxing: run a worker's build/test command through the
    /// docker exec backend (network + filesystem + resource isolation) when
    /// `[worker] sandbox_enabled` / `CONDUKT_WORKER_SANDBOX` is set, otherwise run
    /// it unchanged on the host. Fail-soft: docker-absent degrades to the
    /// `docker_unavailable` verdict (never a host fallback) and always exits 0.
    Sandbox {
        #[command(subcommand)]
        action: SandboxAction,
    },
    /// Multi-sample self-consistency: plan an opt-in fan-out, or tally N verifier
    /// verdicts for one task into a majority winner + escalate-to-opus decision.
    Consensus {
        #[command(subcommand)]
        action: ConsensusAction,
    },
    /// Create ~/.condukt and a default config.toml.
    Init,
    /// Merge the SessionStart hook into ~/.claude/settings.json (manual install).
    Install {
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove condukt hooks from ~/.claude/settings.json.
    Uninstall,
    /// Print ~/.condukt/knowledge.md to stdout (empty output if absent).
    Knowledge,
    /// Terminal external-loop step: open a PR via the gh CLI. Push/PR stays
    /// BEHIND the GATED human approval — the actual `gh pr create` runs ONLY with
    /// `--execute` (supplied by the /condukt skill after approval). Uses gh's own
    /// auth (no API key). Fail-soft: gh absent/unauthenticated degrades to
    /// local-commit-only and exits 0 (never breaks the turn).
    Pr {
        #[command(subcommand)]
        action: PrAction,
    },
    /// Show open runs and their tasks as an ASCII tree.
    Status {
        /// Include all runs, not just open ones.
        #[arg(long)]
        all: bool,
    },
    /// Run one iteration of the test-fix cycle for the given module type.
    ///
    /// Executes build/deploy/test in the sequence appropriate for the module:
    ///   server: deploy → test
    ///   client: build → test
    ///   e2e:    build → deploy → test
    ///
    /// Prints a JSON object with iteration, failure_count, stop, and stop_reason.
    /// Exit 0 while progress is being made; exit 1 when stop=true and tests failed.
    Loop {
        /// Module type determines the cycle sequence.
        #[arg(long, value_name = "server|client|e2e")]
        module: String,
        /// Which iteration this is (informational; included in JSON output).
        #[arg(long, default_value = "1")]
        iteration: usize,
        /// Failure count from the previous iteration (used for no-progress detection).
        #[arg(long)]
        prev_failures: Option<usize>,
    },
    /// Durable async escalation channel: enqueue / list / resolve out-of-band
    /// questions in a per-project persistent store, so a blocked or GATED task
    /// records "I need a human answer for X" and keeps going instead of blocking
    /// inline on an AskUserQuestion. Backed by
    /// `<state_dir>/<project-key>/escalations.json` (atomic writes, fail-soft).
    Escalate {
        #[command(subcommand)]
        action: EscalateAction,
    },
}

#[derive(Subcommand)]
enum EscalateAction {
    /// Enqueue a new escalation and print the created record as JSON (its `id`
    /// on the first line for easy capture, then the pretty record). Persists to
    /// the durable per-project store.
    Add {
        /// The run this question belongs to.
        #[arg(long)]
        run: String,
        /// The task within the run that is blocked on the answer.
        #[arg(long)]
        task: String,
        /// The question being asked.
        #[arg(long)]
        question: String,
        /// One offered option; repeat `--option` for each.
        #[arg(long = "option")]
        option: Vec<String>,
        /// 0-based index of the recommended option.
        #[arg(long, default_value_t = 0)]
        recommend: usize,
    },
    /// List the still-OPEN (unresolved) escalations for a run. Human-readable by
    /// default; `--json` prints the records as a JSON array.
    List {
        #[arg(long)]
        run: String,
        /// Emit JSON instead of the human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Resolve an escalation by id, recording the chosen option. The record stays
    /// in the store (now resolved, with its answer) so the task can resume.
    Resolve {
        #[arg(long)]
        id: String,
        /// The chosen option (should be one of the recorded options).
        #[arg(long)]
        choice: String,
    },
}

#[derive(Subcommand)]
enum WtAction {
    /// Create a worktree at <worktree_base>/<topic> on a new branch; prints path.
    Create {
        #[arg(long)]
        topic: String,
        #[arg(long)]
        branch: String,
    },
    /// Merge <branch> into the configured default branch.
    Merge {
        #[arg(long)]
        branch: String,
    },
    /// Remove a worktree and (best-effort) delete its branch.
    Remove {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        branch: Option<String>,
    },
    /// Report orphan worktrees under worktree_base (--remove to delete them).
    Cleanup {
        #[arg(long)]
        remove: bool,
    },
    /// List registered worktrees (path<TAB>branch).
    List,
}

#[derive(Subcommand)]
enum ConsensusAction {
    /// Decide whether a task should fan out into N candidate implementations.
    /// OPT-IN: enabled only by config `[consensus] enabled` / `CONDUKT_CONSENSUS`,
    /// or a per-task `--risk high`. Prints `{enabled,samples,threshold,risk}` and
    /// exits 0 when a fan-out is warranted (so the skill can branch on the exit
    /// code, like `state autonomy-check`), 1 for the ordinary single-sample path.
    Plan {
        /// Per-task risk flag; "high"/"critical" forces fan-out even when the
        /// global switch is off.
        #[arg(long)]
        risk: Option<String>,
    },
    /// Tally N verifier verdicts (JSON on stdin or --file) for the same task into
    /// a majority winner, agreement rate, and escalate-to-opus decision. Prints
    /// the decision JSON; exits 0 when a consensus winner is taken, 1 when the
    /// caller should escalate (all-fail, tie, or agreement below threshold).
    ///
    /// Input is either a bare array of verdicts or an object
    /// `{"verdicts":[...],"threshold":0.5}`. Each verdict is
    /// `{"candidate":"<id>","pass":<bool>,"group":"<optional answer bucket>"}`.
    Vote {
        #[arg(long)]
        file: Option<PathBuf>,
        /// Agreement threshold override (CLI > JSON > config > built-in default).
        #[arg(long)]
        threshold: Option<f64>,
    },
}

#[derive(Subcommand)]
enum StateAction {
    /// Seed a run from a decomposition JSON; prints the run id on stdout.
    Init {
        #[arg(long)]
        run: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        /// Terminal or session label for identification (e.g. from `tty`).
        /// Shown in `state list` and included in conflict reports.
        #[arg(long)]
        label: Option<String>,
    },
    /// Check whether a decomposition's touched_files conflict with open runs.
    /// Exits 0 if no conflicts, exits 1 if conflicts are found (JSON on stdout).
    ConflictCheck {
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Update one task's status (and optionally its worktree/branch).
    /// Passing --worktree/--branch without a value has no effect; use
    /// --clear-worktree / --clear-branch to explicitly erase the existing value.
    Set {
        #[arg(long)]
        run: String,
        #[arg(long)]
        task: String,
        #[arg(long)]
        status: String,
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Explicitly clear the worktree field (set it to null).
        #[arg(long)]
        clear_worktree: bool,
        /// Explicitly clear the branch field (set it to null).
        #[arg(long)]
        clear_branch: bool,
        /// Model actually used to execute the task (recorded for fugu-router).
        /// Omitted → unchanged; falls back to the decomposition's suggested_model.
        #[arg(long)]
        model: Option<String>,
        /// Observed USD cost of the task (recorded for fugu-router). Omitted → unchanged.
        #[arg(long)]
        cost: Option<f64>,
    },
    /// Print a run's full state as JSON.
    Show {
        #[arg(long)]
        run: String,
    },
    /// Claim files for a run in the cross-session registry (PDO collision guard).
    /// With --file, claims those paths/globs; without, claims every touched_file
    /// in the run's decomposition. Prints a ClaimOutcome JSON. Exits 1 if any file
    /// was skipped because a live holder from another run owns it (hard skip).
    Claim {
        #[arg(long)]
        run: String,
        /// Session id of the owning Claude session (defaults to $CLAUDE_CODE_SESSION_ID).
        #[arg(long)]
        session: Option<String>,
        /// Specific file/glob to claim (repeatable). Omit to claim the run's
        /// whole decomposition footprint.
        #[arg(long)]
        file: Vec<String>,
    },
    /// Release files a run holds in the claim registry. With --file, releases just
    /// those; without, releases every file held by the run.
    Release {
        #[arg(long)]
        run: String,
        #[arg(long)]
        file: Vec<String>,
    },
    /// Refresh the heartbeat of every file a run holds (keep-alive so a live-but-
    /// quiet session is not reaped). Prints the number of claims refreshed.
    Heartbeat {
        #[arg(long)]
        run: String,
    },
    /// Print the live claim registry (stale entries reaped) as JSON.
    Claims,
    /// Claim tasks (by opaque hashkey) for a run in the cross-session task-claim
    /// registry. Prints a ClaimOutcome JSON (Skipped::file carries the hashkey).
    /// Exits 1 if any hashkey was skipped because a live holder from another run
    /// owns it (hard skip — callers must not process that task).
    ClaimTask {
        #[arg(long)]
        run: String,
        /// Session id of the owning Claude session (defaults to $CLAUDE_CODE_SESSION_ID).
        #[arg(long)]
        session: Option<String>,
        /// Human-readable task title, recorded for the execution-state view.
        #[arg(long)]
        title: Option<String>,
        /// Opaque task hashkey to claim (repeatable, required).
        #[arg(long)]
        hashkey: Vec<String>,
    },
    /// Release task hashkeys from the task-claim registry (call when a task
    /// reaches a terminal status). Prints the number of claims released.
    ReleaseTask {
        #[arg(long)]
        run: String,
        /// Opaque task hashkey to release (repeatable, required).
        #[arg(long)]
        hashkey: Vec<String>,
    },
    /// Exit 0 if a live claim (from any run) holds the given task hashkey, else
    /// exit 1. Prints a small JSON `{hashkey, claimed, holder_run}` for humans,
    /// but the exit code is the contract other tools should rely on.
    IsClaimed {
        #[arg(long)]
        hashkey: String,
    },
    /// Join the live task-claim registry against the project backlog and write
    /// `execution-state.json`. Prints the resulting rows as JSON.
    ExecutionState,
    /// Record completed runs' outcomes to fugu-router (the learning signal).
    /// Deterministic, idempotent: only settled, not-yet-recorded runs are emitted,
    /// and each run is marked so repeated firings (e.g. the Stop hook) never
    /// double-record. Soft: a no-op when the fugu-router binary is not on PATH.
    RecordRun {
        /// Record one specific run. Omit with --all to sweep every settled run.
        #[arg(long)]
        run: Option<String>,
        /// Record every settled, not-yet-recorded run for this project.
        #[arg(long)]
        all: bool,
    },
    /// Exit 0 if the run is complete, else print reasons and exit 1.
    Gate {
        #[arg(long)]
        run: String,
    },
    /// List open runs (run_id<TAB>done/total<TAB>goal).
    List,
    /// Auto-reconcile: detect merged/gone branches and mark tasks verified.
    Reconcile {
        #[arg(long)]
        run: String,
        /// Preview changes without writing (dry run).
        #[arg(long)]
        dry_run: bool,
    },
    /// Aggregate stats across all runs (completion rate, task distribution).
    Stats,
    /// Resume context for a stopped run: pending/failed/done tasks as JSON.
    ResumeContext {
        #[arg(long)]
        run: String,
    },
    /// Run the project's test suite (from config [test].command or auto-detect).
    Test {
        #[arg(long)]
        run: String,
    },
    /// Reset running/failed tasks back to pending, clearing their worktree/branch refs.
    Abandon {
        #[arg(long)]
        run: String,
        /// Abandon a specific task (set it back to pending).
        #[arg(long)]
        task: Option<String>,
        /// Abandon all stuck tasks (running + TTL exceeded) at once.
        #[arg(long)]
        all_stuck: bool,
    },
    /// Pause a run: no new workers will be dispatched until resumed.
    Pause {
        #[arg(long)]
        run: String,
    },
    /// Resume a paused run, allowing new workers to be dispatched again.
    Resume {
        #[arg(long)]
        run: String,
    },
    /// List all non-terminal tasks across open runs as JSON (for interactive cancel).
    ListTasks,
    /// Cancel a specific task, marking it as cancelled and clearing its worktree/branch.
    /// Works for pending/running/done tasks. Verified tasks cannot be cancelled.
    /// NOTE: cancelling a running task only updates state; an in-flight worker must be
    /// stopped manually (the binary cannot reach into a running Claude agent).
    Cancel {
        #[arg(long)]
        run: String,
        #[arg(long)]
        task: String,
    },
    /// Run the mechanical gate for a task's done_criteria and print JSON.
    /// Exits 0 if the criteria is met (or non-mechanical), exits 1 if it fails.
    /// The JSON `skip_verifier` field is true ONLY for purely mechanical criteria
    /// that pass; behavioral criteria always report `skip_verifier:false`.
    CheckCriteria {
        #[arg(long)]
        run: String,
        #[arg(long)]
        task: String,
    },
    /// Deterministically ask `tdd oracle` whether a fix/feature task's RED→GREEN
    /// proofs form a valid Fail→Pass reproduction oracle. Fail-soft: no `tdd` on
    /// PATH, a spawn failure, a gone worktree, or corrupt/missing stdout all
    /// degrade to `fallback:true` ("use the legacy gate instead") rather than
    /// panicking. Tasks that don't require an oracle (not fix/feature, or no
    /// `reproduction_tests`) also report `required:false, fallback:true`.
    /// Always prints JSON and exits 0 — this is an advisory signal, not a gate.
    CheckOracle {
        #[arg(long)]
        run: String,
        #[arg(long)]
        task: String,
    },
    /// Report whether condukt is in autonomous mode (config.toml `autonomous` +
    /// `CONDUKT_AUTONOMOUS` env). Prints `{"autonomous":<bool>}` and exits 0 when
    /// autonomous, 1 when not — so the /condukt skill can branch on the exit code
    /// to skip human gates (e.g. the Phase 3 agreement) only when autonomous.
    AutonomyCheck,
    /// Durably checkpoint a run: snapshot its run-state + each task's branch SHA
    /// and journal the event. Prints the new checkpoint seq. The reversibility
    /// safety net for autonomous proceeding (charter #7).
    Checkpoint {
        #[arg(long)]
        run: String,
        /// Optional human label (e.g. the phase name).
        #[arg(long)]
        label: Option<String>,
    },
    /// Roll a run back to a prior checkpoint: restore the snapshotted run-state,
    /// best-effort git-reset each worktree to its recorded SHA, and journal the
    /// rollback. Defaults to the latest checkpoint; --to picks a specific seq.
    Rollback {
        #[arg(long)]
        run: String,
        /// Checkpoint seq to restore (default: latest).
        #[arg(long)]
        to: Option<u64>,
    },
    /// Report whether condukt is in single-worktree mode (config.toml
    /// `single_worktree` + `CONDUKT_SINGLE_WORKTREE` env). Prints
    /// `{"single_worktree":<bool>}` and exits 0 when single-worktree, 1 when not
    /// — so the /condukt skill branches on the exit code to run all tasks in the
    /// main tree (selective staging, no per-task worktree/merge) only when on.
    WorktreeModeCheck,
    /// Resolve the verifier model so it never equals the worker model (shared
    /// blind-spot guard). Prints the chosen model on stdout. A distinct
    /// --suggested is honoured; otherwise a distinct tier is picked.
    VerifierModel {
        /// The model the worker used / will use for this task.
        #[arg(long)]
        worker: String,
        /// The verifier_model from route.json, if any (may equal --worker).
        #[arg(long)]
        suggested: Option<String>,
    },
}

#[derive(Subcommand)]
enum PolicyAction {
    /// Decide how to handle one autonomy decision. Prints `auto`/`escalate`/
    /// `block` on stdout and exits with a contract: 0=auto, 2=escalate,
    /// 3=block, 1=invalid input (unparseable level). Each level is one of
    /// low|medium|high (case-insensitive).
    Decide {
        /// How much damage if this decision is wrong (low|medium|high).
        #[arg(long)]
        risk: String,
        /// How easily the decision can be undone (low|medium|high).
        #[arg(long)]
        reversible: String,
        /// How sure we are the decision is correct (low|medium|high).
        #[arg(long)]
        confidence: String,
        /// Optional task title. Supplying any of --title/--files/--class opts
        /// into the fugu-router *calibrated confidence* soft path: when
        /// fugu-router is on PATH and returns a usable score, that overrides
        /// --confidence. Absent these flags, the legacy path is byte-identical.
        #[arg(long)]
        title: Option<String>,
        /// Optional touched files (repeatable or comma-separated) for the
        /// calibrated-confidence path.
        #[arg(long)]
        files: Vec<String>,
        /// Optional task class for the calibrated-confidence path.
        #[arg(long)]
        class: Option<String>,
    },
    /// Non-interactively answer one question using the graded-autonomy policy.
    /// On an `auto` verdict, prints `{"answered":true,"policy":"auto","chosen":
    /// ...}` (the recommended option) and journals the choice, exit 0. On
    /// `escalate` (exit 2) or `block` (exit 3), prints `{"answered":false,...}`
    /// so the caller falls through to a real AskUserQuestion / refuses. Same
    /// exit contract as `decide` (1 = invalid input: bad level or a --recommend
    /// index with no matching --option).
    Answer {
        /// How much damage if this decision is wrong (low|medium|high).
        #[arg(long)]
        risk: String,
        /// How easily the decision can be undone (low|medium|high).
        #[arg(long)]
        reversible: String,
        /// How sure we are the decision is correct (low|medium|high).
        #[arg(long)]
        confidence: String,
        /// The question being asked (recorded to the decision log on auto).
        #[arg(long)]
        question: String,
        /// One choice; repeat `--option` for each. On auto the recommended one
        /// is chosen.
        #[arg(long = "option")]
        options: Vec<String>,
        /// 0-based index of the recommended option (chosen on auto).
        #[arg(long, default_value_t = 0)]
        recommend: usize,
        /// Directory for the append-only decision log (default: the state dir).
        #[arg(long)]
        journal_dir: Option<PathBuf>,
        /// Optional task title. Supplying any of --title/--files/--class opts
        /// into the fugu-router *calibrated confidence* soft path (overrides
        /// --confidence when fugu-router is present and returns a usable score).
        /// Absent these flags, the legacy path is byte-identical.
        #[arg(long)]
        title: Option<String>,
        /// Optional touched files (repeatable or comma-separated) for the
        /// calibrated-confidence path.
        #[arg(long)]
        files: Vec<String>,
        /// Optional task class for the calibrated-confidence path.
        #[arg(long)]
        class: Option<String>,
    },
    /// Print the auto-answer audit trail (JSONL): every question the policy
    /// self-answered without prompting a human. The review surface for
    /// hands-off autonomy — gates are skipped, but each self-answer is logged
    /// and inspectable here. Prints nothing (exit 0) if the log is absent.
    Answers {
        /// Directory holding the decision log (default: the state dir).
        #[arg(long)]
        journal_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum VerifyAction {
    /// Distill raw test/verifier output (stdin or --file) into a structured
    /// FailureDigest (failing tests, assertion diffs, output tail) as pretty
    /// JSON on stdout, exit 0. Deterministic Rust formatting so the /condukt
    /// skill can fold the *why* — not just pass/fail — into the retry reflux
    /// prompt; the fix DECISION stays with the LLM worker.
    Digest {
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Distill a target's *runtime* output into a structured RuntimeDigest
    /// (exit code, panic/exception lines, stderr/stdout tails) as pretty JSON on
    /// stdout, exit 0 — the phase-3 counterpart of `verify digest`. `--stdout`
    /// / `--stderr` read from files; when `--stderr` is omitted stderr is read
    /// from stdin (the primary input, mirroring `verify digest`). `--exit-code`
    /// is threaded through verbatim (absent → null). With `--reflux` it instead
    /// prints the verifier→worker reflux verdict (pass/fail + an embedded
    /// runtime_digest on a runtime failure). Fail-soft: empty input yields an
    /// empty digest and exits 0; the fix DECISION stays with the LLM worker.
    Runtime {
        /// File holding the target's stdout (empty stdout when omitted).
        #[arg(long)]
        stdout: Option<PathBuf>,
        /// File holding the target's stderr; when omitted, stderr is read from
        /// stdin (the primary input, symmetric to `verify digest`).
        #[arg(long)]
        stderr: Option<PathBuf>,
        /// The target's process exit code (null when omitted, e.g. signal kill).
        #[arg(long)]
        exit_code: Option<i32>,
        /// Print the reflux verdict (pass/fail + embedded runtime_digest on
        /// failure) instead of the bare RuntimeDigest.
        #[arg(long)]
        reflux: bool,
    },
    /// Launch a real target process inside the blastguard-validated envelope and
    /// reflux its runtime signals (stdout/stderr/exit code) through the same
    /// verdict path as `verify runtime --reflux`. The `--cmd` is validated with
    /// blastguard BEFORE spawning; a flagged/destructive command is refused
    /// fail-closed (never run). Absent/unstartable targets and timeouts fail
    /// soft — the turn is never broken. The pass/fail + runtime_digest verdict is
    /// printed as pretty JSON and the process ALWAYS exits 0 (fail-soft); the fix
    /// DECISION stays with the LLM worker.
    ///
    /// With `--health-url`, the target is treated as a *server*: instead of
    /// waiting for it to exit, we poll a raw HTTP/1.1 `GET <health-url>` until it
    /// returns 200 (pass) or `--startup-timeout` elapses (fail-soft), then tear
    /// the process down. Without `--health-url` the legacy exit-wait behavior is
    /// unchanged.
    Launch {
        /// The command to launch (run via `sh -c`). Required.
        #[arg(long)]
        cmd: String,
        /// Timeout in seconds before the launched process is killed (fail-soft).
        /// Only used for the exit-wait path (no `--health-url`).
        #[arg(long, default_value_t = 30)]
        timeout: u64,
        /// Health endpoint to probe (e.g. `http://127.0.0.1:8080/health`). When
        /// set, switches to the server path: poll until HTTP 200 or startup
        /// timeout, then tear the process down.
        #[arg(long)]
        health_url: Option<String>,
        /// Seconds to poll `--health-url` for a 200 before failing soft. Only
        /// used when `--health-url` is set.
        #[arg(long, default_value_t = 30)]
        startup_timeout: u64,
        /// Run `--cmd` inside an isolated Docker container (`docker run --rm
        /// --network=none -v <cwd>:<cwd> -w <cwd> <image> sh -c <cmd>`) instead
        /// of a bare `sh -c` on the host. Same blastguard gate applies BEFORE
        /// spawn; if Docker itself is unavailable (binary missing / daemon
        /// down), fails soft with `note: "docker_unavailable"` — the target is
        /// never run outside the container as a fallback.
        #[arg(long)]
        docker: bool,
        /// Docker image to run `--cmd` in when `--docker` is set.
        #[arg(long, default_value = "alpine:latest")]
        image: String,
        /// Purely-additive deterministic RUN-POLICY gate: instead of directly
        /// launching, consult `decide_run_policy(cheap_verify, divergence,
        /// change_risk)` and launch `--cmd` in a container ONLY on the
        /// `EscalateDocker` verdict (the other verdicts never touch docker).
        /// When unset (default) the existing `--docker`/`--health-url`/host
        /// launch behavior is byte-for-byte unchanged.
        #[arg(long)]
        run_policy: bool,
        /// Cheap-verify signal for `--run-policy` ("pass"|"fail"|else->unknown).
        #[arg(long, default_value = "unknown")]
        cheap_verify: Option<String>,
        /// Production-divergence signal for `--run-policy`
        /// ("low"|"medium"|"high", fail-softs to "high").
        #[arg(long, default_value = "high")]
        divergence: Option<String>,
        /// Change-risk signal for `--run-policy`
        /// ("low"|"medium"|"high", fail-softs to "high").
        #[arg(long, default_value = "high")]
        change_risk: Option<String>,
        /// When set with `--run-policy`, append the chosen run-policy verdict as
        /// a JSONL record to the run's run-policy decision log (same recording
        /// path as `run-policy decide --run`). Omitted = no record is written.
        #[arg(long)]
        run: Option<String>,
    },
}

#[derive(Subcommand)]
enum SandboxAction {
    /// Run `--cmd` under worker sandboxing when enabled by config
    /// (`[worker] sandbox_enabled` / `CONDUKT_WORKER_SANDBOX`): the command runs
    /// inside `docker run --rm --network=none` with the configured image and
    /// optional resource limits (`--memory`/`--cpus`/`--pids-limit`), the CWD
    /// bind-mounted read-write at the same path (filesystem confined to the
    /// worktree). When sandboxing is OFF (the default) the command runs
    /// unchanged on the host through the same reflux verdict path — the
    /// container path is never started. Prints the runtime verdict JSON with a
    /// `sandboxed` marker and ALWAYS exits 0 (fail-soft; docker-absent yields
    /// `note: "docker_unavailable"`, never a host fallback of the container run).
    Run {
        /// The build/test command to run (via `sh -c`). Required.
        #[arg(long)]
        cmd: String,
        /// Timeout in seconds before the process is killed (fail-soft).
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
}

#[derive(Subcommand)]
enum ReplanAction {
    /// Classify a failing task's reflux facts (JSON on stdin or --file:
    /// `{"reason":...,"failed_tests":...,"diff":...,"model_tier":...,
    /// "done_criteria":...,"task_summary":...}`, all fields optional/default
    /// empty) into `escalate_model` vs `replan`, and — ONLY when the
    /// resolution is `replan` — build a `ReplanHandoff` that explicitly
    /// instructs the interpreter to produce a NEW decomposition (different
    /// approach, different scope) rather than re-running the original
    /// decomposition. When the resolution is `escalate_model`, no handoff is
    /// built; only the classification is printed (the cascade's existing
    /// tier-escalation retry path handles that case, unchanged). Prints pretty
    /// JSON on stdout, exit 0. Deterministic Rust formatting so the /condukt
    /// skill's cascade can fold "what next?" into the reflux without an extra
    /// LLM turn; the re-decomposition itself stays the interpreter's job.
    Handoff {
        #[arg(long)]
        file: Option<PathBuf>,
        /// When set, append this decision as a JSONL record to the run's
        /// replan decision log (`<run>.replan-log.jsonl`) for later
        /// aggregation via `condukt replan stats --run <RID>`. Omitted =
        /// no record is written (backward-compatible: stdout output is
        /// unchanged either way).
        #[arg(long)]
        run: Option<String>,
    },
    /// Aggregate the replan decision log for a run into per-directive counts
    /// (`{replan, escalate_model, escalate_to_user}`) and print them as JSON.
    /// Reads records written by `replan handoff --run <RID>`; an empty/missing
    /// log yields all-zero counts (never errors).
    Stats {
        #[arg(long)]
        run: String,
    },
}

#[derive(Subcommand)]
enum RunPolicyAction {
    /// Decide the next verify→docker→ship pipeline stage from a cheap-verify
    /// result, production-divergence, and change-risk. Prints pretty JSON
    /// (`RunPolicyDecision`) on stdout, exit 0. Deterministic Rust decision
    /// so Phase 6 of the `/condukt` skill can fold "docker or not?" into the
    /// verify stage without an extra LLM turn; acting on the verdict (e.g.
    /// actually invoking `condukt verify launch --docker`) stays the skill's
    /// job.
    Decide {
        /// "pass" | "fail" | anything else (fail-softs to "unknown").
        #[arg(long)]
        cheap_verify: String,
        /// "low" | "medium" | "high" (fail-softs to "high", the safest value).
        #[arg(long)]
        divergence: String,
        /// "low" | "medium" | "high" (fail-softs to "high", the safest value).
        #[arg(long)]
        change_risk: String,
        /// When set, append this decision as a JSONL record to the run's
        /// run-policy decision log (`<run>.run-policy-log.jsonl`) for later
        /// aggregation via `condukt run-policy stats --run <RID>`. Omitted =
        /// no record is written (backward-compatible: stdout output is
        /// unchanged either way).
        #[arg(long)]
        run: Option<String>,
    },
    /// Aggregate the run-policy decision log for a run into per-verdict counts
    /// (`{verify_only, escalate_docker, escalate_ship, ask_human}`) and print
    /// them as JSON. Reads records written by `run-policy decide --run <RID>`;
    /// an empty/missing log yields all-zero counts (never errors).
    Stats {
        #[arg(long)]
        run: String,
    },
}

#[derive(Subcommand)]
enum CircuitAction {
    /// Gather the streak/budget/stall signals for a run (all FAIL-SOFT), run the
    /// deterministic `decide_circuit` core, print the verdict + gathered signals
    /// as JSON on stdout, append the same record to the run's append-only
    /// circuit-verdict journal (fail-soft), and EXIT 1 when the breaker trips /
    /// EXIT 0 to continue — so a loop can do
    /// `if ! condukt circuit check --run RID; then stop; fi`.
    Check {
        /// The run id whose signals to gather.
        #[arg(long)]
        run: String,
        /// Consecutive-failure cap (matches the retired "3 consecutive failures"
        /// rule). 0 disables the streak axis.
        #[arg(long, default_value_t = 3)]
        streak_cap: u32,
        /// Idle time-to-live in seconds before the stall axis trips (30 min,
        /// aligning with the stuck-task TTL). 0 disables the stall axis.
        #[arg(long, default_value_t = 1800)]
        idle_ttl_secs: i64,
        /// Optional daily-USD cap for the budget axis. When set (and > 0), the
        /// gate reads budgetguard's on-disk day-usage ledger and trips if today's
        /// spend is at/over this cap. Omitted → the budget axis is disabled
        /// (fail-soft non-trip).
        #[arg(long)]
        budget_cap_usd: Option<f64>,
    },
}

#[derive(Subcommand)]
enum GateAction {
    /// Decide whether a gated task auto-executes or escalates. Loads the run's
    /// decomposition, classifies the task's action text (graded risk +
    /// reversibility), derives whether the run policy is auto (autonomous mode,
    /// the SAME predicate `state autonomy-check` uses) — all FAIL-SOFT — runs the
    /// deterministic `decide_gate_exec` core, prints the verdict + gathered
    /// signals as JSON, and journals the decision (fail-soft). On auto-exec it
    /// checkpoints the run FIRST (so the action is recoverable) then exits 0; on
    /// escalate it exits 1 so a caller can do
    /// `if ! condukt gate check --run RID --task T; then escalate; fi`.
    Check {
        /// The run id whose decomposition holds the task.
        #[arg(long)]
        run: String,
        /// The gated task id to decide.
        #[arg(long)]
        task: String,
    },
}

#[derive(Subcommand)]
enum PrAction {
    /// Prepare (dry-run) or, with --execute, open a PR via `gh pr create`.
    ///
    /// Without --execute this prints a Prepared JSON showing the exact argv that
    /// WOULD run and exits 0 — the GATED dry-run. The /condukt skill passes
    /// --execute ONLY after the human GATED approval, so unattended/autonomous
    /// runs never open a PR on their own. When gh is absent or unauthenticated
    /// the outcome degrades to DegradedLocalOnly and still exits 0 (fail-soft).
    Create {
        #[arg(long)]
        title: String,
        /// PR body text. Mutually complementary with --body-file; if both are
        /// given, --body wins. Defaults to empty.
        #[arg(long)]
        body: Option<String>,
        /// Read the PR body from a file (used when --body is absent).
        #[arg(long)]
        body_file: Option<PathBuf>,
        /// Source branch. Defaults to the current branch when omitted.
        #[arg(long)]
        head: Option<String>,
        /// Target branch. Defaults to the configured default branch.
        #[arg(long)]
        base: Option<String>,
        /// GATED gate: actually run `gh pr create`. Only supplied after the
        /// human GATED approval. Without it, the command is a dry-run.
        #[arg(long)]
        execute: bool,
    },
    /// Poll a PR's CI status via `gh pr checks`, decide the next action with
    /// the deterministic CI state machine, and — ONLY when CI is green — merge
    /// the branch into the default branch.
    ///
    /// The verdict is one of Wait (CI still running), Reenter (CI failed),
    /// Merge (CI green), or DegradedLocalOnly (gh absent / CI status
    /// undeterminable). The real `worktree::merge` runs ONLY on the green
    /// verdict AND with `--execute` (the GATED gate the /condukt skill supplies
    /// after human approval); without --execute a green verdict is reported as a
    /// dry-run and nothing is merged. gh absent, CI-status-unavailable, or a
    /// failed merge all degrade to local-commit-only and exit 0 (fail-soft:
    /// never breaks the turn, never merges on a non-green path).
    Poll {
        /// The PR number or URL to poll (passed to `gh pr checks <pr>`).
        #[arg(long)]
        pr: String,
        /// Branch to merge when CI is green. Defaults to the current branch.
        #[arg(long)]
        branch: Option<String>,
        /// GATED gate: actually run `worktree::merge` on the green path. Only
        /// supplied after the human GATED approval. Without it, a green verdict
        /// is a dry-run and no merge is performed.
        #[arg(long)]
        execute: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        // Hooks never break a turn: swallow panics and always exit 0.
        Command::Restore => run_hook(|| {
            if Config::disabled() {
                return;
            }
            let input = model::HookInput::parse(&read_stdin());
            hooks::restore::run(input.cwd.as_deref().unwrap_or(""));
        }),
        Command::Statusline => run_hook(|| {
            if Config::disabled() {
                return;
            }
            let input = model::HookInput::parse(&read_stdin());
            hooks::statusline::run(input.cwd.as_deref().unwrap_or(""));
        }),
        Command::Editgate => run_hook(|| {
            if Config::disabled() {
                return;
            }
            run_editgate();
        }),
        other => {
            if let Err(e) = run_user(other) {
                eprintln!("condukt: {e:#}");
                std::process::exit(1);
            }
        }
    }
}

fn run_hook<F: FnOnce() + std::panic::UnwindSafe>(f: F) -> ! {
    let _ = std::panic::catch_unwind(f);
    std::process::exit(0);
}

/// PostToolUse edit-time compile gate. Reads a PostToolUse payload from stdin,
/// and — only for an Edit/Write/MultiEdit to a Rust file inside a live condukt
/// worktree that a real (non-fallback) `cargo check` finds broken — prints one
/// line of JSON `{"decision":"block","reason":<diagnostics>}` so the worker
/// fixes it in the same turn. Every other path (non-edit tool, no file, non-Rust
/// file, out-of-worktree edit, clean build, fallback, empty/invalid stdin, or
/// any error) prints nothing. Called under [`run_hook`], so it exits 0 and a
/// panic is swallowed — it can never break a turn.
fn run_editgate() {
    // Parse the PostToolUse payload; empty/invalid stdin → stay silent.
    let input = match harness_core::hook::HookInput::parse(&read_stdin()) {
        Some(i) => i,
        None => return,
    };

    // Only file-mutating edit tools are in scope. `HookInput::target()` also
    // yields a path for Read/NotebookEdit, so gate on the tool name explicitly.
    if !matches!(input.tool_name.as_str(), "Edit" | "Write" | "MultiEdit") {
        return;
    }
    let file_path = match input.target() {
        Some(p) => PathBuf::from(p),
        None => return,
    };

    let cfg = Config::load();
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(_) => return,
    };

    // Resolve the project's run-state directory the same way CheckOracle does
    // (Config.state_dir keyed by the repo root under `cwd`), then ask which
    // live worktree — if any — this edited path falls under.
    let run_dir = cfg
        .state_dir
        .join(store::project_key(&store::repo_root(&cwd)));
    let wt = state::active_worktree_for_path(&file_path, &run_dir);

    // BLOCK enforcement: only a real (non-fallback) broken verdict rejects.
    let verdict = editgate::check_edit(&file_path, wt.as_deref(), true);
    if let state::EditGateDecision::Reject = state::enforce_edit_gate(&verdict) {
        let reason = verdict
            .get("diagnostics")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("edit left the crate not compiling (see cargo check)");
        let out = serde_json::json!({ "decision": "block", "reason": reason });
        if let Ok(line) = serde_json::to_string(&out) {
            println!("{line}");
        }
    }
}

fn run_user(cmd: Command) -> Result<()> {
    let cfg = Config::load();
    let cwd = std::env::current_dir().context("getting cwd")?;
    match cmd {
        Command::Schedule { file } => {
            let dec = read_decomposition(file)?;
            let errs = schedule::validate(&dec);
            if !errs.is_empty() {
                bail!("invalid decomposition:\n  - {}", errs.join("\n  - "));
            }
            let sched = schedule::schedule(&dec, &cfg.shared_globs);
            println!("{}", serde_json::to_string_pretty(&sched)?);
        }
        Command::Validate { file } => {
            let dec = read_decomposition(file)?;
            let errs = schedule::validate(&dec);
            if errs.is_empty() {
                eprintln!("ok: {} task(s)", dec.tasks.len());
            } else {
                bail!("invalid:\n  - {}", errs.join("\n  - "));
            }
        }
        Command::Worktree { action } => run_worktree(&cfg, &cwd, action)?,
        Command::State { action } => run_state(&cfg, &cwd, action)?,
        Command::Verify { action } => match action {
            VerifyAction::Digest { file } => {
                let raw = match file {
                    Some(p) => std::fs::read_to_string(&p)
                        .with_context(|| format!("reading {}", p.display()))?,
                    None => read_stdin(),
                };
                let digest = verify::distill_failure(&raw);
                println!("{}", serde_json::to_string_pretty(&digest)?);
            }
            VerifyAction::Runtime {
                stdout,
                stderr,
                exit_code,
                reflux,
            } => {
                // stdout: read the file when given, else empty. stderr: read the
                // file when given, else read stdin (the primary input, so empty
                // stdin fails soft to an empty digest and exit 0).
                let stdout_raw = match stdout {
                    Some(p) => std::fs::read_to_string(&p)
                        .with_context(|| format!("reading {}", p.display()))?,
                    None => String::new(),
                };
                let stderr_raw = match stderr {
                    Some(p) => std::fs::read_to_string(&p)
                        .with_context(|| format!("reading {}", p.display()))?,
                    None => read_stdin(),
                };
                if reflux {
                    let verdict =
                        verify::runtime_reflux_verdict(&stdout_raw, &stderr_raw, exit_code);
                    println!("{}", serde_json::to_string_pretty(&verdict)?);
                } else {
                    let digest = verify::distill_runtime(&stdout_raw, &stderr_raw, exit_code);
                    println!("{}", serde_json::to_string_pretty(&digest)?);
                }
            }
            VerifyAction::Launch {
                cmd,
                timeout,
                health_url,
                startup_timeout,
                docker,
                image,
                run_policy,
                cheap_verify,
                divergence,
                change_risk,
                run,
            } => {
                // Fail-soft by contract: all launch paths never panic and always
                // return a verdict, so we always print it and exit 0.
                let verdict = if run_policy {
                    // Purely-additive deterministic RUN-POLICY gate. Takes
                    // precedence only when --run-policy is set; consult
                    // decide_run_policy and launch the container ONLY on the
                    // EscalateDocker verdict (the launcher is injected so the
                    // gate itself does no I/O; docker-absent still fails soft).
                    let cv = cheap_verify.unwrap_or_else(|| "unknown".to_string());
                    let dv = divergence.unwrap_or_else(|| "high".to_string());
                    let cr = change_risk.unwrap_or_else(|| "high".to_string());
                    let workdir = std::env::current_dir()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| ".".to_string());
                    let gate = verify::run_policy_gate(&cv, &dv, &cr, || {
                        verify::launch_in_container(
                            &cmd,
                            timeout,
                            &image,
                            &workdir,
                            &verify::SandboxLimits::default(),
                        )
                    });
                    // Observability: when --run is given, record the chosen
                    // run-policy verdict to the JSONL log, reusing the exact
                    // recording path as `run-policy decide --run`. Fail-soft: a
                    // logging failure must never break the stdout verdict.
                    if let Some(rid) = &run {
                        let decision = run_policy::decide_run_policy(&cv, &dv, &cr);
                        let verdict_str = match decision.verdict {
                            run_policy::RunPolicyVerdict::VerifyOnly => "verify_only",
                            run_policy::RunPolicyVerdict::EscalateDocker => "escalate_docker",
                            run_policy::RunPolicyVerdict::EscalateShip => "escalate_ship",
                            run_policy::RunPolicyVerdict::AskHuman => "ask_human",
                        };
                        let record = state::RunPolicyLogRecord {
                            verdict: verdict_str.to_string(),
                            reason: decision.reason.clone(),
                            cheap_verify: decision.cheap_verify.clone(),
                            divergence: decision.divergence.clone(),
                            change_risk: decision.change_risk.clone(),
                            recorded_at: state::now_secs(),
                        };
                        if let Err(e) = state::record_run_policy_decision(&cfg, &cwd, rid, &record)
                        {
                            eprintln!(
                                "condukt: warning: failed to record run-policy decision: {e}"
                            );
                        }
                    }
                    gate
                } else if docker {
                    let workdir = std::env::current_dir()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| ".".to_string());
                    // `verify launch --docker` keeps the network-only argv
                    // (no resource/fs hardening) — worker sandboxing wires its
                    // own limits via `condukt sandbox run`.
                    verify::launch_in_container(
                        &cmd,
                        timeout,
                        &image,
                        &workdir,
                        &verify::SandboxLimits::default(),
                    )
                } else {
                    // With a health URL we probe a server for a 200; otherwise we
                    // keep the legacy exit-wait behavior.
                    match health_url {
                        Some(url) => verify::launch_server_and_probe(&cmd, &url, startup_timeout),
                        None => verify::launch_and_reflux(&cmd, timeout),
                    }
                };
                println!("{}", serde_json::to_string_pretty(&verdict)?);
            }
        },
        Command::Replan { action } => match action {
            ReplanAction::Handoff { file, run } => {
                let raw = match file {
                    Some(p) => std::fs::read_to_string(&p)
                        .with_context(|| format!("reading {}", p.display()))?,
                    None => read_stdin(),
                };
                let input: ReplanHandoffInput = if raw.trim().is_empty() {
                    ReplanHandoffInput::default()
                } else {
                    serde_json::from_str(&raw).context("parsing replan handoff JSON")?
                };
                let directive = replan::decide_replan(
                    &input.reason,
                    &input.failed_tests,
                    &input.diff,
                    &input.model_tier,
                    &input.done_criteria,
                    &input.task_summary,
                    input.replan_count,
                );
                // Structured observability record — a side effect on top of the
                // (unchanged) stdout directive JSON below. Only written when the
                // caller opts in via `--run` (backward-compatible: omitted =
                // no record, matching the pre-existing stateless behavior).
                if let Some(rid) = &run {
                    let directive_str = match directive.directive {
                        replan::Directive::EscalateModel => "escalate_model",
                        replan::Directive::Replan => "replan",
                        replan::Directive::EscalateToUser => "escalate_to_user",
                    };
                    let record = state::ReplanLogRecord {
                        directive: directive_str.to_string(),
                        reason: directive.classification.reason.clone(),
                        reached_tier: canonical_tier_for_log(&input.model_tier),
                        replan_count: directive.replan_count,
                        recorded_at: state::now_secs(),
                    };
                    // Fail-soft: a logging failure must never break the reflux
                    // cascade's stdout contract.
                    if let Err(e) = state::record_replan_decision(&cfg, &cwd, rid, &record) {
                        eprintln!("condukt: warning: failed to record replan decision: {e}");
                    }
                }
                println!("{}", serde_json::to_string_pretty(&directive)?);
            }
            ReplanAction::Stats { run } => {
                let records = state::load_replan_records(&cfg, &cwd, &run);
                let stats = state::aggregate_replan_stats(&records);
                println!("{}", serde_json::to_string_pretty(&stats)?);
            }
        },
        Command::Sandbox { action } => match action {
            SandboxAction::Run { cmd, timeout } => {
                let cfg = Config::load();
                let workdir = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| ".".to_string());
                // never-break-a-turn: both branches return a verdict JSON and
                // exit 0. When sandboxing is enabled we route through the docker
                // exec backend with the configured image + resource limits;
                // otherwise the command runs unchanged on the host (the
                // container path is never started when disabled).
                let mut verdict = if cfg.worker_sandbox_enabled {
                    let limits = verify::SandboxLimits {
                        memory: cfg.worker_sandbox_memory.clone(),
                        cpus: cfg.worker_sandbox_cpus.clone(),
                        pids_limit: cfg.worker_sandbox_pids_limit,
                        read_only: false,
                    };
                    let image = cfg
                        .worker_sandbox_image
                        .clone()
                        .unwrap_or_else(|| "alpine:latest".to_string());
                    verify::launch_in_container(&cmd, timeout, &image, &workdir, &limits)
                } else {
                    verify::launch_and_reflux(&cmd, timeout)
                };
                // Observability: record which path was taken on the verdict.
                if let serde_json::Value::Object(ref mut m) = verdict {
                    m.insert(
                        "sandboxed".to_string(),
                        serde_json::Value::Bool(cfg.worker_sandbox_enabled),
                    );
                }
                println!("{}", serde_json::to_string_pretty(&verdict)?);
            }
        },
        Command::RunPolicy { action } => match action {
            RunPolicyAction::Decide {
                cheap_verify,
                divergence,
                change_risk,
                run,
            } => {
                let decision =
                    run_policy::decide_run_policy(&cheap_verify, &divergence, &change_risk);
                // Structured observability record — a side effect on top of the
                // (unchanged) stdout decision JSON below. Only written when the
                // caller opts in via `--run` (backward-compatible: omitted =
                // no record, matching the pre-existing stateless behavior).
                if let Some(rid) = &run {
                    let verdict_str = match decision.verdict {
                        run_policy::RunPolicyVerdict::VerifyOnly => "verify_only",
                        run_policy::RunPolicyVerdict::EscalateDocker => "escalate_docker",
                        run_policy::RunPolicyVerdict::EscalateShip => "escalate_ship",
                        run_policy::RunPolicyVerdict::AskHuman => "ask_human",
                    };
                    let record = state::RunPolicyLogRecord {
                        verdict: verdict_str.to_string(),
                        reason: decision.reason.clone(),
                        cheap_verify: decision.cheap_verify.clone(),
                        divergence: decision.divergence.clone(),
                        change_risk: decision.change_risk.clone(),
                        recorded_at: state::now_secs(),
                    };
                    // Fail-soft: a logging failure must never break the
                    // run-policy decision's stdout contract.
                    if let Err(e) = state::record_run_policy_decision(&cfg, &cwd, rid, &record) {
                        eprintln!("condukt: warning: failed to record run-policy decision: {e}");
                    }
                }
                println!("{}", serde_json::to_string_pretty(&decision)?);
            }
            RunPolicyAction::Stats { run } => {
                let records = state::load_run_policy_records(&cfg, &cwd, &run);
                let stats = state::aggregate_run_policy_stats(&records);
                println!("{}", serde_json::to_string_pretty(&stats)?);
            }
        },
        Command::Circuit { action } => match action {
            CircuitAction::Check {
                run,
                streak_cap,
                idle_ttl_secs,
                budget_cap_usd,
            } => {
                // The handler prints the verdict JSON + journals fail-soft and
                // returns the exit code (0 = continue, 1 = trip). Exit directly
                // so the loops can branch on the process status.
                let code = circuit::run_circuit_check(
                    &cfg,
                    &cwd,
                    &run,
                    streak_cap,
                    idle_ttl_secs,
                    budget_cap_usd,
                );
                std::process::exit(code);
            }
        },
        Command::Gate { action } => match action {
            GateAction::Check { run, task } => {
                // The handler prints the verdict JSON, checkpoints + journals
                // (fail-soft) and returns the exit code (0 = auto-exec, 1 =
                // escalate). Exit directly so a caller can branch on the status.
                let code = gate_exec::run_gate_check(&cfg, &cwd, &run, &task);
                std::process::exit(code);
            }
        },
        Command::Consensus { action } => run_consensus(&cfg, action)?,
        Command::Init => init(&cfg)?,
        Command::Install { dry_run } => {
            if dry_run {
                install::dry_run()?;
            } else {
                install::install()?;
            }
        }
        Command::Uninstall => install::uninstall()?,
        Command::Knowledge => {
            let path = config::base_dir().join("knowledge.md");
            if path.exists() {
                print!("{}", std::fs::read_to_string(&path)?);
            }
        }
        Command::Pr { action } => run_pr(&cfg, &cwd, action)?,
        Command::Loop {
            module,
            iteration,
            prev_failures,
        } => {
            use crate::config::ModuleCycle;
            use crate::state::{loop_should_stop, run_cycle};
            let cycle = ModuleCycle::from_str(&module)
                .ok_or_else(|| anyhow!("unknown module '{module}'; use server, client, or e2e"))?;
            let result = run_cycle(&cfg, &cwd, cycle)?;
            let (stop, stop_reason) = loop_should_stop(prev_failures, result.failure_count);
            let out = serde_json::json!({
                "iteration": iteration,
                "module": module,
                "failure_count": result.failure_count,
                "success": result.success,
                "stop": stop,
                "stop_reason": stop_reason,
                "output": result.output,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
            // Exit non-zero when not all tests pass so shell scripts can branch on it.
            if !result.success && stop {
                std::process::exit(1);
            }
        }
        Command::Policy { action } => run_policy(action),
        Command::Escalate { action } => run_escalate(&cfg, &cwd, action)?,
        Command::Status { all } => status::render(&cfg, &cwd, all),
        // These are dispatched as hooks in main() (via run_hook, which exits and
        // never returns here). Reaching this arm would be an internal dispatch
        // bug; return a clean error instead of panicking the process.
        Command::Restore | Command::Statusline | Command::Editgate => {
            bail!("internal: hook subcommands must be dispatched in main(), not run_user()")
        }
    }
    Ok(())
}

/// `condukt policy decide` — parse the three graded levels, print the
/// [`policy::Decision`] and exit with the documented contract (0=auto,
/// 2=escalate, 3=block, 1=invalid input). Never panics: an unparseable level
/// prints a message to stderr and exits 1.
/// Parse the three graded-autonomy levels or exit 1 (invalid input) — the
/// shared front half of `policy decide` and `policy answer`.
fn parse_policy_levels(
    risk: &str,
    reversible: &str,
    confidence: &str,
) -> (policy::Level, policy::Level, policy::Level) {
    let parsed = policy::parse_level(risk)
        .zip(policy::parse_level(reversible))
        .zip(policy::parse_level(confidence));
    match parsed {
        Some(((r, rev), c)) => (r, rev, c),
        None => {
            eprintln!("condukt: --risk/--reversible/--confidence must each be low|medium|high");
            std::process::exit(1);
        }
    }
}

/// Soft dependency: resolve a *calibrated* confidence [`policy::Level`] from
/// fugu-router when task-identifying flags are supplied, else `None`.
///
/// This is the I/O boundary that keeps `policy::decide` / `Level::from_score`
/// pure. The entire path is gated behind at least one of the new optional flags
/// (`--title`/`--files`/`--class`): when none are supplied it returns `None`
/// immediately with NO shell-out, so the legacy CLI invocation is byte-for-byte
/// unchanged. When a flag IS supplied it shells out to `fugu-router confidence`
/// (a sibling task adds that subcommand; it prints a single float in `[0,1]` to
/// stdout when it has sufficient history), parses the float, and maps it through
/// [`policy::Level::from_score`].
///
/// Wire contract for the sibling subcommand:
/// `fugu-router confidence [--files <csv>] [--class <c>] <title text...>` — the
/// title is a POSITIONAL argument (no `--title` flag), passed LAST after the
/// flags. `--files` is comma-joined (fugu splits on `,`).
///
/// Every failure mode falls through to `None` (never a hard error), mirroring
/// the `fugu_fingerprint` / `record_runs` soft-probe precedent:
/// - fugu-router not on PATH → `Command::output` errors → `None`
/// - non-zero exit (e.g. insufficient history) → `None`
/// - empty / unparseable / non-finite stdout → `None`
fn calibrated_confidence(
    title: &Option<String>,
    files: &[String],
    class: &Option<String>,
) -> Option<policy::Level> {
    // Gate: no new flags → the calibrated path is entirely inert (no shell-out).
    if title.is_none() && files.is_empty() && class.is_none() {
        return None;
    }
    let mut cmd = std::process::Command::new("fugu-router");
    cmd.arg("confidence");
    if !files.is_empty() {
        cmd.args(["--files", &files.join(",")]);
    }
    if let Some(c) = class {
        cmd.args(["--class", c]);
    }
    // Title is a POSITIONAL argument (no --title flag), passed last after the
    // flags so clap accepts it as the trailing free-form title text.
    if let Some(t) = title {
        cmd.arg(t);
    }
    let out = cmd.output().ok()?; // spawn failed (not on PATH) → soft-skip
    if !out.status.success() {
        return None; // error / insufficient history → fall back
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let score: f64 = raw.trim().parse().ok()?; // empty/unparseable → fall back
    if !score.is_finite() {
        return None; // NaN/±inf on stdout → fall back
    }
    Some(policy::Level::from_score(score))
}

/// `condukt escalate add|list|resolve` — the durable async escalation channel.
/// Deterministic store I/O; fail-soft (missing/corrupt store = empty). The
/// created-at timestamp uses wall-clock seconds (observability only; it never
/// feeds the deterministic record id).
fn run_escalate(cfg: &Config, cwd: &Path, action: EscalateAction) -> Result<()> {
    match action {
        EscalateAction::Add {
            run,
            task,
            question,
            option,
            recommend,
        } => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let rec = escalate::add_escalation(
                cfg, cwd, &run, &task, &question, &option, recommend, now,
            )?;
            // id on the first line for easy capture, then the full record.
            println!("{}", rec.id);
            println!("{}", serde_json::to_string_pretty(&rec)?);
        }
        EscalateAction::List { run, json } => {
            let open = escalate::list_escalations(cfg, cwd, &run)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&open)?);
            } else if open.is_empty() {
                println!("No open escalations for run {run}.");
            } else {
                println!("Open escalations for run {run}:");
                for e in &open {
                    let rec = e
                        .options
                        .get(e.recommended)
                        .map(|o| format!(" (recommend: {o})"))
                        .unwrap_or_default();
                    println!("  {}  [{}] {}{}", e.id, e.task, e.question, rec);
                    println!("        options: {}", e.options.join(" | "));
                }
            }
        }
        EscalateAction::Resolve { id, choice } => {
            match escalate::resolve_escalation(cfg, cwd, &id, &choice)? {
                Some(rec) => println!("{}", serde_json::to_string_pretty(&rec)?),
                None => {
                    eprintln!("condukt: no escalation with id '{id}'");
                    std::process::exit(1);
                }
            }
        }
    }
    Ok(())
}

fn run_policy(action: PolicyAction) -> ! {
    match action {
        PolicyAction::Decide {
            risk,
            reversible,
            confidence,
            title,
            files,
            class,
        } => {
            let (risk, reversible, mut confidence) =
                parse_policy_levels(&risk, &reversible, &confidence);
            // Soft dependency: when the new task-identifying flags are supplied
            // and fugu-router yields a usable calibrated score, it overrides the
            // self-reported confidence. Absent the flags (legacy invocation)
            // this is inert and confidence stays exactly as parsed.
            if let Some(cal) = calibrated_confidence(&title, &files, &class) {
                confidence = cal;
            }
            let decision = policy::decide(risk, reversible, confidence);
            println!("{decision}");
            let code = match decision {
                policy::Decision::Auto => 0,
                policy::Decision::Escalate => 2,
                policy::Decision::Block => 3,
            };
            std::process::exit(code);
        }
        PolicyAction::Answer {
            risk,
            reversible,
            confidence,
            question,
            options,
            recommend,
            journal_dir,
            title,
            files,
            class,
        } => {
            let (risk, reversible, mut confidence) =
                parse_policy_levels(&risk, &reversible, &confidence);
            // Same soft-dependency override as `decide` (see there). Inert on
            // the legacy invocation (no --title/--files/--class).
            if let Some(cal) = calibrated_confidence(&title, &files, &class) {
                confidence = cal;
            }
            let decision = policy::decide(risk, reversible, confidence);
            let outcome = gatelog::answer_outcome(decision, &options, recommend);
            match &outcome {
                gatelog::AnswerOutcome::Answered {
                    chosen,
                    recommend_index,
                } => {
                    // Self-answer: record the choice for audit, then emit it.
                    let dir = journal_dir.unwrap_or_else(|| config::Config::load().state_dir);
                    let entry = gatelog::GateDecision {
                        question,
                        options: options.clone(),
                        recommend_index: *recommend_index,
                        chosen: chosen.clone(),
                        policy: "auto".to_string(),
                        created_at: state::now_secs(),
                    };
                    gatelog::append_decision(&dir, &entry);
                    // Serialize `chosen` so quotes/backslashes in an option can't
                    // break the JSON line.
                    let chosen_json =
                        serde_json::to_string(chosen).unwrap_or_else(|_| "\"\"".to_string());
                    println!(
                        "{{\"answered\":true,\"policy\":\"auto\",\"chosen\":{chosen_json},\"recommend_index\":{recommend_index}}}"
                    );
                }
                gatelog::AnswerOutcome::Escalate => {
                    println!("{{\"answered\":false,\"policy\":\"escalate\"}}");
                }
                gatelog::AnswerOutcome::Block => {
                    println!("{{\"answered\":false,\"policy\":\"block\"}}");
                }
                gatelog::AnswerOutcome::Invalid => {
                    eprintln!(
                        "condukt: --recommend index {recommend} has no matching --option \
                         (got {} option(s))",
                        options.len()
                    );
                }
            }
            std::process::exit(outcome.exit_code());
        }
        PolicyAction::Answers { journal_dir } => {
            let dir = journal_dir.unwrap_or_else(|| config::Config::load().state_dir);
            for entry in gatelog::load_decisions(&dir) {
                if let Ok(line) = serde_json::to_string(&entry) {
                    println!("{line}");
                }
            }
            std::process::exit(0);
        }
    }
}

fn run_worktree(cfg: &Config, cwd: &Path, action: WtAction) -> Result<()> {
    let repo = worktree::toplevel(cwd)?;
    match action {
        WtAction::Create { topic, branch } => {
            let path = worktree::create(&repo, &cfg.worktree_base, &topic, &branch)?;
            println!("{}", path.display());
        }
        WtAction::Merge { branch } => worktree::merge(&repo, &branch, &cfg.default_branch)?,
        WtAction::Remove { path, branch } => {
            let undeleted = worktree::remove(&repo, &path, branch.as_deref())?;
            if let Some(b) = undeleted {
                eprintln!(
                    "condukt: worktree removed but branch '{}' remains — \
                     it has unmerged commits. Merge or force-delete it manually.",
                    b
                );
            }
        }
        WtAction::Cleanup { remove } => {
            let orphans = worktree::orphans(&repo, &cfg.worktree_base)?;
            let _ = worktree::git(&repo, &["worktree", "prune"]);
            if orphans.is_empty() {
                eprintln!("no orphan worktrees under {}", cfg.worktree_base.display());
            }
            for o in orphans {
                if remove {
                    std::fs::remove_dir_all(&o)
                        .with_context(|| format!("removing {}", o.display()))?;
                    eprintln!("removed orphan {}", o.display());
                } else {
                    eprintln!("orphan (pass --remove to delete): {}", o.display());
                }
            }
        }
        WtAction::List => {
            for (p, b) in worktree::list(&repo)? {
                println!("{}\t{}", p.display(), b.unwrap_or_else(|| "-".into()));
            }
        }
    }
    Ok(())
}

/// JSON input for `consensus vote`: either a bare array of verdicts, or an
/// object that may also carry a `threshold`.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ConsensusInput {
    Wrapped {
        #[serde(default)]
        verdicts: Vec<consensus::Verdict>,
        #[serde(default)]
        threshold: Option<f64>,
    },
    Bare(Vec<consensus::Verdict>),
}

/// JSON input for `replan handoff`: the reflux facts plus the original
/// task's done_criteria/summary. All fields optional, defaulting to empty
/// strings (mirrors `failure_context`'s shape used elsewhere in the /condukt
/// skill's cascade; `model_tier`/`done_criteria`/`task_summary` are the extra
/// fields this subcommand needs beyond the bare `failure_context`).
/// `replan_count` tracks how many times this task has been replanned (default 0,
/// backward-compatible).
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ReplanHandoffInput {
    reason: String,
    failed_tests: String,
    diff: String,
    model_tier: String,
    done_criteria: String,
    task_summary: String,
    replan_count: usize,
}

/// Collapse a model tier string to its canonical keyword for the replan
/// decision log (mirrors `replan::canonical_tier`'s collapsing logic without
/// reaching into `replan.rs`'s private helper — this is purely a display/log
/// normalization, not part of the replan decision itself). Never panics on
/// empty/garbage input.
fn canonical_tier_for_log(model_tier: &str) -> String {
    const TIERS: [&str; 3] = ["haiku", "sonnet", "opus"];
    let m = model_tier.trim().to_lowercase();
    for t in TIERS {
        if m.contains(t) {
            return t.to_string();
        }
    }
    m
}

fn run_consensus(cfg: &Config, action: ConsensusAction) -> Result<()> {
    match action {
        ConsensusAction::Plan { risk } => {
            let plan = consensus::plan(
                cfg.consensus_enabled,
                cfg.consensus_samples,
                cfg.consensus_threshold,
                risk.as_deref(),
            );
            println!("{}", serde_json::to_string(&plan)?);
            // Exit-code contract (mirrors `state autonomy-check`): 0 = fan out,
            // 1 = ordinary single-sample path. The skill branches on this.
            if !plan.enabled {
                std::process::exit(1);
            }
        }
        ConsensusAction::Vote { file, threshold } => {
            let raw = match &file {
                Some(p) => std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?,
                None => read_stdin(),
            };
            let input: ConsensusInput =
                serde_json::from_str(&raw).context("parsing consensus verdicts JSON")?;
            let (verdicts, json_threshold) = match input {
                ConsensusInput::Wrapped {
                    verdicts,
                    threshold,
                } => (verdicts, threshold),
                ConsensusInput::Bare(v) => (v, None),
            };
            // Precedence: --threshold flag > JSON threshold > config > default.
            let thr = threshold
                .or(json_threshold)
                .unwrap_or(cfg.consensus_threshold);
            if !(0.0..=1.0).contains(&thr) {
                bail!("threshold must be within [0.0, 1.0], got {thr}");
            }
            let decision = consensus::tally(&verdicts, thr);
            println!("{}", serde_json::to_string_pretty(&decision)?);
            // Exit 1 when the caller should escalate (all-fail, tie, or agreement
            // below threshold); 0 when a consensus winner is taken. Distinct JSON
            // on stdout lets the skill tell this apart from a hard error.
            if decision.escalate {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// Spawn `gh <argv>` and map it to the injected-runner shape `detect_gh` expects:
/// `Some((success, combined_output))`, or `None` when the binary can't be spawned
/// (gh absent). Never panics — a spawn failure is the fail-soft "absent" signal.
fn gh_probe(argv: &[&str]) -> Option<(bool, String)> {
    let out = std::process::Command::new("gh").args(argv).output().ok()?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Some((out.status.success(), combined))
}

/// Extract the PR URL from `gh pr create` stdout: gh prints the URL (typically
/// the last non-empty line). Falls back to the trimmed stdout when no
/// `http(s)://` line is found.
fn parse_pr_url(stdout: &str) -> String {
    for line in stdout.lines().rev() {
        let t = line.trim();
        if t.starts_with("http://") || t.starts_with("https://") {
            return t.to_string();
        }
    }
    stdout.trim().to_string()
}

/// Keep the last `n` non-empty trailing chars of gh stderr for a degrade reason,
/// so the DegradedLocalOnly reason is informative but bounded.
fn stderr_tail(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.len() <= max {
        return t.to_string();
    }
    let start = t.len().saturating_sub(max);
    // Respect char boundaries (t is UTF-8): find the next boundary at/after start.
    let start = (start..=t.len())
        .find(|i| t.is_char_boundary(*i))
        .unwrap_or(t.len());
    t[start..].to_string()
}

fn run_pr(cfg: &Config, cwd: &Path, action: PrAction) -> Result<()> {
    match action {
        PrAction::Poll {
            pr,
            branch,
            execute,
        } => {
            let branch = branch.unwrap_or_else(current_branch);

            // Fetch CI status through the SAME injected-runner pattern as the PR
            // path (gh spawned via gh_probe), mapped by the pure state machine.
            let argv = ci::build_ci_checks_args(&pr);
            let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
            let verdict = ci::fetch_and_decide(gh_probe, &argv_ref);

            // Merge gate: the real worktree::merge runs ONLY on the green verdict
            // AND with --execute (the GATED gate). Every other verdict — Wait,
            // Reenter, DegradedLocalOnly — leaves the work as local commits.
            match &verdict {
                ci::CiVerdict::Merge if execute => {
                    let repo = worktree::toplevel(cwd)?;
                    // Fail-soft: a merge failure (e.g. conflicts) is reported and
                    // the turn still exits 0 — it never breaks the turn.
                    match worktree::merge(&repo, &branch, &cfg.default_branch) {
                        Ok(()) => {
                            println!("CI green: merged '{branch}' into '{}'.", cfg.default_branch)
                        }
                        Err(e) => println!(
                            "CI green but merge of '{branch}' into '{}' failed; \
                             left work as local commits (fail-soft): {e:#}",
                            cfg.default_branch
                        ),
                    }
                }
                ci::CiVerdict::Merge => println!(
                    "CI green: would merge '{branch}' into '{}' \
                     (dry-run; pass --execute to merge).",
                    cfg.default_branch
                ),
                ci::CiVerdict::Wait => {
                    println!("CI still running for '{pr}': waiting (no merge).")
                }
                ci::CiVerdict::Reenter => {
                    println!("CI failed for '{pr}': re-enter the worker to fix it (no merge).")
                }
                ci::CiVerdict::DegradedLocalOnly { reason } => println!(
                    "CI status unavailable for '{pr}': {reason}; \
                     left work as local commits (no merge)."
                ),
            }
        }
        PrAction::Create {
            title,
            body,
            body_file,
            head,
            base,
            execute,
        } => {
            // Resolve the body: --body wins; else --body-file; else empty.
            let body = match (body, body_file) {
                (Some(b), _) => b,
                (None, Some(p)) => std::fs::read_to_string(&p)
                    .with_context(|| format!("reading body file {}", p.display()))?,
                (None, None) => String::new(),
            };
            // Resolve head: --head, else the current git branch, else empty
            // (gh infers the current branch when --head is empty anyway).
            let head = head.unwrap_or_else(current_branch);
            let base = base.unwrap_or_else(|| cfg.default_branch.clone());

            let plan = pr::PrPlan {
                title,
                body,
                head,
                base,
            };

            // Detect gh via real spawns, mapped through the pure detector.
            let status = pr::detect_gh(gh_probe);
            let outcome = pr::decide_pr(&status, &plan, execute);

            // Execute path (the GATED gate): only when --execute AND gh is usable
            // did decide_pr return Prepared for a usable gh. Run gh with the args,
            // parse the URL into Created; degrade soft on any gh failure.
            let final_outcome = match (&outcome, execute) {
                (pr::PrOutcome::Prepared { args }, true) => run_gh_create(args),
                _ => outcome,
            };

            // All paths print a JSON PrOutcome and exit 0 (never break the turn).
            println!("{}", serde_json::to_string_pretty(&final_outcome)?);
        }
    }
    Ok(())
}

/// Run `gh <args>` for the executed PR-create path. On success, parse the URL
/// into [`pr::PrOutcome::Created`]; on any failure (spawn or non-zero exit),
/// degrade soft to [`pr::PrOutcome::DegradedLocalOnly`] so the turn is not broken.
fn run_gh_create(args: &[String]) -> pr::PrOutcome {
    match std::process::Command::new("gh").args(args).output() {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            pr::PrOutcome::Created {
                url: parse_pr_url(&stdout),
            }
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            pr::PrOutcome::DegradedLocalOnly {
                reason: format!(
                    "gh pr create failed; left work as local commits: {}",
                    stderr_tail(&stderr, 200)
                ),
            }
        }
        Err(e) => pr::PrOutcome::DegradedLocalOnly {
            reason: format!("gh pr create could not be spawned; left work as local commits: {e}"),
        },
    }
}

/// Best-effort current git branch (`git rev-parse --abbrev-ref HEAD`), trimmed.
/// Empty string on any failure — gh then infers the current branch itself.
fn current_branch() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn run_state(cfg: &Config, cwd: &Path, action: StateAction) -> Result<()> {
    match action {
        StateAction::Init { run, file, label } => {
            let raw = match &file {
                Some(p) => std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?,
                None => read_stdin(),
            };
            let dec: model::Decomposition =
                serde_json::from_str(&raw).context("parsing decomposition JSON")?;
            let errs = schedule::validate(&dec);
            if !errs.is_empty() {
                bail!("invalid decomposition:\n  - {}", errs.join("\n  - "));
            }
            let run_id = run.unwrap_or_else(default_run_id);
            let resolved_label = label.or_else(resolve_label);
            let tasks = dec
                .tasks
                .iter()
                .map(|t| state::TaskState {
                    id: t.id.clone(),
                    status: state::Status::Pending,
                    ..Default::default()
                })
                .collect();
            let rs = state::RunState {
                run_id: run_id.clone(),
                goal: dec.goal.clone(),
                tasks,
                paused: false,
                terminal_label: resolved_label,
                recorded_at: None,
            };
            let path = rs.save(cfg, cwd)?;
            // Persist the decomposition so `state resume-context` can reconstruct tasks.
            state::save_decomposition(cfg, cwd, &run_id, &raw)?;
            eprintln!("initialized run '{run_id}' at {}", path.display());
            println!("{run_id}");
        }
        StateAction::ConflictCheck { file } => {
            let raw = match &file {
                Some(p) => std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?,
                None => read_stdin(),
            };
            let dec: model::Decomposition =
                serde_json::from_str(&raw).context("parsing decomposition JSON")?;
            let report = state::cross_run_conflicts(cfg, cwd, &dec);
            println!("{}", serde_json::to_string_pretty(&report)?);
            if report.has_conflicts {
                std::process::exit(1);
            }
        }
        StateAction::Set {
            run,
            task,
            status,
            worktree,
            branch,
            clear_worktree,
            clear_branch,
            model,
            cost,
        } => {
            // Hold the per-run state lock across the entire load → oracle-gate →
            // mutate → save cycle so a concurrent session/worktree cannot lose
            // this update (last-writer-wins TOCTOU). Fail-soft: degrades to
            // unlocked on contention rather than failing the update.
            let _lock = lock::RunLock::acquire(cfg, cwd, &run);
            let mut rs = state::RunState::load(cfg, cwd, &run)?;
            let st: state::Status = status.parse()?;

            // Cross-session claim guard (PDO collision): before marking a task
            // RUNNING, claim the files it will touch. If a live holder from
            // ANOTHER run owns any of them, hard-skip — its work is already in
            // flight elsewhere, so we must not process it again. Non-conflicting
            // files are still claimed so sibling tasks proceed (partial progress).
            // Fail-soft: a task with no declared touched_files is not guarded.
            if st == state::Status::Running {
                let files = task_files(cfg, cwd, &run, &task);
                if !files.is_empty() {
                    let session = session_id_from_env();
                    let out = claim::claim_files(
                        cfg,
                        cwd,
                        &run,
                        session.as_deref(),
                        &files,
                        state::now_secs(),
                    )?;
                    if !out.skipped.is_empty() {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "skipped": true,
                                "run": run,
                                "task": task,
                                "conflicts": out.skipped,
                            }))?
                        );
                        eprintln!(
                            "condukt: task '{task}' skipped — {} file(s) already \
                             claimed by a live run",
                            out.skipped.len()
                        );
                        std::process::exit(1);
                    }
                }
            }

            // F→P oracle completion gate: before promoting a task to
            // `verified`, ask `tdd oracle` (via `oracle::check_oracle`)
            // whether it carries a valid Fail→Pass reproduction proof. This
            // mirrors `CheckOracle`'s load + run_dir resolution exactly, but
            // fails soft (degrade to the legacy gate) whenever the
            // decomposition or matching task can't be found — a run-state
            // task with no matching decomposition entry must still be
            // settable to verified.
            let mut fp_gate_value: Option<bool> = None;
            if st == state::Status::Verified {
                if let Ok(dec_raw) = state::load_decomposition(cfg, cwd, &run) {
                    if let Ok(dec) = serde_json::from_str::<model::Decomposition>(&dec_raw) {
                        if let Some(dt) = dec.tasks.iter().find(|dt| dt.id == task) {
                            // Resolve run_dir from `rs` (immutable read) into a
                            // local *before* taking the `&mut` borrow below.
                            let run_dir = rs
                                .tasks
                                .iter()
                                .find(|s| s.id == task)
                                .and_then(|s| s.worktree.as_deref())
                                .map(std::path::PathBuf::from)
                                .unwrap_or_else(|| cwd.to_path_buf());
                            let verdict = oracle::check_oracle(
                                dt.requires_fp_oracle(),
                                dt.reproduction_tests.as_deref(),
                                &task,
                                &run_dir,
                            );
                            match state::enforce_fp_gate(&verdict) {
                                state::FpGateDecision::Reject => {
                                    bail!(
                                        "refusing to verify task '{task}': no valid fail-to-pass reproduction oracle ({})",
                                        verdict
                                            .get("reason")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("see `condukt state check-oracle` for details")
                                    );
                                }
                                state::FpGateDecision::Allow(v) => {
                                    fp_gate_value = v;
                                }
                            }
                        }
                    }
                }
            }

            let t = rs
                .tasks
                .iter_mut()
                .find(|t| t.id == task)
                .ok_or_else(|| anyhow!("no task '{task}' in run '{run}'"))?;
            let prior_status = t.status;
            t.status = st;
            t.updated_at = Some(state::now_secs());
            if st == state::Status::Verified {
                t.fp_oracle_valid = fp_gate_value;
            }
            // Record the model/cost used so the fugu-router outcome reflects the
            // truth (incl. escalation), not just the originally suggested model.
            if model.is_some() {
                t.model = model;
            }
            if cost.is_some() {
                t.cost_usd = cost;
            }
            // None 上書き保護: --worktree/--branch が省略された場合は既存値を保持する。
            // 明示的にクリアしたい場合は --clear-worktree / --clear-branch を使う。
            if clear_worktree {
                t.worktree = None;
            } else if worktree.is_some() {
                t.worktree = worktree;
            }
            if clear_branch {
                t.branch = None;
                t.branch_sha = None;
            } else if branch.is_some() {
                // Record the SHA at assignment time so reconcile can detect
                // force-push false-positives by checking the original commit.
                if let Some(ref b) = branch {
                    if t.branch_sha.is_none() {
                        let repo = worktree::toplevel(cwd).ok();
                        t.branch_sha = repo
                            .as_deref()
                            .and_then(|r| worktree::git(r, &["rev-parse", b]).ok())
                            .map(|s| s.trim().to_string());
                    }
                }
                t.branch = branch;
            }
            rs.save(cfg, cwd)?;
            let (done, total) = rs.counts();
            eprintln!("run '{run}': {done}/{total} verified");
            // Auto-rollback (charter #7): a task that fails after having been
            // verified restores the run to the last checkpoint — the safety net
            // that makes unattended proceeding reversible. Fail-soft: a missing
            // checkpoint or any restore error is logged, never breaks the turn.
            if st == state::Status::Failed && prior_status == state::Status::Verified {
                let dir = checkpoint_project_dir(cfg, cwd, &run);
                if let Some(cp) = checkpoint::latest_checkpoint(&dir, &run) {
                    restore_checkpoint(
                        cfg,
                        cwd,
                        &dir,
                        &run,
                        &cp,
                        checkpoint::JournalKind::AutoRollback,
                    );
                    eprintln!(
                        "auto-rollback: verified task '{task}' failed; restored checkpoint {}",
                        cp.seq
                    );
                }
            }
            // Claim upkeep (PDO): release this task's files once it reaches a
            // terminal state (its work is done — free them for other sessions),
            // and refresh the run's remaining claims so a live-but-quiet session
            // is not reaped mid-run. Both are fail-soft — never break the update.
            if matches!(
                st,
                state::Status::Verified | state::Status::Failed | state::Status::Cancelled
            ) {
                let files = task_files(cfg, cwd, &run, &task);
                if !files.is_empty() {
                    let _ = claim::release_files(cfg, cwd, &run, &files);
                }
            }
            let _ = claim::heartbeat(cfg, cwd, &run, state::now_secs());
        }
        StateAction::Show { run } => {
            let rs = state::RunState::load(cfg, cwd, &run)?;
            println!("{}", serde_json::to_string_pretty(&rs)?);
        }
        StateAction::Claim { run, session, file } => {
            let files = if file.is_empty() {
                decomposition_files(cfg, cwd, &run)
            } else {
                file
            };
            let session = session.or_else(session_id_from_env);
            let out = claim::claim_files(
                cfg,
                cwd,
                &run,
                session.as_deref(),
                &files,
                state::now_secs(),
            )?;
            println!("{}", serde_json::to_string_pretty(&out)?);
            if !out.skipped.is_empty() {
                std::process::exit(1);
            }
        }
        StateAction::Release { run, file } => {
            let n = if file.is_empty() {
                claim::release_run(cfg, cwd, &run)?
            } else {
                claim::release_files(cfg, cwd, &run, &file)?
            };
            eprintln!("released {n} claim(s) for run '{run}'");
        }
        StateAction::Heartbeat { run } => {
            let n = claim::heartbeat(cfg, cwd, &run, state::now_secs())?;
            eprintln!("refreshed {n} claim(s) for run '{run}'");
        }
        StateAction::Claims => {
            let live = claim::active_claims(cfg, cwd, state::now_secs())?;
            println!("{}", serde_json::to_string_pretty(&live)?);
        }
        StateAction::ClaimTask {
            run,
            session,
            title,
            hashkey,
        } => {
            let session = session.or_else(session_id_from_env);
            let out = claim::claim_tasks(
                cfg,
                cwd,
                &hashkey,
                &run,
                session.as_deref(),
                state::now_secs(),
                title.as_deref(),
            )?;
            println!("{}", serde_json::to_string_pretty(&out)?);
            if !out.skipped.is_empty() {
                std::process::exit(1);
            }
        }
        StateAction::ReleaseTask { run, hashkey } => {
            let n = claim::release_tasks(cfg, cwd, &hashkey)?;
            eprintln!("released {n} task claim(s) for run '{run}'");
        }
        StateAction::IsClaimed { hashkey } => {
            let live = claim::active_claims(cfg, cwd, state::now_secs())?;
            let holder = live.task_claims.get(&hashkey);
            let claimed = holder.is_some();
            let out = serde_json::json!({
                "hashkey": hashkey,
                "claimed": claimed,
                "holder_run": holder.map(|c| c.run_id.clone()),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
            if !claimed {
                std::process::exit(1);
            }
        }
        StateAction::ExecutionState => {
            let entries = claim::write_execution_state(cfg, cwd, state::now_secs())?;
            println!("{}", serde_json::to_string_pretty(&entries)?);
        }
        StateAction::RecordRun { run, all } => {
            record_runs(cfg, cwd, run, all)?;
        }
        StateAction::Gate { run } => {
            let rs = state::RunState::load(cfg, cwd, &run)?;
            let reasons = state::gate_reasons(cfg, cwd, &rs);
            if reasons.is_empty() {
                eprintln!("gate PASS: run '{run}' complete");
            } else {
                eprintln!("gate FAIL ({} reason(s)):", reasons.len());
                for r in &reasons {
                    eprintln!("  - {r}");
                }
                std::process::exit(1);
            }
        }
        StateAction::Reconcile { run, dry_run } => {
            let rs = state::RunState::load(cfg, cwd, &run)?;
            let (updated, changes) = state::reconcile_run(cfg, cwd, rs, &cfg.default_branch)?;
            if changes.is_empty() {
                eprintln!("reconcile: nothing to change for run '{run}'");
            } else {
                for c in &changes {
                    if c.old_status == c.new_status {
                        eprintln!(
                            "  [{}]  {} (status unchanged: {:?})",
                            c.task_id, c.reason, c.old_status
                        );
                    } else {
                        eprintln!(
                            "  [{}]  {:?} → {:?}  ({})",
                            c.task_id, c.old_status, c.new_status, c.reason
                        );
                    }
                }
                if dry_run {
                    eprintln!("dry-run: {} change(s) would be applied", changes.len());
                } else {
                    updated.save(cfg, cwd)?;
                    let (done, total) = updated.counts();
                    eprintln!(
                        "reconcile: applied {} change(s) — run '{run}': {done}/{total} verified",
                        changes.len()
                    );
                }
            }
        }
        StateAction::List => {
            for rs in state::open_runs(cfg, cwd) {
                let (done, total) = rs.counts();
                let paused_tag = if rs.paused { " [paused]" } else { "" };
                let label_tag = rs
                    .terminal_label
                    .as_deref()
                    .map(|l| format!(" @{l}"))
                    .unwrap_or_default();
                println!(
                    "{}\t{}/{}\t{}{}{}",
                    rs.run_id, done, total, rs.goal, paused_tag, label_tag
                );
            }
        }
        StateAction::Stats => {
            let all = state::compute_stats(cfg, cwd);
            if all.is_empty() {
                eprintln!("no runs found");
                return Ok(());
            }
            let total_runs = all.len();
            let complete = all.iter().filter(|s| s.is_complete).count();
            let rate = (complete * 100).checked_div(total_runs).unwrap_or(0);
            eprintln!("condukt stats: {complete}/{total_runs} complete ({rate}%)\n");
            // Header
            println!("{:<32}  {:>6}  {:>6}  goal", "run_id", "done", "total");
            println!("{}", "-".repeat(72));
            for s in &all {
                let marker = if s.is_complete { "✓" } else { " " };
                let goal_short = truncate_chars(&s.goal, 30);
                println!(
                    "{marker} {:<30}  {:>6}  {:>6}  {}",
                    &s.run_id, s.verified, s.total, goal_short
                );
            }
            // Task status distribution across all runs.
            let mut totals: std::collections::HashMap<String, usize> = Default::default();
            for s in &all {
                for (k, v) in &s.status_counts {
                    *totals.entry(k.clone()).or_insert(0) += v;
                }
            }
            println!("\ntask status distribution (all runs):");
            let mut pairs: Vec<_> = totals.into_iter().collect();
            pairs.sort_by_key(|b| std::cmp::Reverse(b.1));
            for (k, v) in pairs {
                println!("  {k}: {v}");
            }
        }
        StateAction::ResumeContext { run } => {
            let rs = state::RunState::load(cfg, cwd, &run)?;
            let dec_raw = state::load_decomposition(cfg, cwd, &run)?;
            let dec: model::Decomposition =
                serde_json::from_str(&dec_raw).context("parsing saved decomposition")?;
            // Build a map from task id → full decomposition task.
            let dec_map: std::collections::HashMap<_, _> =
                dec.tasks.iter().map(|t| (t.id.clone(), t)).collect();

            let mut pending = Vec::new();
            let mut failed = Vec::new();
            let mut needs_verification = Vec::new();
            let mut verified_count = 0usize;

            for ts in &rs.tasks {
                let base = dec_map
                    .get(&ts.id)
                    .map(|t| serde_json::to_value(t).unwrap_or_default());
                let entry = serde_json::json!({
                    "id": ts.id,
                    "status": format!("{:?}", ts.status).to_lowercase(),
                    "worktree": ts.worktree,
                    "branch": ts.branch,
                    "task": base,
                });
                match ts.status {
                    state::Status::Pending | state::Status::Running => pending.push(entry),
                    state::Status::Failed => failed.push(entry),
                    state::Status::Done => needs_verification.push(entry),
                    state::Status::Verified | state::Status::Cancelled => verified_count += 1,
                }
            }

            let out = serde_json::json!({
                "run_id": rs.run_id,
                "goal": rs.goal,
                "verified_count": verified_count,
                "total_count": rs.tasks.len(),
                "pending_tasks": pending,
                "failed_tasks": failed,
                "needs_verification": needs_verification,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        StateAction::Test { run } => {
            let rs = state::RunState::load(cfg, cwd, &run)?;
            state::run_tests(cfg, cwd, &rs)?;
        }
        StateAction::Abandon {
            run,
            task,
            all_stuck,
        } => {
            let mut rs = state::RunState::load(cfg, cwd, &run)?;
            let ids: Vec<String> = if let Some(task_id) = task {
                // Specific task: validate it exists and is running/failed.
                let t = rs
                    .tasks
                    .iter()
                    .find(|t| t.id == task_id)
                    .ok_or_else(|| anyhow!("no task '{task_id}' in run '{run}'"))?;
                if t.status != state::Status::Running && t.status != state::Status::Failed {
                    bail!(
                        "task '{task_id}' has status {:?}; only running/failed tasks can be abandoned",
                        t.status
                    );
                }
                vec![task_id]
            } else if all_stuck {
                state::stuck_task_ids(&rs, cfg.stuck_ttl_secs)
            } else {
                bail!("specify --task <id> or --all-stuck");
            };

            if ids.is_empty() {
                eprintln!("nothing to abandon");
            } else {
                for id in &ids {
                    let t = rs
                        .tasks
                        .iter_mut()
                        .find(|t| t.id == *id)
                        .ok_or_else(|| anyhow!("task '{id}' not found in run '{run}'"))?;
                    t.status = state::Status::Pending;
                    t.worktree = None;
                    t.branch = None;
                    t.updated_at = None;
                }
                rs.save(cfg, cwd)?;
                eprintln!("abandoned {} task(s): {}", ids.len(), ids.join(", "));
            }
        }
        StateAction::Pause { run } => {
            state::pause_run(cfg, cwd, &run)?;
        }
        StateAction::Resume { run } => {
            state::resume_run(cfg, cwd, &run)?;
        }
        StateAction::ListTasks => {
            let tasks = state::list_cancellable_tasks(cfg, cwd);
            println!("{}", serde_json::to_string_pretty(&tasks)?);
        }
        StateAction::Cancel { run, task } => {
            let mut rs = state::RunState::load(cfg, cwd, &run)?;
            let t = rs
                .tasks
                .iter_mut()
                .find(|t| t.id == task)
                .ok_or_else(|| anyhow!("no task '{task}' in run '{run}'"))?;
            if t.status == state::Status::Verified {
                bail!("task '{task}' is already verified and cannot be cancelled");
            }
            if t.status == state::Status::Cancelled {
                eprintln!("task '{task}' is already cancelled");
                return Ok(());
            }
            let was_running = t.status == state::Status::Running;
            let old_status = t.status;
            t.status = state::Status::Cancelled;
            t.worktree = None;
            t.branch = None;
            t.updated_at = Some(state::now_secs());
            rs.save(cfg, cwd)?;
            if was_running {
                eprintln!(
                    "cancelled task '{task}' (was running — in-flight worker may still \
                     complete; stop it manually if needed)"
                );
            } else {
                eprintln!("cancelled task '{task}' (was {:?})", old_status);
            }
            let (done, total) = rs.counts();
            eprintln!("run '{run}': {done}/{total} done");
        }
        StateAction::CheckCriteria { run, task } => {
            let rs = state::RunState::load(cfg, cwd, &run)?;
            let dec_raw = state::load_decomposition(cfg, cwd, &run)?;
            let dec: model::Decomposition = serde_json::from_str(&dec_raw)?;
            let t = dec
                .tasks
                .iter()
                .find(|t| t.id == task)
                .ok_or_else(|| anyhow!("no task '{task}' in run '{run}'"))?;
            let Some(dc) = &t.done_criteria else {
                // No criteria to check → nothing mechanical, verifier still runs.
                println!("{{\"mechanical\":false,\"behavioral\":false,\"skip_verifier\":false}}");
                return Ok(());
            };
            let cls = verify::classify_criteria(dc);
            let run_dir = rs
                .tasks
                .iter()
                .find(|s| s.id == task)
                .and_then(|s| s.worktree.as_deref())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| cwd.to_path_buf());

            // Behavioral (or non-runnable) criteria: the LLM verifier MUST run.
            // Any embedded command is run only to attach evidence — never to
            // skip the verifier, and never to fail this gate.
            if !cls.skip_eligible {
                let evidence = cls.mechanical_cmd.as_ref().map(|cmd| {
                    let (passed, output) = run_mechanical(cmd, &run_dir);
                    serde_json::json!({ "cmd": cmd, "passed": passed, "output": output })
                });
                let out = serde_json::json!({
                    "mechanical": cls.mechanical_cmd.is_some(),
                    "behavioral": cls.behavioral,
                    "skip_verifier": false,
                    "evidence": evidence,
                });
                println!("{}", serde_json::to_string(&out)?);
                return Ok(());
            }

            // Purely mechanical criteria: the verifier may be skipped iff the
            // command passes. A failing mechanical check fails this gate. If the
            // skip_eligible invariant is ever violated (no command), this fails
            // soft to skip_verifier:false rather than panicking an unattended run.
            let (out, gate_failed) =
                verify::mechanical_skip_verdict(&cls, |cmd| run_mechanical(cmd, &run_dir));
            println!("{}", serde_json::to_string(&out)?);
            if gate_failed {
                std::process::exit(1);
            }
        }
        StateAction::CheckOracle { run, task } => {
            let rs = state::RunState::load(cfg, cwd, &run)?;
            let dec_raw = state::load_decomposition(cfg, cwd, &run)?;
            let dec: model::Decomposition = serde_json::from_str(&dec_raw)?;
            let t = dec
                .tasks
                .iter()
                .find(|t| t.id == task)
                .ok_or_else(|| anyhow!("no task '{task}' in run '{run}'"))?;
            let run_dir = rs
                .tasks
                .iter()
                .find(|s| s.id == task)
                .and_then(|s| s.worktree.as_deref())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| cwd.to_path_buf());
            let out = oracle::check_oracle(
                t.requires_fp_oracle(),
                t.reproduction_tests.as_deref(),
                &task,
                &run_dir,
            );
            println!("{}", serde_json::to_string(&out)?);
        }
        StateAction::VerifierModel { worker, suggested } => {
            let chosen = verify::resolve_verifier_model(&worker, suggested.as_deref());
            // Invariant guard: never emit a verifier model equal to the worker.
            debug_assert!(!verify::same_model(&chosen, &worker));
            println!("{chosen}");
        }
        StateAction::AutonomyCheck => {
            // Shared predicate (see `policy_is_autonomous`): delegate to the
            // central policy engine instead of reading the raw bool. This
            // preserves the existing stdout bytes + exit contract exactly.
            let autonomous = policy_is_autonomous(cfg);
            println!("{{\"autonomous\":{autonomous}}}");
            if !autonomous {
                std::process::exit(1);
            }
        }
        StateAction::Checkpoint { run, label } => {
            let rs = state::RunState::load(cfg, cwd, &run)?;
            let dir = checkpoint_project_dir(cfg, cwd, &run);
            let shas = capture_branch_shas(cwd, &rs);
            let seq = checkpoint::write_checkpoint(
                &dir,
                &run,
                &rs,
                label.as_deref().unwrap_or(""),
                shas,
            )?;
            println!("{seq}");
            eprintln!("checkpoint {seq} recorded for run '{run}'");
        }
        StateAction::Rollback { run, to } => {
            let dir = checkpoint_project_dir(cfg, cwd, &run);
            let cp = match to {
                Some(seq) => checkpoint::checkpoint_at(&dir, &run, seq),
                None => checkpoint::latest_checkpoint(&dir, &run),
            };
            let Some(cp) = cp else {
                bail!("no checkpoint to roll back to for run '{run}'");
            };
            restore_checkpoint(cfg, cwd, &dir, &run, &cp, checkpoint::JournalKind::Rollback);
            let depth = checkpoint::load_journal(&dir, &run).len();
            println!("{}", cp.seq);
            eprintln!(
                "rolled run '{run}' back to checkpoint {} ({depth} journal events)",
                cp.seq
            );
        }
        StateAction::WorktreeModeCheck => {
            let single = cfg.single_worktree;
            println!("{{\"single_worktree\":{single}}}");
            if !single {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// The durable per-project state dir for a run's checkpoints/journal — the same
/// dir the run-state and decomposition live in (derived via the public
/// `decomposition_path` so we need no new state.rs API).
/// Shared autonomy predicate consumed by BOTH `state autonomy-check` and `gate
/// check`: delegate to the central policy engine rather than reading the raw
/// `cfg.autonomous` bool. The autonomous flag supplies confidence on a routine
/// (medium risk, medium reversibility) gate — an `Auto` verdict means
/// autonomous; anything else (Escalate) keeps the human in the loop. Factoring
/// it here guarantees the two subcommands stay in lockstep (non-autonomous mode
/// always yields `false`, so every gated task still escalates as today).
pub(crate) fn policy_is_autonomous(cfg: &Config) -> bool {
    let confidence = if cfg.autonomous {
        policy::Level::High
    } else {
        policy::Level::Low
    };
    policy::decide(policy::Level::Medium, policy::Level::Medium, confidence)
        == policy::Decision::Auto
}

fn checkpoint_project_dir(cfg: &Config, cwd: &Path, run_id: &str) -> PathBuf {
    state::decomposition_path(cfg, cwd, run_id)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.to_path_buf())
}

/// The owning Claude session id from the environment, if set — recorded on
/// claims so `state claims` can attribute a lease to a session.
fn session_id_from_env() -> Option<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty())
}

/// All `touched_files` across a run's decomposition (deduped). Fail-soft: an empty
/// vec when the decomposition is missing or unparseable, so claim/release degrade
/// to no-ops rather than erroring.
fn decomposition_files(cfg: &Config, cwd: &Path, run_id: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(raw) = state::load_decomposition(cfg, cwd, run_id) {
        if let Ok(dec) = serde_json::from_str::<model::Decomposition>(&raw) {
            for t in &dec.tasks {
                for f in &t.touched_files {
                    if !out.contains(f) {
                        out.push(f.clone());
                    }
                }
            }
        }
    }
    out
}

/// The `touched_files` of one task in a run's decomposition. Fail-soft: empty when
/// the decomposition or the task can't be found.
fn task_files(cfg: &Config, cwd: &Path, run_id: &str, task_id: &str) -> Vec<String> {
    if let Ok(raw) = state::load_decomposition(cfg, cwd, run_id) {
        if let Ok(dec) = serde_json::from_str::<model::Decomposition>(&raw) {
            if let Some(t) = dec.tasks.iter().find(|t| t.id == task_id) {
                return t.touched_files.clone();
            }
        }
    }
    Vec::new()
}

/// Snapshot each task's current branch tip SHA (best-effort): tasks with no
/// branch, or whose `git rev-parse` fails, are simply omitted — never aborts.
pub(crate) fn capture_branch_shas(cwd: &Path, rs: &state::RunState) -> BTreeMap<String, String> {
    let mut shas = BTreeMap::new();
    let repo = worktree::toplevel(cwd).ok();
    for t in &rs.tasks {
        if let (Some(repo), Some(branch)) = (repo.as_deref(), t.branch.as_deref()) {
            if let Ok(sha) = worktree::git(repo, &["rev-parse", branch]) {
                shas.insert(t.id.clone(), sha.trim().to_string());
            }
        }
    }
    shas
}

/// Restore a checkpoint's run-state snapshot as the current run-state, then
/// best-effort git-reset each recorded worktree to its snapshot SHA, and journal
/// the event. Every side effect is fail-soft: a git or journal error is logged
/// and skipped so a rollback (incl. the auto-rollback path) never breaks a turn.
fn restore_checkpoint(
    cfg: &Config,
    cwd: &Path,
    dir: &Path,
    run_id: &str,
    cp: &checkpoint::Checkpoint,
    kind: checkpoint::JournalKind,
) {
    // 1. Restore the tracking state (atomic save).
    if let Err(e) = cp.run.save(cfg, cwd) {
        eprintln!("rollback: failed to restore run-state for '{run_id}': {e:#}");
    }
    // 2. Best-effort restore each worktree to its snapshot SHA.
    for t in &cp.run.tasks {
        if let (Some(wt), Some(sha)) = (t.worktree.as_deref(), cp.branch_shas.get(&t.id)) {
            let wt_path = Path::new(wt);
            if wt_path.exists() {
                if let Err(e) = worktree::git(wt_path, &["reset", "--hard", sha]) {
                    eprintln!("rollback: git reset skipped for task '{}': {e:#}", t.id);
                }
            }
        }
    }
    // 3. Journal the event.
    checkpoint::append_journal(
        dir,
        run_id,
        &checkpoint::JournalEntry {
            seq: cp.seq,
            kind,
            label: cp.label.clone(),
            created_at: state::now_secs(),
            note: None,
        },
    );
}

/// Run a mechanical check command in `run_dir`, returning (passed, combined output).
/// A spawn failure counts as not-passed with the error text as output.
fn run_mechanical(cmd: &[String], run_dir: &Path) -> (bool, String) {
    match std::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(run_dir)
        .output()
    {
        Ok(out) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            (out.status.success(), combined)
        }
        Err(e) => (false, e.to_string()),
    }
}

/// Probe whether the `fugu-router` binary is on PATH and, if so, return its
/// skill fingerprint (stdout of `fugu-router fingerprint`, trimmed). `None`
/// means fugu-router is absent → recording is a soft no-op.
fn fugu_fingerprint() -> Option<String> {
    let out = std::process::Command::new("fugu-router")
        .arg("fingerprint")
        .output()
        .ok()?; // spawn failed (not on PATH) → soft-skip
    let fp = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some(fp)
}

/// Record completed runs' outcomes to fugu-router. Deterministic and idempotent:
/// only settled, not-yet-recorded runs emit episodes, and each is stamped with
/// `recorded_at` so repeated firings (the Stop hook) never double-record.
///
/// Soft dependency: when fugu-router is not on PATH this is a clean no-op and
/// does NOT mark runs recorded, so a later invocation (once fugu-router exists)
/// still captures the signal.
fn record_runs(cfg: &Config, cwd: &Path, run: Option<String>, all: bool) -> Result<()> {
    let run_ids: Vec<String> = match (run, all) {
        (Some(r), _) => vec![r],
        (None, true) => state::all_runs(cfg, cwd)
            .into_iter()
            .map(|rs| rs.run_id)
            .collect(),
        (None, false) => bail!("record-run requires --run <id> or --all"),
    };

    // Probe fugu-router once. Absent → soft no-op (leave runs unrecorded).
    let fingerprint = match fugu_fingerprint() {
        Some(fp) => fp,
        None => return Ok(()),
    };

    let mut emitted = 0usize;
    let mut recorded_runs = 0usize;
    for rid in run_ids {
        let mut rs = match state::RunState::load(cfg, cwd, &rid) {
            Ok(rs) => rs,
            Err(_) => continue, // run vanished between listing and load
        };
        // Need the decomposition to recover title/files/class/suggested_model.
        let dec = match state::load_decomposition(cfg, cwd, &rid) {
            Ok(raw) => match serde_json::from_str::<model::Decomposition>(&raw) {
                Ok(d) => d,
                Err(_) => continue, // corrupt sidecar → skip (leave unrecorded)
            },
            Err(_) => continue, // pre-feature run without a sidecar → skip
        };
        let Some(specs) = state::records_for_run(&rs, &dec) else {
            continue; // not settled, or already recorded
        };
        for s in &specs {
            let mut cmd = std::process::Command::new("fugu-router");
            cmd.arg("record")
                .args(["--title", &s.title])
                .args(["--files", &s.files.join(",")])
                .args(["--class", &s.class])
                .args(["--model", &s.model])
                .args(["--status", &s.status])
                .args(["--cost", &s.cost_usd.to_string()]);
            if let Some(dc) = &s.done_criteria {
                cmd.args(["--done-criteria", dc]);
            }
            if !fingerprint.is_empty() {
                cmd.args(["--skill-fingerprint", &fingerprint]);
            }
            // Best-effort: a single failed record must not abort the sweep.
            if let Err(e) = cmd.status() {
                eprintln!("condukt: fugu-router record failed for '{}': {e}", s.title);
            } else {
                emitted += 1;
            }
        }
        rs.recorded_at = Some(state::now_secs());
        rs.save(cfg, cwd)?;
        recorded_runs += 1;
    }

    if emitted > 0 {
        eprintln!(
            "condukt: recorded {emitted} outcome(s) from {recorded_runs} run(s) to fugu-router"
        );
    }
    Ok(())
}

fn init(cfg: &Config) -> Result<()> {
    let base = config::base_dir();
    // Loud-fail: a swallowed mkdir here (e.g. a read-only or non-writable parent)
    // used to surface much later as a cryptic I/O error when run-state or a
    // worktree was first written. Name the dir so the root cause is obvious.
    std::fs::create_dir_all(&cfg.state_dir)
        .with_context(|| format!("could not create state dir {}", cfg.state_dir.display()))?;
    std::fs::create_dir_all(&cfg.worktree_base).with_context(|| {
        format!(
            "could not create worktree base {}",
            cfg.worktree_base.display()
        )
    })?;
    let cfg_path = base.join("config.toml");
    if cfg_path.exists() {
        eprintln!("config already exists: {}", cfg_path.display());
        return Ok(());
    }
    let default = format!(
        "# condukt configuration\n\
         # Where worktrees are created (MUST be outside any repo).\n\
         worktree_base = \"{}\"\n\
         # Branch completed work merges back into.\n\
         default_branch = \"{}\"\n\
         # Soft cap on concurrent workers (advisory; the skill honors it).\n\
         max_parallel = {}\n\
         # Globs that force a touching task to run serially (never in parallel),\n\
         # e.g. [\"**/models.py\", \"**/migrations/**\", \"docs/glossary.md\"].\n\
         shared_globs = []\n\
         \n\
         # Command `condukt state test` runs (via `sh -c`, from the repo root).\n\
         # Omit to auto-detect (cargo test / npm test / pytest).\n\
         # [test]\n\
         # command = \"cargo test\"\n\
         \n\
         # Loop feature: commands for `condukt loop` test-fix cycles.\n\
         # Use with /condukt-loop skill to run test→fix→test until all tests pass.\n\
         #   server cycle: deploy → test\n\
         #   client cycle: build  → test\n\
         #   e2e    cycle: build  → deploy → test\n\
         # [loop]\n\
         # build_command  = \"npm run build\"\n\
         # deploy_command = \"kubectl rollout restart deployment/api && kubectl rollout status deployment/api\"\n\
         # max_iters      = 10\n\
         \n\
         # Multi-sample self-consistency (COST GUARD: opt-in, OFF by default).\n\
         # When enabled, a high-risk task is implemented N independent times,\n\
         # each verified, and a majority vote picks the winner; low agreement\n\
         # escalates the task to opus. N-sample generation is N x the cost, so\n\
         # keep this off unless you want fan-out consensus on risky tasks. A\n\
         # per-task `condukt consensus plan --risk high` forces fan-out even when\n\
         # enabled=false. samples is clamped to a ceiling of 5.\n\
         # [consensus]\n\
         # enabled   = false\n\
         # samples   = 3\n\
         # threshold = 0.5\n",
        cfg.worktree_base.display(),
        cfg.default_branch,
        cfg.max_parallel
    );
    std::fs::write(&cfg_path, default)
        .with_context(|| format!("writing {}", cfg_path.display()))?;
    eprintln!("wrote {}", cfg_path.display());
    Ok(())
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.char_indices();
    match chars.nth(max_chars) {
        Some((idx, _)) => format!("{}…", &s[..idx]),
        None => s.to_string(),
    }
}

fn default_run_id() -> String {
    chrono::Local::now().format("run-%Y%m%d-%H%M%S").to_string()
}

/// Auto-detect a terminal label: try `tty` output first, fall back to `pid-<PID>`.
fn resolve_label() -> Option<String> {
    let tty = std::process::Command::new("tty").output().ok()?;
    if tty.status.success() {
        let name = String::from_utf8_lossy(&tty.stdout).trim().to_string();
        if !name.is_empty() && name != "not a tty" {
            return Some(name);
        }
    }
    Some(format!("pid-{}", std::process::id()))
}

fn read_decomposition(file: Option<PathBuf>) -> Result<model::Decomposition> {
    let raw = match file {
        Some(p) => {
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?
        }
        None => read_stdin(),
    };
    serde_json::from_str(&raw).context("parsing decomposition JSON")
}

fn read_stdin() -> String {
    use std::io::Read;
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    s
}

#[cfg(test)]
mod calibrated_confidence_tests {
    use super::calibrated_confidence;

    /// The legacy invocation supplies none of the new flags: the calibrated
    /// path must be entirely inert (return `None`, no shell-out) so `decide`
    /// keeps using the self-reported `--confidence`. This pins the hard
    /// backward-compat gate without depending on fugu-router being present.
    #[test]
    fn no_new_flags_is_inert_and_falls_back() {
        assert_eq!(calibrated_confidence(&None, &[], &None), None);
    }

    /// With a flag supplied but fugu-router absent (or its `confidence`
    /// subcommand missing), the probe must fail-soft to `None` — never a hard
    /// error — so the caller falls back to `--confidence`.
    ///
    /// This is made HERMETIC by forcing `PATH` to exclude any directory that
    /// actually contains a `fugu-router` executable, so the assertion holds
    /// regardless of ambient machine state (e.g. a dev box where fugu-router
    /// IS on PATH and its episode store has history would otherwise return
    /// `Some(..)`, per `calibrated_confidence`'s shell-out above). Every
    /// other directory (git, gh, tty, ...) stays resolvable, and the
    /// original `PATH` is restored on drop (incl. on panic, since `Drop`
    /// still runs during unwind) so no other concurrently-running test in
    /// this binary observes the mutation.
    #[test]
    fn flag_supplied_but_probe_unusable_falls_back() {
        /// RAII guard: temporarily overrides the process `PATH` env var,
        /// restoring the original value on drop.
        struct PathGuard {
            original: Option<String>,
        }
        impl PathGuard {
            fn install(filtered: &str) -> Self {
                let original = std::env::var("PATH").ok();
                // SAFETY: test-only, single-purpose mutation; always
                // restored by `Drop::drop` below (incl. on panic/unwind).
                unsafe { std::env::set_var("PATH", filtered) };
                Self { original }
            }
        }
        impl Drop for PathGuard {
            fn drop(&mut self) {
                match &self.original {
                    // SAFETY: see `install` above — this only ever restores
                    // the exact value that was captured before mutation.
                    Some(v) => unsafe { std::env::set_var("PATH", v) },
                    None => unsafe { std::env::remove_var("PATH") },
                }
            }
        }

        let original_path = std::env::var("PATH").unwrap_or_default();
        let sep = if cfg!(windows) { ';' } else { ':' };
        let filtered: Vec<&str> = original_path
            .split(sep)
            .filter(|dir| !std::path::Path::new(dir).join("fugu-router").exists())
            .collect();
        let sep_str = sep.to_string();
        let filtered_path = filtered.join(sep_str.as_str());

        let _guard = PathGuard::install(&filtered_path);

        let title = Some("some task".to_string());
        // With fugu-router unresolvable on PATH, `Command::output()` fails
        // to spawn → soft-skip → `None`, regardless of any ambient
        // fugu-router install / episode history on the host.
        assert_eq!(calibrated_confidence(&title, &[], &None), None);
    }
}

#[cfg(test)]
mod state_set_tests {
    use crate::state::{RunState, Status, TaskState};

    fn make_run(worktree: Option<&str>, branch: Option<&str>) -> RunState {
        RunState {
            run_id: "r1".into(),
            goal: "test".into(),
            tasks: vec![TaskState {
                id: "t1".into(),
                status: Status::Pending,
                worktree: worktree.map(str::to_string),
                branch: branch.map(str::to_string),
                branch_sha: None,
                updated_at: None,
                model: None,
                cost_usd: None,
                fp_oracle_valid: None,
            }],
            paused: false,
            terminal_label: None,
            recorded_at: None,
        }
    }

    /// worktree/branch が Some のタスクに worktree/branch なしで Set を実行しても
    /// 既存の worktree/branch が消えないこと (None 上書き保護)。
    #[test]
    fn state_set_none_does_not_overwrite_worktree_or_branch() {
        let mut rs = make_run(Some("/path/to/tree"), Some("feature/x"));
        let t = rs.tasks.iter_mut().find(|t| t.id == "t1").unwrap();

        // worktree/branch を None のまま (省略相当) でステータスだけ更新する。
        let new_worktree: Option<String> = None;
        let new_branch: Option<String> = None;
        let clear_worktree = false;
        let clear_branch = false;

        t.status = Status::Running;
        t.updated_at = Some(crate::state::now_secs());
        if clear_worktree {
            t.worktree = None;
        } else if new_worktree.is_some() {
            t.worktree = new_worktree;
        }
        if clear_branch {
            t.branch = None;
        } else if new_branch.is_some() {
            t.branch = new_branch;
        }

        assert_eq!(
            t.worktree.as_deref(),
            Some("/path/to/tree"),
            "worktree must be preserved"
        );
        assert_eq!(
            t.branch.as_deref(),
            Some("feature/x"),
            "branch must be preserved"
        );
        assert_eq!(t.status, Status::Running);
        assert!(t.updated_at.is_some());
    }

    /// --clear-worktree / --clear-branch フラグで既存値を明示的に消去できること。
    #[test]
    fn state_set_clear_flags_erase_worktree_and_branch() {
        let mut rs = make_run(Some("/path/to/tree"), Some("feature/x"));
        let t = rs.tasks.iter_mut().find(|t| t.id == "t1").unwrap();

        let clear_worktree = true;
        let clear_branch = true;
        let new_worktree: Option<String> = None;
        let new_branch: Option<String> = None;

        if clear_worktree {
            t.worktree = None;
        } else if new_worktree.is_some() {
            t.worktree = new_worktree;
        }
        if clear_branch {
            t.branch = None;
        } else if new_branch.is_some() {
            t.branch = new_branch;
        }

        assert!(t.worktree.is_none(), "worktree must be cleared");
        assert!(t.branch.is_none(), "branch must be cleared");
    }
}
