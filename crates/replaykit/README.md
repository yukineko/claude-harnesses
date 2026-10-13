# replaykit

A **trace→golden replay** regression harness — the sibling of
[`curate`](../curate) (which promotes fugu-router playbooks). Where curate turns
a *playbook* into a golden, replaykit turns a recorded **run trace** into one.

condukt runs are recorded by [`tracekit`](../tracekit) as append-only spans
(`~/.tracekit/<run_id>/spans.jsonl`). replaykit distils a run into a portable
**trajectory summary** — its ordered steps plus an `expect` block pinning the
run's phase set, error count, and cost — and promotes that into an
[`evalkit`](../evalkit) golden case. Replaying the golden re-checks the pinned
invariants, so a regression (a new error, a cost blowout, a missing phase)
surfaces as a failing golden when `evalkit run` is next run (there is no CI wiring;
nothing runs it automatically).

It is **subscription-native**: one bundled Rust binary, std + serde + clap, no
API key, no network.

## The record→promote→evalkit loop

```sh
# 1. a condukt run is recorded by tracekit → ~/.tracekit/<run_id>/spans.jsonl
# 2. promote it into a committed golden replay dataset
replaykit promote --run my-run-2026-06-28 --root . --dataset replayed
#    writes  evals/replay/fixtures/<id>.json      (the portable summary)
#       and  evals/replay/replayed.jsonl          (the golden, deduped by id)
# 3. evalkit runs the golden, whose cmd is `replaykit verify <fixture>`
evalkit run        # re-checks the pinned invariants (run it manually)
```

## Subcommands

### `replaykit extract --run <RID> [--spans <path>] [--out <path|->]`

Load a run's spans (from `--spans`, else `~/.tracekit/<sanitize(RID)>/spans.jsonl`),
build the trajectory summary, and print it as pretty JSON to `--out` (default
`-` = stdout). Malformed span lines are skipped.

### `replaykit verify <fixture.json>`

Read a committed summary fixture and **recompute** its aggregates (phase set,
error count, total cost) from the steps, checking them against the fixture's
`expect` block. This is a real self-test of the aggregation logic, not a static
read. Violations are printed to stderr.

### `replaykit promote --run <RID> [--spans <path>] [--root <dir>] [--evals-dir <name>] [--dataset <name>] [--draft]`

Build the summary, write it to `<root>/<evals_dir>/replay/fixtures/<id>.json`,
and append a golden line to `<root>/<evals_dir>/replay/<sanitize(dataset)>.jsonl`
(deduped by `id`). The golden's `cmd` is `["replaykit","verify",<rel-fixture>]`
with the fixture path **relative to root**, so the committed golden is portable.
`--root` defaults to the CWD, `--evals-dir` to `evals`, `--dataset` to `replayed`.
`--draft` is accepted but is currently a **no-op** (reserved for parity with curate).

## Exit codes

Mirrors the evalkit / trajectoryeval 0/1/2 gate policy:

| code | meaning |
|------|---------|
| `0`  | replay matched the pinned invariants (pass) |
| `1`  | a real regression / invariant violation |
| `2`  | harness error (missing / unreadable / malformed input) |

This is a plain CLI **gate**, not a lifecycle hook.
