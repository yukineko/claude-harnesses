A concurrency cap for Claude Code tool calls, counted by a binary instead of by
a sentence in a prompt. [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_shells_admit_exactly_three]

parallelguard counts the calls that have passed `acquire` and not yet reached
`release`, and refuses the call that would exceed the cap. [test: crates/parallelguard/src/model.rs::the_fourth_concurrent_shell_is_denied] [test: crates/parallelguard/tests/concurrency.rs::releasing_returns_the_slot]

Two independent pools, capped at **3 each**: [test: crates/parallelguard/src/model.rs::a_full_shell_pool_does_not_block_subagents] [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_shells_admit_exactly_three] [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_subagents_admit_exactly_three]

| pool | tools | cap [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_shells_admit_exactly_three] |
|---|---|---|
| shell | `Bash` | 3 concurrent [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_shells_admit_exactly_three] |
| subagent | `Task` / `Agent` | 3 concurrent [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_subagents_admit_exactly_three] [test: crates/parallelguard/src/model.rs::bash_and_task_draw_from_different_pools] |

Do not merge the two pools into one shared pool: with two pools a full shell
pool never blocks a subagent. [test: crates/parallelguard/tests/concurrency.rs::a_full_shell_pool_never_blocks_a_subagent]

Nothing raises the cap: 3 is a ceiling, and an out-of-range-high or unparsable
`HARNESS_MAX_PARALLEL` value resolves to the ceiling [test: crates/parallelguard/tests/prose_claims.rs::claim_ceiling] [test: crates/parallelguard/tests/concurrency.rs::the_cap_can_be_lowered_but_not_raised].
`HARNESS_MAX_PARALLEL` may **lower** the cap [test: crates/parallelguard/tests/prose_claims.rs::claim_lower_cap].
A value of `0` is raised to the floor of 1, not to the ceiling. [test: crates/parallelguard/tests/prose_claims.rs::claim_ceiling_zero_actual_behaviour]

```
PreToolUse(Bash|Task|Agent)          -> parallelguard acquire   take a slot, or deny [test: crates/parallelguard/tests/prose_claims.rs::claim_hooks_wiring] [test: crates/parallelguard/src/model.rs::the_fourth_concurrent_shell_is_denied]
PostToolUse(Bash|Task|Agent)         -> parallelguard release   give the slot back [test: crates/parallelguard/tests/prose_claims.rs::claim_hooks_wiring] [test: crates/parallelguard/tests/concurrency.rs::releasing_returns_the_slot]
SessionStart|UserPromptSubmit|Stop   -> parallelguard reset      clear the ledger [test: crates/parallelguard/tests/prose_claims.rs::claim_hooks_wiring] [test: crates/parallelguard/tests/concurrency.rs::reset_clears_a_leaked_ledger]
```

The ledger is one JSON file per session under
`$HOME/.parallelguard/state/sessions/` (override with an absolute
`PARALLELGUARD_STATE_DIR`) [test: crates/parallelguard/tests/prose_claims.rs::claim_ledger_path] [test: crates/parallelguard/src/store.rs::a_relative_state_dir_override_is_ignored].
It is guarded by an advisory lockfile so the concurrent hook processes of a
parallel tool batch cannot race each other into admitting more than the cap. [test: crates/parallelguard/tests/prose_claims.rs::claim_lock_serializes] [test: crates/parallelguard/tests/concurrency.rs::eight_simultaneous_shells_admit_exactly_three]

Silence is not available as a degraded mode: none of the paths below exits 0
with empty stdout [test: crates/parallelguard/tests/prose_claims.rs::claim_never_silent] [test: crates/parallelguard/tests/concurrency.rs::the_launcher_denies_when_no_binary_is_bundled].
An unparseable payload, an unreadable ledger, a lock that will not come free, a
write that fails, a missing platform build — each means the number in flight is
**unknown**, and an unknown count is not a free slot: all of them deny (CLAUDE.md 3) [test: crates/parallelguard/tests/concurrency.rs::an_unparseable_payload_denies] [test: crates/parallelguard/tests/concurrency.rs::an_unreadable_ledger_denies_rather_than_reading_as_empty] [test: crates/parallelguard/tests/prose_claims.rs::claim_lock_held_denies] [test: crates/parallelguard/tests/prose_claims.rs::claim_write_failure_denies] [test: crates/parallelguard/tests/concurrency.rs::the_launcher_denies_when_no_binary_is_bundled].

* the ledger is cleared at every turn boundary by `reset`; [test: crates/parallelguard/tests/prose_claims.rs::claim_hooks_wiring] [test: crates/parallelguard/tests/concurrency.rs::reset_clears_a_leaked_ledger]
* a killed hook never wedges the lock: it is a kernel advisory lock
  (`flock`) on a persistent lockfile, released by the kernel when the holder
  dies — there is no staleness timeout and nothing is ever stolen; [test: crates/parallelguard/tests/prose_claims.rs::claim_killed_holder]

* Slots never expire by age. [test: crates/parallelguard/src/model.rs::a_slot_never_expires_by_age]
* `Bash` with `run_in_background: true` is not special-cased: it takes a slot
  at `acquire` and gives it back at `release` like any other `Bash` call, so a
  background process still running after its `release` is outside the count. [test: crates/parallelguard/tests/prose_claims.rs::claim_background_not_special]

```sh
parallelguard status       # the cap in force, per-session counts, recent denials [test: crates/parallelguard/tests/prose_claims.rs::claim_status_contents]
parallelguard --protects   # print the PROTECTS / AGAINST / GROUNDS statement [test: crates/parallelguard/tests/protects_flag.rs::protects_prints_three_labelled_nonempty_lines_and_exits_zero]
```

`status` prints per-pool counts while a session ledger exists, even with
nothing in flight, and a "the gate is NOT running" line when no ledger exists [test: crates/parallelguard/tests/prose_claims.rs::claim_status_distinguishes].
The turn-boundary `reset` deletes the ledger, so right after a `reset` `status`
prints that same "NOT running" line although the gate has run. [test: crates/parallelguard/tests/prose_claims.rs::claim_status_after_reset]
