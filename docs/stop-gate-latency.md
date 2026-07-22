# Stop-gate latency contract

The harness registers ten independent **Stop** hooks, each shipped by its own
plugin. Every one runs when the agent tries to end a turn. This document is the
contract they collectively honor: their timeouts, the ordering/independence
rules they must obey, the aggregate latency budget, and the observability path
that measures the ones that can actually hurt.

## The ten Stop gates

| gate | command | timeout |
|---|---|---|
| autoflow | `autoflow stop` | 10s |
| condukt | `condukt state record-run --all` | 15s |
| ctxrot | `ctxrot stop` | 10s |
| context-governor | `context-governor` | 10s |
| budgetguard | `budgetguard gate` | 30s |
| tdd | `tdd gate` | 30s |
| precommit-audit | `precommit-audit --mode stop` | 30s |
| donegate | `donegate gate` | 600s |
| reviewgate | `reviewgate review` | 600s |
| propguard | `propguard check` | 600s |

Timeouts are the per-hook values registered in each plugin's `hooks.json`. Seven
gates are "light" (≤30s); three are "heavy" (600s) because they may shell out to
project acceptance commands, a subprocess reviewer, or a property checker.

## Ordering & independence contract

Claude Code fires the Stop hooks registered by separate plugins **independently**,
and their relative order is **not guaranteed**. There is no documented,
stable-across-versions ordering between hooks contributed by different plugins.
Therefore every gate MUST be:

- **Order-independent.** A gate must never depend on another gate having already
  run (or not run) in the same Stop event. No gate may read another gate's
  in-flight decision, and none may assume it runs first or last.
- **Side-effect-isolated.** A gate writes only its OWN state (its own
  `~/.<gate>/state/...` files). It must not mutate another gate's state, and it
  must not rely on shared mutable state being in a particular condition.
- **Fail-closed on crash, for any gate that carries a verdict.** A gate whose
  Stop handler can emit `{"decision":"block"}` runs its body under
  `harness_core::gate::run::run_guarded`: a panic there means the check never
  ran, so the crash itself is surfaced as a fail-closed block (bounded to one
  occurrence via `stop_hook_active`, so a deterministically-crashing gate can't
  trap the session) instead of silently allowing the stop. This applies to
  donegate, reviewgate, propguard, tdd, budgetguard, autoflow, and ctxrot.
  **Never-break-a-turn only applies to gates that carry no verdict** —
  condukt's `record-run --all` (pure fugu-router bookkeeping), context-governor,
  and precommit-audit never emit a block decision, so an internal error there is
  swallowed and the stop allowed (exit 0); the observability write itself failing
  is likewise swallowed, never surfaced.

Because the gates are independent, their wall-times are effectively additive from
the user's point of view: a Stop event is not "done" until all registered hooks
have returned (or timed out). That is what makes an *aggregate* budget the right
unit to watch, not any single gate's timeout.

## Aggregate latency budget

The aggregate Stop-hook budget defaults to **30s** and is overridable via the
`HARNESS_HOOK_LATENCY_BUDGET_MS` environment variable (parsed as `u64`
milliseconds; unset/garbage falls back to the 30s default).

The three 600s gates dominate wall-time: each of the seven light gates is ≤30s on
its own and, in practice, returns in well under a second, whereas the heavy gates
can legitimately run for minutes when they invoke a full test suite, a subprocess
reviewer, or a property checker. The budget exists to catch the case where the
heavy gates' combined per-session time balloons past what a human-on-the-loop
turn should tolerate.

## Observability path

The three heavy gates are instrumented. At the top of each one's Stop handler
they start a timer, and immediately before every `process::exit` they append one
line to a single shared central ledger:

```
~/.harness/state/hook-latency.jsonl
```

Each line is a `LatencyEntry { ts, hook, session, elapsed_ms }` (rfc3339 local
timestamp, gate name, session id or `""` when unknown, elapsed milliseconds).
Recording is best-effort: any IO or serialization failure is swallowed so the
observability log can never break the turn it is measuring.

`harness-status hooks` reads that one file, groups entries by session, sums
`elapsed_ms` overall and per hook (repeats within a session are SUMMED, giving the
cumulative wall-time a hook cost that session), sorts sessions slowest-first, and
warns for any session whose combined total exceeds the budget:

```
harness-status hooks            # human panel, per-session totals + ⚠ over budget
harness-status hooks --json     # HookLatencyReport as JSON
```

The same panel is included in the default all-panels `harness-status` view.

## Coverage — honest scope

Only the **three heavy (600s) gates** — donegate, reviewgate, propguard — are
instrumented today. The seven light gates (autoflow, condukt, ctxrot,
context-governor, budgetguard, tdd, precommit-audit) are ≤30s each, are
documented here, but are **not yet measured**: they do not write to the ledger,
so they do not appear in `harness-status hooks`. This is deliberate — the heavy
gates are where wall-time actually accrues — but it means the aggregation is a
view of the heavy gates only, not full Stop-hook coverage. Do not read an empty
or small `hook-latency.jsonl` as proof that a turn's total Stop-hook time was
small; it only reflects the instrumented three.
