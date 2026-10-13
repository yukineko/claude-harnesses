# autoflow

Session-end auto-flow gate for Claude Code — a **Stop** hook that keeps a
session from ending with an in-flight condukt run left on the floor. When a turn
finishes, autoflow prompts `/record` once, then loops `/condukt` while the run's
pending set keeps shrinking, escalating *visibly* if progress stalls. A
**PreCompact** + **UserPromptSubmit** pair keeps that loop alive across a
`/compact`: PreCompact drops a resume marker when this session is driving the
project's backlog queue (per `backlog lock status`), and the next UserPromptSubmit consumes it to re-inject a
"resume /flow" instruction exactly once.

**The backlog queue is out of scope, as of 2026-08-20.** Until then an empty
condukt pending set fell through to the queue and blocked the Stop with
"/backlog を実行してください" every turn (behind a compass-freshness gate), and a
**SessionStart** hook made the same request up front as "バックログに N 件…/flow
で開始しますか？". All three — the proposal, the Stop arm, and its compass gate —
were retired on the user's instruction. Whether to work the queue is the
operator's call, made by invoking `/flow` or `/backlog`. The backlog crate still
surfaces its own queue state at SessionStart; what is gone is the instruction to
act on it now.

Subscription-native: three hooks plus a bundled Rust binary, **no API key**, no
daemon. The Stop hook only ever emits a `block` decision with a reason — it
never runs the work itself (its only side effects are its own per-session state
file, marking the pending condukt tasks `running` on a routine continuation, and
an overwatch violation record per block). It is a gate, so it does not resolve
"cannot determine" to "allow": an unreadable condukt run-state **blocks** (see
below), and a panic in the Stop hook fails closed via
`harness_core::gate::run::run_guarded` (one `block`; only the immediately
following `stop_hook_active` re-entry is allowed through so a crash cannot trap
the session). The launcher likewise emits a `block` for `stop` when no binary
exists for the host. What *does* end silently: no session id at all (neither
the payload nor `CLAUDE_CODE_SESSION_ID`), the gate disabled, a missing
condukt run file (condukt never ran here — observed as "no pending task"), and
the stand-downs listed below.

## What it does

The Stop hook is a per-session state machine. Each phase decides whether to
block the turn (with a `/`-command nudge) or let it end:

| Phase | Condition | autoflow does |
|---|---|---|
| **Idle** | enough turns + tool events this session | block → `/session-insights:record` |
| **RecordRequested / Continuing** | condukt tasks still pending | block → `/condukt` (continues while the pending set keeps *shrinking* — no call-count ceiling; escalates *visibly* only when the pending set fails to shrink `stuck_threshold` (default 3) Stops in a row) |
| **RecordRequested / Continuing** | condukt run-state could not be read | block with a "判定不能" (undetermined) reason; the phase is left untouched so the next Stop re-observes |
| **Done** | no pending task in the condukt run | allow the turn to end (the queue is not consulted) |

The escalation is never silent: it blocks with a message worded by the shared
autonomy switch (`harness_core::autonomy` — autonomous: "continuing"; otherwise:
"ask the user"). The escalating Stop is the one Stop that does *not* re-mark the
pending tasks `running`.

Two further checks run on every Stop:

- **Explicit user stop** (before the state machine): if the user's most recent
  transcript signal — a typed message, or their answer to an `AskUserQuestion` —
  is an explicit stop instruction ("止めて", "stop", …), *this* Stop is allowed
  through untouched. It is not latched into `Done`; nudging resumes if the user
  keeps working. An unreadable or absent signal never counts as a stop.
- **Delegation-record advisory** (in RecordRequested / Continuing): if this
  session's transcript shows `/flow` drove a condukt run to completion without a
  `fugu-router record` for it, block once per session asking for
  `fugu-router record --class flow-delegation …`.

It stands down entirely (allows the Stop) while `backlog lock status` reports a
live driver for this project, so a running `/flow` or `/backlog` driver is never
double-driven. That stand-down is kept: it yields to a peer rather than asking
anyone to do anything. Note its direction: a liveness answer autoflow could not
obtain (backlog failed to run, exited non-zero, printed something
uninterpretable, or its install state could not be determined) is treated as
"a driver may be active" and also stands down — this protects against
double-driving, at the cost of not nudging on that Stop. Backlog being
*observably* not installed does not stand down.

At **PreCompact**, if this session is the exclusive lock holder or one of the
registered drivers in `backlog lock status` (i.e. a `/flow` loop is actually
driving from this session; an unreadable status counts as "not driving") and the user hasn't opted out
(`resume_flow_on_compact = false`), autoflow writes a resume marker — it never
blocks compaction. The following **UserPromptSubmit** consumes that marker (if
any) and injects a "resume `/flow`" instruction exactly once; every ordinary
turn without a marker stays silent.

## Why it exists

Long sessions tend to end with loose ends — a `/record` never taken, a condukt
run left half-finished — simply because "the turn finished." autoflow inserts a
deterministic "unfinished-work check" at the session boundary so the record →
condukt chain actually runs to completion. Judgement (how to do the work) stays
with the skills and the LLM; autoflow only owns the "may this end?" gate.

Parked backlog items are deliberately NOT in that list any more. A run this
session started and abandoned is an accident; a queue item nobody has started is
not, and asking about it every single turn adds no information — repetition is
not detection. That is why the queue arms were retired rather than quietened.

## Install (plugin)

Installed via the plugin marketplace, the bundled `hooks/hooks.json` wires the
**Stop**, **PreCompact**, and **UserPromptSubmit** hooks to
`${CLAUDE_PLUGIN_ROOT}/bin/autoflow` automatically — nothing else to do
(**SessionStart** was removed in 2026-08-20). Thresholds (min turns, min tool
events, stuck threshold, resume-flow-on-compact) come from config defaults; the
gate is on by default. To override, write `~/.autoflow/config.toml`:

```toml
enabled = true                # default true
min_turns = 2                 # turns before the /record prompt
min_tool_events = 3           # tool events before the /record prompt
stuck_threshold = 3           # no-progress Stops before visible escalation
resume_flow_on_compact = true
state_dir = "~/.autoflow/state"
```

An unparseable config file is ignored (defaults apply).

## Standalone (cargo)

```sh
cargo install --path .
autoflow stop            # Stop hook: run the record→condukt state machine
autoflow pre-compact     # PreCompact hook: drop a resume-flow marker if this session holds the lock
autoflow prompt-submit   # UserPromptSubmit hook: consume the marker and re-inject "/flow を再開" once
```

`autoflow stop` reads the hook JSON on stdin and prints a `block` decision (or
nothing); `autoflow pre-compact` and `autoflow prompt-submit` are silent unless
the resume-marker gate is met. `AUTOFLOW_DISABLE=1` silences the gate.

## Build

```sh
cargo test
```

Only the `bin/autoflow` launcher is committed; it execs the per-platform
`bin/autoflow-<os>-<arch>` binary, which is gitignored and placed by the
repo's `scripts/rollout-plugins.sh` (see `docs/repo-operations.md`). End users
need neither cargo nor an API key. Re-run the rollout when you change behavior
the hook relies on.
