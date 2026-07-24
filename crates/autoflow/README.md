# autoflow

Session-end auto-flow gate for Claude Code — a **Stop** hook that keeps a
session from ending with work left on the floor, paired with a **SessionStart**
hook that proposes `/flow` when the backlog has pending work. When a turn
finishes, autoflow prompts `/record` once, then loops `/condukt` until the
project's pending tasks are cleared, and finally drains the cross-project
backlog. It blocks the Stop automatically up to 4× and, from the 5th prompt
onward, asks the user before continuing so a stuck loop can't run away. A
**PreCompact** + **UserPromptSubmit** pair keeps that same loop alive across a
`/compact`: PreCompact drops a resume marker when this session holds the
backlog lock, and the next UserPromptSubmit consumes it to re-inject a
"resume /flow" instruction exactly once.

Subscription-native: four hooks plus a bundled Rust binary, **no API key**, no
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
| **Continuing** | condukt clear, backlog has open items, compass charter fresh | block → `/backlog <next item>` |
| **Continuing** | backlog open but compass charter **stale** | nudge `/compass`, then stand down |
| **Done** | nothing pending | allow the turn to end |

compass is a soft dependency — if it's absent or unparseable, autoflow treats
the charter as fresh and proceeds. It also stands down entirely while another
live session holds the backlog lock, so a running `/flow` or `/backlog` driver
is never double-driven.

At **SessionStart** (startup / resume / clear), autoflow checks the same backlog
and, when open items exist and the charter is fresh, injects a one-line `/flow`
proposal as context; if the charter is stale it nudges `/compass` instead. With
nothing pending it stays silent, so it never breaks a turn on session open.

At **PreCompact**, if this session currently holds the backlog lock (i.e. a
`/flow` loop is actually driving it) and the user hasn't opted out
(`resume_flow_on_compact = false`), autoflow writes a resume marker — it never
blocks compaction. The following **UserPromptSubmit** consumes that marker (if
any) and injects a "resume `/flow`" instruction exactly once; every ordinary
turn without a marker stays silent.

## Why it exists

Long sessions tend to end with loose ends — a `/record` never taken, condukt
tasks left pending, backlog items parked and forgotten — simply because "the
turn finished." autoflow inserts a deterministic "unfinished-work check" at the
session boundary so the record → condukt → backlog chain actually runs to
completion. Judgement (how to do the work) stays with the skills and the LLM;
autoflow only owns the "may this end?" gate — which is why its auto-blocking is
capped, hands off to the user past the cap, and stands down when another session
holds the lock.

## Install (plugin)

Installed via the plugin marketplace, the bundled `hooks/hooks.json` wires the
**Stop**, **SessionStart**, **PreCompact**, and **UserPromptSubmit** hooks to
`${CLAUDE_PLUGIN_ROOT}/bin/autoflow` automatically — nothing else to do.
Thresholds (min turns, min tool events, max backlog prompts,
resume-flow-on-compact) come from config defaults; the gate is on by default.

## Standalone (cargo)

```sh
cargo install --path .
autoflow stop            # Stop hook: run the record→condukt→backlog state machine
autoflow session-start   # SessionStart hook: propose /flow when backlog has pending items
autoflow pre-compact     # PreCompact hook: drop a resume-flow marker if this session holds the lock
autoflow prompt-submit   # UserPromptSubmit hook: consume the marker and re-inject "/flow を再開" once
```

`autoflow stop` reads the hook JSON on stdin and prints a `block` decision (or
nothing); `autoflow session-start` prints an `additionalContext` proposal (or
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
