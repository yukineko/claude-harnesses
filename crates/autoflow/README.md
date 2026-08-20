# autoflow

Session-end auto-flow gate for Claude Code — a **Stop** hook that keeps a
session from ending with an in-flight condukt run left on the floor. When a turn
finishes, autoflow prompts `/record` once, then loops `/condukt` while the run's
pending set keeps shrinking, escalating *visibly* if progress stalls. A
**PreCompact** + **UserPromptSubmit** pair keeps that loop alive across a
`/compact`: PreCompact drops a resume marker when this session holds the
backlog lock, and the next UserPromptSubmit consumes it to re-inject a
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
never runs work itself, and a missing state file or empty stdin exits 0 so the
turn is never broken.

## What it does

The Stop hook is a per-session state machine. Each phase decides whether to
block the turn (with a `/`-command nudge) or let it end:

| Phase | Condition | autoflow does |
|---|---|---|
| **Idle** | enough turns + tool events this session | block → `/session-insights:record` |
| **RecordRequested / Continuing** | condukt tasks still pending | block → `/condukt` (continues while the pending set keeps *shrinking* — no call-count ceiling; escalates *visibly* only when progress stalls) |
| **Done** | no pending task in the condukt run | allow the turn to end (the queue is not consulted) |

It stands down entirely while another live session holds the backlog lock, so a
running `/flow` or `/backlog` driver is never double-driven. That stand-down is
kept: it yields to a peer rather than asking anyone to do anything.

At **PreCompact**, if this session currently holds the backlog lock (i.e. a
`/flow` loop is actually driving it) and the user hasn't opted out
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
gate is on by default.

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

The committed `bin/autoflow-*` binaries are what the plugin ships, so end users
need neither cargo nor an API key. Rebuild and recommit them (the workspace
builds with `cargo build --workspace --release`) when you change behavior the
hook relies on.
