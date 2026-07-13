# ADR 0001 — Cross-harness injection budget

- **Status:** Phase 1 landed (ctxrot observation); **Phase 2 landed** (cross-harness aggregation + detection/warn budget). Active same-turn enforcement remains future work.
- **Date:** 2026-06-22
- **Scope:** the harness-plugin family (`ctxrot`, `playbook`, `gauge`, `session-insights`, …) — multiple plugins, distributed together from the `yukineko/claude-harnesses` monorepo
- **Owner:** ctxrot maintainers (originating repo); requires buy-in from the sibling plugins

## Context

ctxrot fights context rot by keeping the window light. But it is not the only
plugin injecting into the model context, and **several plugins inject on the same
event**: `UserPromptSubmit`. ctxrot's `guard` and `playbook` both add text every
qualifying prompt; `gauge` / `session-insights` add their own. Each is individually
"conditional and minimal", but **nobody bounds the sum**.

The failure mode is the same one ctxrot exists to prevent, one level up: a family
of well-behaved plugins that each add "just a little" every turn collectively
push the window the wrong way. ctxrot capping *its own* injection (per
`guard_inject_max_chars`, N2) is necessary but not sufficient — it cannot see, let
alone bound, what its siblings add in the same turn.

This is intrinsically cross-repo: no single plugin can solve it alone, because the
budget must be shared state across processes that don't otherwise coordinate.

## Constraints (inherited from ctxrot's invariants)

1. A hook must never break the turn (exit 0, silent on any error). Any shared
   mechanism must degrade to "inject normally" when unavailable.
2. No LLM calls in hooks.
3. Per-turn coordination must be cheap (hooks run on a tight timeout) and must not
   require a daemon.
4. CJK-safe accounting (char counts, not bytes) where a budget is enforced.
5. Subscription-only; no external services.

## Decision

Proceed in two phases. **Observe before coordinating.**

### Phase 1 — Observation (this ADR lands the ctxrot half)

Each injecting plugin records *its own* per-turn injection size to a place a
report can sum. ctxrot now emits an `inject` metric per qualifying prompt
(`{chars, blocks}`, post-cap) to `<state_dir>/metrics.jsonl`, rolled up as
`inject_chars` and shown in `ctxrot metrics compare` (the `inject A/B` line).

The cross-harness rollup belongs in an aggregator that already reads multiple
plugins' signals — **`gauge` or `session-insights`** — which should:
- read each plugin's per-turn injection size (from a shared, agreed channel; see
  below), and
- report combined per-turn and per-session injection, ideally with a per-plugin
  breakdown.

Deliverable of Phase 1: you can *see* the combined number. No enforcement yet.

### Phase 2 — Cross-harness aggregation + detection/warn budget (LANDED)

Landed in `harness-core` + `harness-status`, not the sibling repos: all five
`UserPromptSubmit` injectors in this monorepo now report to a single shared
ledger.

**Shared channel.** `harness_core::inject_metrics` owns a central append-only
JSONL ledger at `<base_dir("harness")>/state/inject-metrics.jsonl` (i.e.
`~/.harness/state/inject-metrics.jsonl`), mirroring the `hook-latency.jsonl`
model. Each line is an `InjectEntry { ts, turn_key, plugin, session, chars }`.
Recording is best-effort and swallows every error (never breaks a turn), and a
zero-char injection records nothing.

**Turn id — solved without cross-process coordination.** The hard problem in the
original design ("defining a turn reliably across hooks") is resolved by the
observation that all five injectors receive the SAME user `prompt` on the SAME
`UserPromptSubmit` event. So `turn_key = FNV-1a(session_id + "\n" + prompt)` (a
STABLE hash, deliberately not `DefaultHasher`) is a deterministic shared key the
five separate processes derive independently — no daemon, no lock, no shared
mutable state file. `inject_metrics::turn_key` computes it; aggregation groups by
it.

**Instrumented injectors (post-cap size).** Each injector records the CHAR count
of the exact string it emits, at its emit site, after its own per-injector cap:
`playbook`, `runbook`, `ctxrot` (guard), `context-governor`
(`additionalContext` on UserPromptSubmit), and `fugu-router`. Injection behavior
is unchanged — only a side-effecting `record()` call was added before the
existing emit.

**Aggregation + warn.** `harness-status inject` (and the default all-panels view)
reads the ledger via `inject_metrics::aggregate`, groups by `turn_key`, sums the
combined and per-plugin chars, sorts most-recent-turn-first, and flags any turn
whose combined size exceeds `HARNESS_INJECT_BUDGET_CHARS` (default 20000 chars)
with a clear `⚠ turn <key> injection total <n> chars exceeds budget <b>` line.

**Shipped enforcement is detection + warn, honestly not forced truncation.**
Cross-process same-turn ordering across the five hooks is not guaranteed, and
dropping safety-critical injected context to fit a same-turn budget is riskier
than a transient overflow. So Phase 2 ships observability and an over-budget
warning, not same-turn trimming. `inject_metrics::remaining_for_turn(path,
turn_key, budget)` is provided (and tested) as a cooperative self-cap helper —
budget minus what siblings already recorded for the turn, saturating at 0 — that
a future active-enforcement phase can wire in per-injector, but no injector calls
it yet.

### Phase 3 — Active shared budget (design only; implement after Phase 2 data)

Two candidate mechanisms, to be chosen once Phase 1 shows the real magnitude:

- **(A) Loose cooperative budget via a per-turn shared state file.** A well-known
  path (e.g. `~/.claude/state/inject-budget-<turn-id>.json`) holds "remaining
  injection budget for this turn". Each `UserPromptSubmit` hook, in registration
  order, reads remaining, injects up to its share, decrements, writes back. The
  turn id comes from the hook input (session id + a per-turn nonce). Pros: no
  daemon, fully degradable (missing/locked file → inject normally). Cons: hook
  ordering is not guaranteed across plugins; file contention; defining "a turn"
  reliably across hooks.

- **(B) Priority-ranked global cap.** A shared config declares a family-wide
  per-turn ceiling and a priority order across plugins (safety > knowledge
  injection > anchor/supplemental). Each plugin tags its blocks with a priority
  (ctxrot already does internally — `Prio` in `guard.rs`); the aggregator (or a
  thin shared lib) trims lowest-priority blocks across *all* plugins to fit. Pros:
  globally optimal drops, reuses ctxrot's existing priority model. Cons: needs a
  shared library or a coordinating hook all plugins call; the largest change.

**Leaning:** start with (A) as an opt-in, default-off cooperative budget (matches
ctxrot's conservative posture and the "never break the turn" invariant), and only
escalate to (B) if Phase 1 data shows the loose scheme leaves too much on the
table. Either way, the shared channel and "turn id" definition are the hard part
and must be specified in a follow-up ADR before code.

## Options considered (and rejected for now)

- **Do nothing / rely on each plugin's own cap.** Rejected: per-plugin caps
  cannot bound the sum, which is the actual problem.
- **A coordinating daemon.** Rejected: violates the no-daemon / cheap-hook
  constraint and adds an availability dependency that could break turns.
- **Hard global cap with no priority.** Rejected: would drop safety-critical
  warnings as readily as supplemental anchors.

## Consequences

- ctxrot's injection is now observable in-repo (Phase 1, ctxrot side). This is the
  seed; the family-wide number requires the sibling plugins to emit comparably and
  an aggregator to sum.
- No behavior change yet for users: `inject` is a metric only.
- A second ADR is required to fix the shared-channel format and the turn-id
  definition before any enforcement code in the sibling repos.
- The work is sequenced after ctxrot's own cap (N2) deliberately: bounding the
  largest single contributor first makes the residual cross-harness problem
  smaller and easier to measure.

## Outcome verification

How we will know each phase actually worked (not just merged):

- **Phase 1 (observation):**
  - `ctxrot metrics compare on- off-` shows a non-zero `inject` line for guard-ON
    sessions and ~0 for `GUARD_DISABLE` sessions. *(verifiable now, in this repo)*
  - The chosen aggregator (`gauge`/`session-insights`) reports a combined per-turn
    injection figure across ≥2 plugins on a real session, with a per-plugin
    breakdown that sums to the total. *(pending those repos)*
- **Phase 2 (budget), once implemented:**
  - With the budget enabled and several injecting plugins active, the combined
    per-turn injection stays at or below the configured ceiling on a heavy session
    (measured via the Phase 1 report).
  - Disabling the budget (or removing the shared state file) reproduces the
    uncapped combined figure — i.e. the mechanism, not chance, did the bounding.
  - No turn is ever broken by the coordination path: inject-normally fallback is
    exercised by a test that makes the shared state unreadable/locked.

## References

- ctxrot N2 (`guard_inject_max_chars`) — per-plugin cap and the `Prio` model this
  ADR's option (B) would lift to family scope: `src/hooks/guard.rs`.
- ctxrot `inject` metric (Phase 1 observation): `src/metrics.rs`, `src/hooks/guard.rs`.
