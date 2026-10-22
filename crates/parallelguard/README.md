A concurrency cap for Claude Code tool calls, counted by a binary instead of by
a sentence in a prompt.

parallelguard counts the calls that have passed `acquire` and not yet reached
`release`, and refuses the call that would exceed the cap.

Two independent pools, capped at **3 each**:

| pool | tools | cap |
|---|---|---|
| shell | `Bash` | 3 concurrent |
| subagent | `Task` / `Agent` | 3 concurrent |

Do not merge the two pools into one shared pool: with two pools a full shell
pool never blocks a subagent.

Nothing raises the cap: 3 is a ceiling, and an out-of-range-high or unparsable
`HARNESS_MAX_PARALLEL` value resolves to the ceiling.
`HARNESS_MAX_PARALLEL` may **lower** the cap.
A value of `0` is raised to the floor of 1, not to the ceiling.

```
PreToolUse(Bash|Task|Agent)          -> parallelguard acquire   take a slot, or deny
PostToolUse(Bash|Task|Agent)         -> parallelguard release   give the slot back
SessionStart|UserPromptSubmit|Stop   -> parallelguard reset      clear the ledger
```

The ledger is one JSON file per session under
`$HOME/.parallelguard/state/sessions/` (override with an absolute
`PARALLELGUARD_STATE_DIR`).
It is guarded by an advisory lockfile so the concurrent hook processes of a
parallel tool batch cannot race each other into admitting more than the cap.

Silence is not available as a degraded mode: none of the paths below exits 0
with empty stdout.
An unparseable payload, an unreadable ledger, a lock that will not come free, a
write that fails, a missing platform build — each means the number in flight is
**unknown**, and an unknown count is not a free slot: all of them deny (CLAUDE.md 3).

* the ledger is cleared at every turn boundary by `reset`;
* a killed hook never wedges the lock: it is a kernel advisory lock
  (`flock`) on a persistent lockfile, released by the kernel when the holder
  dies — there is no staleness timeout and nothing is ever stolen;

* Slots never expire by age.
* `Bash` with `run_in_background: true` is not special-cased: it takes a slot
  at `acquire` and gives it back at `release` like any other `Bash` call, so a
  background process still running after its `release` is outside the count.

```sh
parallelguard status       # the cap in force, per-session counts, recent denials
parallelguard --protects   # print the PROTECTS / AGAINST / GROUNDS statement
```

`status` prints per-pool counts while a session ledger exists, even with
nothing in flight, and a "the gate is NOT running" line when no ledger exists.
The turn-boundary `reset` deletes the ledger, so right after a `reset` `status`
prints that same "NOT running" line although the gate has run.
