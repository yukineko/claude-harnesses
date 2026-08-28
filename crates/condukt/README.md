# condukt

A **deterministic orchestration engine** for Claude Code.

Large tasks decompose into many small ones. The judgement — interpret the
request, implement each piece, verify it — is LLM work. But deciding *which
tasks can run in parallel*, *managing git worktrees*, *tracking run state*, and
*knowing when you are actually done* should not be eyeballed by a language model.
condukt splits the two:

```
LLM  (the /condukt skill + interpreter/researcher/worker/verifier agents)
  ├ interpret the request        ─┐
  ├ decompose into tasks (JSON)   │   condukt binary (deterministic)
  ├ implement each task           ├──▶ schedule:  conflict analysis → parallel/serial batches
  └ verify against criteria       │    worktree:  create / merge / remove / cleanup
                                  ─┘    state:     run tracking + completion gate
```

The binary is a single Rust executable exposing one subcommand per job. It is
**subscription-native**: no `ANTHROPIC_API_KEY`, no separate install for plugin
users — the work runs inside Claude Code via a skill, four agents, a SessionStart
hook (`restore`) and a Stop hook (`state record-run --all`).

## What the engine does

| subcommand | purpose |
|---|---|
| `condukt schedule` | read a decomposition JSON, output ordered parallel batches + serial/gated lists. Two tasks share a batch only if their `touched_files` don't conflict and neither depends on the other. |
| `condukt validate` | check a decomposition JSON (unique ids, known deps, no cycles). |
| `condukt worktree create/merge/remove/cleanup/list` | git-worktree lifecycle; enforces "path outside the repo" and "one dir = one branch". |
| `condukt state init/set/show/gate/list` | persist a run's task statuses; `set` accepts `--model`/`--cost` so a recorded outcome reflects the actual (escalated) model and gauge cost; `gate` exits non-zero until every task is verified and no worktree is left dirty or unremoved. `set --status verified` also enforces the F→P reproduction gate (below): it refuses to promote a `fix`/`feature` task that lacks a valid Fail→Pass oracle. |
| `condukt state check-oracle --run <id> --task <id>` | ask whether a `fix`/`feature` task carries a valid Fail→Pass reproduction proof. When the task is in scope (`kind` is `fix`/`feature`) and has `reproduction_tests`, it runs `tdd oracle --task <id>` inside the task's worktree and prints `{"required","valid_fp_oracle","fallback","transition","reason"}`. Fail-soft: when `tdd` is absent/unreachable or the verdict is unreadable it returns `fallback:true` (degrade to the legacy gate) and never panics or exits non-zero. |
| `condukt state check-criteria --run <id> --task <id>` | run the mechanical gate for a task's `done_criteria`: exits 0 when met (or non-mechanical), 1 when it fails. The JSON `skip_verifier` field is true ONLY for purely mechanical criteria that pass, so the skill can skip the LLM verifier for them; behavioral criteria always report `skip_verifier:false`. |
| `condukt state conflict-check/abandon/list-tasks/cancel/pause/resume` | cross-session safety + run editing: detect file/goal conflicts before `init`, return stuck `running` tasks to `pending` (`--all-stuck`), list/cancel a run's tasks, pause/resume a conflicting run (see the skill's Phase 0/3.5 and the cancel utility). |
| `condukt state claim/release/heartbeat/claims` | **cross-session file-claim registry** (`<state>/<project>/claims.json`) that turns `conflict-check`'s one-time advisory snapshot into a live, enforcing lease so two sessions on the same machine never process the same work. `claim --run <id> [--file ...]` leases a task's `touched_files` (defaults to the run's whole decomposition); it **hard-skips** (exits 1, prints the conflicting holders) any file a *live* holder from another run owns. `release` frees them (all, or `--file`), `heartbeat` refreshes liveness so a busy session isn't reaped, `claims` prints the live registry. Enforcement is automatic: `state set --status running` auto-claims the task's files and **exits 1 with a skip JSON** on a live conflict; terminal transitions auto-release. Stale claims (heartbeat older than the stuck-TTL) are reaped — liveness is anchored to the heartbeat, not the ephemeral CLI pid; every op is fail-soft and serialized by the per-run lock. |
| `condukt state autonomy-check` | report whether condukt is in autonomous mode (config `autonomous` + `CONDUKT_AUTONOMOUS` env): prints `{"autonomous":<bool>}` and exits 0 when autonomous, 1 when not, so the skill can deterministically degrade human gates (e.g. the Phase 3 agreement) only when autonomous. Off by default (every `AskUserQuestion` still fires — backward compatible). |
| `condukt state worktree-mode-check` | report whether condukt is in single-worktree mode (config `single_worktree` + `CONDUKT_SINGLE_WORKTREE` env): prints `{"single_worktree":<bool>}`, exits 0 when on / 1 when off, so the skill can run all tasks in the main tree (selective staging, no per-task worktree/merge) only when enabled. |
| `condukt state checkpoint/rollback --run <id>` | the reversibility safety net for autonomous proceeding (charter #7). `checkpoint` durably snapshots a run's state + each task's branch SHA and journals the event, printing the new seq; `rollback` restores a snapshotted state and best-effort `git reset`s each worktree to its recorded SHA (`--to <seq>` picks a checkpoint; default: latest). |
| `condukt state verifier-model --worker <model> [--suggested <model>]` | resolve the verifier model so it never equals the worker model (shared blind-spot guard). Honours a distinct `--suggested`; otherwise picks a distinct tier. Prints the chosen model. |
| `condukt state record-run --run <id> \| --all` | deterministically record settled tasks to fugu-router (fired by the Stop hook; idempotent via per-run `recorded_at`; soft no-op when fugu-router is absent). |
| `condukt learning-signal` | compute `mean_replan_reduction_ratio = 1 - (mean_hit / mean_miss)` by joining the replan-log's `replan_count` × the retrieval ledger's `hit` flag per `run_id` — the deterministic measurement surface for cross-task learning (fail-soft, `ratio` is `null` when a group is empty or `mean_miss == 0`). |
| `condukt knowledge` | emit project-specific conventions/pitfalls injected into the interpreter/worker prompt (soft; empty when none). |
| `condukt consensus plan/vote` | multi-sample self-consistency (opt-in cost guard). `plan` decides whether a task should fan out into N candidate implementations (exit 0 = fan out, 1 = single sample); `vote` tallies N verifier verdicts into a deterministic majority winner + agreement rate, escalating to opus on all-fail, a tie, or agreement below threshold. |
| `condukt adversarial plan/adjudicate` | adversarial refutation panel (verification-side fan-out; opt-in cost guard, OFF by default). `plan --touched <path>...` decides whether a completed artifact warrants N independent skeptics (exit 0 = panel warranted, 1 = ordinary single-verifier path); engaged by `CONDUKT_ADVERSARIAL`/`[adversarial] enabled`, OR forced regardless of the switch when any `--touched` path is under a GATE crate (blastguard/propguard/specguard/stuckguard/mutategate). `adjudicate` takes N skeptic ballots (`refute\|pass\|abstain`, JSON on stdin or `--file`) for the same artifact and fails closed: exits 0 when it passes, 1 when the caller must not auto-accept (refute ratio at/above `block_ratio`, or fewer than `min_voters` effective ballots). |
| `condukt policy decide/answer/answers` | central **graded-autonomy policy**: map a decision's risk × reversibility × confidence to `auto`/`escalate`/`block` via an exit-code contract (0=auto, 2=escalate, 3=block, 1=invalid). `answer` non-interactively resolves one question on an `auto` verdict (journaling the choice) and otherwise falls through so the caller runs a real `AskUserQuestion`; `answers` prints the auto-answer audit trail (every question self-answered without a human). `decide`/`answer` also accept optional `--title/--files/--class`: when given and `fugu-router` is on PATH, the self-reported `--confidence` is overridden by a calibrated `[0,1]` score from `fugu-router confidence` (historical pass-rate), mapped to a band via the pure `Level::from_score` (thresholds 0.34/0.67); absent fugu-router or the new flags, it falls back byte/exit-identically to the self-reported value. |
| `condukt verify digest/runtime/launch/regressions/confidence/checks` | deterministic verifier-stage helpers (formatting only; the fix DECISION stays with the LLM worker). `digest` distills raw test output into a structured `FailureDigest`; `runtime` distills a target's runtime output (exit code, panic/exception lines, stderr/stdout tails), `--reflux` for the pass/fail verdict; `launch` runs a real target inside the blastguard-validated envelope (`--cmd` refused fail-closed if destructive) and refluxes its runtime signals — with `--health-url` it polls a server for HTTP 200 instead of waiting for exit. `regressions --baseline <f> --current <f>` diffs two failing-test sets (pure set-difference — `current - baseline`) so the verifier's regression call is deterministic, not eyeballed. `confidence --check-executed --exit-zero --no-regressions` derives `high|medium|low` from those observed facts instead of an LLM self-report. `checks --file <task.json> [--cwd <dir>]` runs a task's declared `checks[]` (see the schema below) as a machine oracle and prints `{"verdict":"passed"|"failed"|"no_checks_declared","all_passed":bool,"results":[...]}`. A document declaring no checks (key absent, misspelled, or an empty array) reports `no_checks_declared` with `all_passed:false` — never a vacuous pass, since nothing ran. All fail-soft (exit 0). |
| `condukt replan handoff/stats` | deterministic reflux-cascade helpers (classification/formatting only; the re-decomposition DECISION stays with the LLM). `handoff` classifies a failing task's reflux facts into `escalate_model` vs `replan` and, only on `replan`, builds a handoff instructing the interpreter to produce a NEW decomposition; `--run <id>` also journals the decision. `stats --run <id>` aggregates that log into per-directive counts. |
| `condukt circuit check --run <id> [--streak-cap N] [--idle-ttl-secs S] [--budget-cap-usd C]` | deterministic CIRCUIT-BREAKER stop-condition gate: gathers a run's consecutive-failure streak, idle/stall, and (optional) budget-over-cap signals — all fail-soft — runs the pure `decide_circuit` core, prints the verdict + signals as JSON, journals it, and exits non-zero when the breaker trips, so a loop can do `if ! condukt circuit check --run RID; then stop; fi`. |
| `condukt gate check --run <id> --task <id>` | deterministic GATE-EXEC decision for a `gated` task: classifies its action text (risk × reversibility) and reads the autonomy policy — all fail-soft — runs `decide_gate_exec`, prints the verdict + signals as JSON, checkpoints the run before auto-executing (so the action is recoverable), and exits non-zero on escalate, so a caller can do `if ! condukt gate check --run RID --task T; then escalate; fi`. |
| `condukt escalate add/list/resolve` | durable async escalation channel (`<state_dir>/<project>/escalations.json`, atomic + fail-soft): `add --run --task --question --option <o> [--recommend N]` enqueues an out-of-band question and prints its `id`; `list --run [--json]` shows the still-open escalations for a run; `resolve --id --choice` records the chosen answer so a blocked/gated task can resume instead of stalling on an inline `AskUserQuestion`. |
| `condukt pr create --title <t> [--execute]` | terminal external-loop step: open a PR via the `gh` CLI. Without `--execute` it dry-runs, printing the exact argv that WOULD run; the `/condukt` skill passes `--execute` ONLY after the human GATED approval, so autonomous runs never open a PR on their own. Uses gh's own auth (no API key); gh absent/unauthenticated degrades to local-commit-only and exits 0 (fail-soft). |
| `condukt shadow-run enable\|disable\|status` | manually toggle the opt-in **shadow-run** mode (default: disabled). `status` exits 0 when enabled, 1 when disabled (mirrors `state autonomy-check`'s exit-code contract). Always a human decision — there is no API exposing remaining rate-limit-window time, so no automatic trigger is implemented; pair with `gauge config set-window`/`gauge config show` if you want a manual approximation of the window to decide *when* to flip this on. |
| `condukt shadow-run exec --topic <t> --branch <b> --model <m>` | gated on the enable flag (refuses with a non-zero exit while disabled, before creating anything): creates a second worktree via the existing `worktree create` machinery and prints its path. The caller (the `/condukt` skill, via a worker agent) runs the SAME task under `--model` inside it, purely to produce a clean side-by-side data point. |
| `condukt shadow-run finish --path <p> --branch <b> --title <t> --model <m> [--pass] --cost <c> --duration <d>` | discards the shadow worktree (force-remove the dir + force-delete its branch — the shadow attempt is **never merged**, whatever the primary worker produced is what ships) and best-effort records the pass/fail/cost/duration outcome to `fugu-router record --class shadow-run` for later routing comparison. Soft dependency: succeeds even when `fugu-router` is absent or older than this flag set. |
| `condukt state stats` | aggregate all runs (complete and incomplete): completion rate, task count, status distribution — useful as a before/after benchmark. |
| `condukt state reconcile --run <id> [--dry-run]` | auto-promote tasks to `verified` when their branch is already merged into the default branch or has been deleted with its worktree. Fixes stale state after a session crash without manual `state set` calls. **Cross-run duplicate guard:** before that auto-promotion, it scans sibling runs for any hashkey this run completed (`done`/`verified`) that another `run_id` *also* completed *after* this run's `claimed_at`; on a hit it mutates nothing, prints `{"duplicate_completion":[{hashkey,runs:[run_id...]}]}`, and **exits 2** (escalate → human/HOTL picks which implementation to keep — per condukt's 0=auto / 2=escalate / 3=block convention). The no-duplicate path is unchanged (auto-verify, exit 0). |
| `condukt state timings --run <id> [--json]` | per-task **phase timing** breakdown from the `TaskState` phase timestamps (`worker_started_at`/`worker_ended_at`/`verifier_started_at`/`verifier_ended_at`/`merge_completed_at`), each stamped by the real orchestrator transition it already performs — `state set --status running` → worker start, the first `--status done` → worker end + verifier start, `--status verified`/`failed` → verifier end, and a successful `worktree merge --run --task` → merge complete (set-once; measurement only — never consulted by scheduling/routing). An **unmeasured** phase is rendered as an explicit `unmeasured` marker (text) / `null` (JSON), NEVER as `0` seconds, so a never-measured phase stays distinguishable from one that completed instantly — the same tri-state norm as fugu-router's `duration_secs`. Backward-compatible: run-state JSON written before these fields existed still loads (`Option` + serde-default/skip). |
| `condukt state resume-context --run <id>` | emit pending/failed/done tasks as JSON for resuming a stopped run across sessions (see Phase 0-alt in the skill). |
| `condukt state test` | run the project's test suite from the repo root (auto-detects `cargo test` / `npm test` / `pytest`, or uses `[test].command` from config). |
| `condukt editgate` | PostToolUse hook: after a worker's Edit/Write to a Rust file inside a live worktree, the **edit-time compile gate** deterministically decides whether the edit left the crate broken; on a real broken verdict it prints `{"decision":"block","reason":<diagnostics>}` so the worker fixes it in the same turn. Fail-soft everywhere else (prints nothing, exits 0). |
| `condukt restore` | SessionStart hook: reminds you of unfinished runs / orphan worktrees. |
| `condukt statusline` | one-line run progress for the `statusLine` setting. |
| `condukt status [--all]` | show open runs and their tasks as an ASCII tree (`--all` includes closed runs). |
| `condukt init / install / uninstall` | create `~/.condukt`; manual hook wiring (plugin users don't need these). |

The decomposition schema (what the interpreter agent emits / `schedule` consumes).
Canonical definition: `agents/condukt-interpreter.md`.

```json
{ "goal": "...", "linked_hypotheses": ["hid1"],
  "tasks": [
  { "id": "t1", "title": "...", "touched_files": ["path/or/glob"],
    "deps": ["t0"], "class": "parallel|serial|gated", "kind": "fix|feature|chore",
    "suggested_model": "sonnet|opus|haiku", "done_criteria": "observable pass condition",
    "checks": [{ "cmd": "cargo test -p x", "expect_exit": 0, "expect_substring": "ok" }],
    "expected_trajectory": { "mode": "strict|unordered|subsequence", "steps": [{ "tool": "Read" }] } }
]}
```

`checks` and `expected_trajectory` are both optional and backward-compatible
(`#[serde(default)]`; a task with neither behaves exactly as before). `checks[]`
declares deterministic machine-oracle commands the verifier stage can run
directly (`condukt verify checks --file <task.json>`) instead of an LLM judging
pass/fail for that command. `expected_trajectory` declares the tool-call order a
worker is expected to follow; when present, Phase 6 of the `/condukt` skill feeds
the worker's transcript through the soft-dependency `trajectoryeval extract`/`check`
pair to verify the *path* alongside the existing output-only `done_criteria`
check — a second, independent verification dimension. Absent `expected_trajectory`
or an absent `trajectoryeval` binary skip this step entirely (fail-soft, no-op).

`kind` is optional and backward-compatible (`#[serde(default)]`). Only `fix` and
`feature` (case-insensitive) are in scope for the **F→P reproduction gate**: such a
task must ship a task-specific test that fails on the buggy tree and passes on the
fixed tree (a Fail→Pass transition). `condukt state check-oracle` classifies the
worker's `tdd` red/green proofs, and `state set --status verified` refuses the
promotion unless the transition is a valid Fail→Pass — so "done" means *the
reproduction actually flipped from red to green*, not just that the criteria text
matched. The whole path is fail-soft: with `tdd` absent, no `reproduction_tests`,
or a non-`fix`/`feature` task, the gate degrades to the legacy done-criteria check.

**Cross-task lessons lifecycle.** A lesson is written when `stuckguard` escalates
(a recurring stuck pattern crosses its threshold); `condukt replan handoff`
retrieves the single best-matching past lesson via a deterministic lexical
search and, only above a match-score floor, injects it into the replan handoff
wrapped in an `--- UNTRUSTED PRIOR-LESSON ---` boundary marker (`replan.rs`) —
reference material, not an instruction, and never able to override
`done_criteria`/scope. `condukt learning-signal` (above) is the read-only
measurement layer over that same lessons flow.

## Install

### Plugin (recommended)

> The marketplace catalog lives in a separate central repo. Once condukt is
> published there, install is:

```
/plugin marketplace add <git-url-of-the-catalog-repo>
/plugin install condukt@yukineko
```

This bundles the `/condukt` skill, the four agents, the SessionStart + Stop hooks,
and a prebuilt binary. Optionally run `condukt init` once to create `~/.condukt` and a
default `config.toml`.

### Manual (build from source)

```
cargo build --release
cp target/release/condukt ~/.cargo/bin/      # or anywhere on PATH
condukt init
condukt install --dry-run                    # preview settings.json changes
condukt install                              # merge the SessionStart hook (backs up settings.json)
cp -r skills/condukt ~/.claude/skills/        # and agents/ -> ~/.claude/agents/
```

Remove with `condukt uninstall`.

## Configuration

`~/.condukt/config.toml` (defaults shown):

```toml
worktree_base  = "~/.condukt/worktrees"  # MUST be outside any repo
default_branch = "main"
max_parallel   = 4                        # advisory soft cap on concurrent workers
shared_globs   = []                       # globs that force a touching task to run serially
autonomous     = false                    # when true, degrade human gates (Phase 3 agreement) to deterministic defaults
single_worktree = false                   # when true, run all tasks in the main tree (selective staging, no per-task worktree/merge)

# Command `condukt state test` runs (via `sh -c`, from the repo root).
# Omit to auto-detect (cargo test / npm test / pytest).
# [test]
# command = "cargo test"

# Multi-sample self-consistency (OPT-IN cost guard; OFF by default). When
# enabled, a high-risk task is implemented N times, verified, and a majority
# vote picks the winner; low agreement escalates to opus. N-sample generation
# is N x the cost. A per-task `consensus plan --risk high` forces fan-out even
# when enabled = false. samples is clamped to a ceiling of 5.
# [consensus]
# enabled   = false
# samples   = 3
# threshold = 0.5

# Adversarial refutation panel (verification-side fan-out; OPT-IN, OFF by
# default). When enabled, a completed high-stakes artifact is refuted by N
# independent skeptics instead of a single verifier; a refute ratio at/above
# block_ratio fails the artifact closed. A GATE-crate-touching change forces
# the panel even when enabled = false. min_voters is the effective-voter floor
# below which the panel fails closed (too few ballots to trust a verdict).
# [adversarial]
# enabled     = false
# size        = 3
# min_voters  = 2
# block_ratio = 0.5

# Opt-in worker sandboxing (OFF by default). When enabled, a worker's build/test
# command run via `condukt sandbox run` executes inside the docker exec backend
# (`docker run --rm --network=none`, the CWD bind-mounted read-write at the same
# path) instead of directly on the host — giving network + filesystem isolation
# plus optional resource limits. Docker-absent degrades to a fail-soft
# `docker_unavailable` verdict (never a host fallback). Edits still happen on the
# host worktree; only build/test EXECUTION is sandboxed.
# [worker]
# sandbox_enabled = false
# docker_image    = "alpine:latest"
# memory_limit    = "512m"   # docker --memory  (omit = no cap)
# cpus            = "1.5"    # docker --cpus    (omit = no cap)
# pids_limit      = 256      # docker --pids-limit (omit = no cap)
```

`shared_globs` is how you keep workers off project-wide files without hardcoding
anything — e.g. `["**/models.py", "**/migrations/**", "docs/glossary.md"]`. Any
parallel task touching one is demoted to serial with a warning.

### Environment variables

All config file keys can be overridden at runtime with environment variables.
`CONDUKT_DISABLE` is a hook-only kill switch and has no config file equivalent.

| Variable | Default | Description |
|---|---|---|
| `CONDUKT_WORKTREE_BASE` | `~/.condukt/worktrees` | Directory where worktrees are created (must be outside any repo). |
| `CONDUKT_DEFAULT_BRANCH` | `main` | Branch completed work is merged back into. |
| `CONDUKT_MAX_PARALLEL` | `4` | Advisory soft cap on concurrent workers. |
| `CONDUKT_DISABLE` | _(unset)_ | Set to `1` to make the SessionStart/statusline hooks no-op (useful in CI). |
| `CONDUKT_CONSENSUS` | `false` | Set to `1`/`true` to enable multi-sample self-consistency fan-out (overrides `[consensus] enabled`). Opt-in cost guard; off by default. |
| `CONDUKT_ADVERSARIAL` | `false` | Set to `1`/`true` to enable the adversarial refutation panel (overrides `[adversarial] enabled`). Opt-in cost guard; off by default. A GATE-crate-touching change forces the panel regardless of this switch. |
| `CONDUKT_AUTONOMOUS` | `false` | Set to `1`/`true` to run autonomously (degrades human gates; overrides config `autonomous`). Read by `state autonomy-check`. |
| `CONDUKT_SINGLE_WORKTREE` | `false` | Set to `1`/`true` to run all tasks in the main tree (no per-task worktree/merge; overrides config `single_worktree`). Read by `state worktree-mode-check`. |
| `CONDUKT_STUCK_TTL_SECS` | `1800` | Age (seconds) past which a `running` task becomes a CANDIDATE for `state abandon --all-stuck`. TTL-staleness is necessary, not sufficient: the bulk path abandons only a task whose progress is a confirmed `Known(Stalled)` (backlog `356bd51d`). |
| `CONDUKT_WORKER_SANDBOX` | `false` | Set to `1`/`true` to run a worker's build/test through the sandboxed docker exec backend (overrides `[worker] sandbox_enabled`). Read by `sandbox run`. |
| `CONDUKT_WORKER_SANDBOX_IMAGE` | _(unset)_ | Override the container image for sandboxed worker execution (overrides `[worker] docker_image`). |
| `CONDUKT_SHADOW_RUN_DIR` | `~/.condukt` | Directory holding the shadow-run enable flag (`shadow_run.json`). Override in tests so they never touch the real `~/.condukt`. |

### `condukt loop` — test-fix cycle

Runs one iteration of a test-fix cycle for a given module type and prints a JSON
result. The `/condukt-loop` skill calls this repeatedly, inserting a fix step
between iterations, until all tests pass or no progress is detected.

```
condukt loop --module <server|client|e2e> [--iteration N] [--prev-failures N]
```

**Cycle sequences** (configured via `[loop]` in `config.toml`):

| `--module` | Steps |
|---|---|
| `server` | deploy → test |
| `client` | build → test |
| `e2e` | build → deploy → test |

**JSON output** (one object per invocation):

```json
{
  "iteration": 1,
  "module": "client",
  "failure_count": 3,
  "success": false,
  "stop": false,
  "stop_reason": "",
  "output": "<combined stdout+stderr>"
}
```

`stop=true` when `failure_count == 0` (`stop_reason: "all tests pass"`) or when
`failure_count == prev_failures` (`stop_reason: "no progress: failure count unchanged"`).

**Config:**

```toml
[loop]
build_command  = "npm run build"
deploy_command = "kubectl rollout restart deployment/api && kubectl rollout status deployment/api"
max_iters      = 10   # safety cap; the skill enforces it
```

### `condukt state test`

Runs the project's test suite from the repo root and propagates its exit code.

```
condukt state test --run <run-id>
```

The command source is resolved in this priority order:

1. `[test].command` in `~/.condukt/config.toml`
2. Auto-detected from the repo root: `cargo test` (Cargo.toml), `npm test` (package.json), `pytest` (pyproject.toml / setup.py), falling back to `cargo test`.

The command is executed via `sh -c`, so quoted arguments, pipes, and env-var
expansions all work as expected — e.g. `command = "pytest -k 'unit or smoke'"`.
Running from the repo root (not the cwd of the caller) means auto-detection always
sees the project manifest even when the caller is in a subdirectory.

### `condukt shadow-run` — manual dual-model speculative execution

Runs the SAME task under a second model in an independent worktree purely to
produce a clean pass/fail/cost/duration comparison point for `fugu-router` — a
manual A/B data point, not a routing change. **Manual-trigger only, by
design**: there is no API or hook input exposing how much of the account's
rate-limit window remains, so an automatic "fire when there's spare capacity"
trigger isn't feasible. Deciding *when* to run one is always a human call
(optionally informed by `gauge config set-window`/`gauge config show`'s
manually-registered window approximation); deciding *whether it's allowed at
all* is the `enable`/`disable` flag below.

```
condukt shadow-run enable                 # permit shadow-run to fire (off by default)
condukt shadow-run status                 # exit 0/1 = enabled/disabled
condukt shadow-run exec --topic t1-shadow --branch shadow/t1-opus --model opus
# -> prints the new worktree's path; refuses (non-zero exit) while disabled,
#    before creating anything
# ... the /condukt skill runs a worker under --model inside that worktree ...
condukt shadow-run finish --path <path> --branch shadow/t1-opus \
  --title "t1 shadow attempt" --model opus --pass --cost 0.42 --duration 12.5
# -> discards the worktree + branch (never merged) and best-effort records
#    the outcome via `fugu-router record --class shadow-run`
condukt shadow-run disable
```

The shadow worktree is always discarded via the same `worktree` machinery
`condukt worktree remove`/`cleanup` use elsewhere (force-remove the dir,
force-delete the branch) — whatever the primary worker produced is what
ships; shadow-run never merges its own output.

## Soft integrations

The `/condukt` skill has **soft dependencies** on several other plugins: each is
used when its binary is on `PATH` and skipped (soft no-op) otherwise, so condukt
never hard-requires any of them.

| plugin | where the skill uses it |
|---|---|
| `fugu-router` | deterministic model routing (`route`) and playbook search (`procedures search`); outcomes are recorded back via `state record-run`. |
| `gauge` | per-sub-agent / per-session cost capture (`gauge subagents` ≥ 0.3.0, `gauge session` ≥ 0.2.0) written into `state set --cost`. |
| `hypothesis` | inject open hypotheses into the interpreter and mark `linked_hypotheses` `awaiting-measurement` after the gate. |
| `backlog` / `compass` | source the next task when the argument is "what's next" (Phase 0-next). |
| `schemaguard` | pre-validate the decomposition JSON before `validate` (one re-ask). |
| `specguard` | post-gate spec-drift audit when `specguard.toml` exists. |
| `deepwiki` | inject architecture pages into the interpreter and `deepwiki refresh` after the gate. |
| `tracekit` / `replaykit` | record interpreter→worker→verifier spans and promote the run to a replay golden. |
| `trajectoryeval` | Phase 6: `extract` a worker's tool-call trajectory from its transcript, then `check` it against a task's `expected_trajectory` — a second, path-level verifier dimension alongside `done_criteria`. Skipped entirely when the task has no `expected_trajectory` or the binary is absent. |
| `curate` | golden-ification: on a `verified` task with mechanical `done_criteria`, the skill offers one HOTL confirm (`AskUserQuestion`) to promote the run to an evalkit golden; only on an explicit yes does it run `curate promote "<task.title>" --dataset <name>` — a decline writes nothing. |

## Constraints

- **Per-machine marketplace step.** Each user runs `/plugin marketplace add <url>`
  once — Claude Code does not auto-register a marketplace from a checked-in repo.
- **Per-platform binaries.** Linux x86_64 is committed in `bin/`. macOS arm64 /
  x86_64 are built by the GitHub Actions macOS runner (Apple SDK can't cross-build
  from Linux). If the host has no matching binary the launcher exits 0 with a build
  hint, so a hook never breaks a turn.
- **Exec bits.** Binaries and the launcher must keep their exec bit in the git
  index (`git update-index --chmod=+x bin/condukt bin/condukt-*`), because the repo
  is often checked out on a `core.filemode=false` mount.

## Development

```
cargo test          # unit tests (scheduling, gate, project key)
cargo clippy --all-targets
scripts/build-plugin-bin.sh        # stage bin/condukt-<os>-<arch> for the host
```

### Source of truth: edit the repo, not the cache

`crates/condukt/` (this directory) is the **single source of truth**. `/plugin
install` copies it to `~/.claude/plugins/cache/<owner>/condukt/<version>/` as a
plain copy (no `.git`), and the running `/condukt` skill loads its agents and
`SKILL.md` from there. Editing that cache copy — easy to do by accident when you
use condukt to improve condukt itself — produces edits that live outside git and
silently diverge from the repo.

Rule: **never hand-edit the cache.** Edit the files here, then refresh your local
install. When condukt orchestrates a change to its **own** plugin, point the
workers at this repo (a git worktree of it), never at the cache path.

```
scripts/sync-plugin-assets.sh           # repo -> cache: refresh your local install
scripts/sync-plugin-assets.sh --check   # report drift; exit 1 if cache != repo
```

Run `--check` before committing (or wire it into a pre-push hook) to catch a
cache that has drifted from the repo, or a new agent/skill file that was created
in the cache but never committed.

## License

MIT
