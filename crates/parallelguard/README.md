# parallelguard

A per-session concurrency cap for Claude Code, enforced by a binary instead of
by a sentence in a prompt.

Freezing a WSL2 host takes one thing: too many processes at once. Every fan-out
in this harness — a skill spawning one auditor per shard, a batch of parallel
`Bash` calls in a single message — spends the same budget. Before parallelguard
the only thing between a wide fan-out and a frozen session was a request in a
SKILL.md asking the model to please send at most N at a time.

parallelguard counts what is *actually in flight* and refuses the call that
would exceed the cap.

## What it bounds

Two independent pools, **3 each** by default:

| pool | tools | cap |
|---|---|---|
| shell | `Bash` | 3 concurrent |
| subagent | `Task` / `Agent` | 3 concurrent |

Two pools rather than one shared pool of 3, because a shared pool deadlocks: a
subagent holds its slot for its whole lifetime, so with 3 subagents live, every
`Bash` call *those same subagents* make would be denied — the holders could
never progress and so could never release.

`HARNESS_MAX_PARALLEL` may **lower** the cap. Nothing raises it: 3 is a ceiling,
and an out-of-range or unparsable value resolves to the ceiling.

## How it is wired

```
PreToolUse(Bash|Task|Agent)          -> parallelguard acquire   take a slot, or deny
PostToolUse(Bash|Task|Agent)         -> parallelguard release   give the slot back
SessionStart|UserPromptSubmit|Stop   -> parallelguard reset      clear the ledger
```

The ledger is one JSON file per session under
`$HOME/.parallelguard/state/sessions/` (override with an absolute
`PARALLELGUARD_STATE_DIR`), guarded by an advisory lockfile so the concurrent
hook processes of a parallel tool batch cannot race each other into admitting
more than the cap.

## Cannot-determine denies

An unparseable payload, an unreadable ledger, a lock that will not come free, a
write that fails, a panic in the binary, a missing platform build — each means
the number in flight is **unknown**, and an unknown count is not a free slot.
All of them deny (CLAUDE.md 3). Silence is not available as a degraded mode: a
`PreToolUse` hook that exits 0 with no output *is* an allow, byte for byte, so
"the gate broke" would be indistinguishable from "the gate found room".

The cost of that choice is bounded on purpose — every deny is recoverable
without a human:

* the ledger is cleared at every turn boundary by `reset`;
* a lockfile abandoned by a killed process is stolen after 30 s;
* a denied call is a call that simply did not run — re-issuing it costs a round,
  not work.

## What it does NOT bound

* `Bash` with `run_in_background: true` returns immediately, so its
  `PostToolUse` fires while the process keeps running. Background shells are
  outside the count.
* Slots never expire by age. A long-running call holds its slot until it
  finishes or the turn ends — "this looks old, assume it died" would be a
  permissive guess and a way to exceed the cap by taking longer.

## Operator commands

```sh
parallelguard status   # the cap in force, per-session counts, recent denials
```

`status` distinguishes "nothing in flight" from "this hook has never run" —
they produce an identical empty ledger otherwise.
