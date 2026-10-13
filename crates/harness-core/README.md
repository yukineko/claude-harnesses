# harness-core

The single source of truth for the unchanging infrastructure shared across the
harness's Claude Code plugins. Anything that **must be identical in every
plugin** — the parallel-session-safe note store, the hook I/O wrappers
(exit-0 for verdict-free observability hooks, fail-closed for Stop gates), the
three-valued `Verdict`/`Determination` types, the `~/.claude/settings.json`
install mechanics, per-project addressing, the metrics sink — lives here once, so the plugins compose it
instead of re-implementing it (and drifting apart).

This is a **build-time LIBRARY crate, not a plugin**: it ships no `plugin.json`,
no hooks, and no binary. Each plugin links it statically into its self-contained
binary, so the distributed `crates/<plugin>/bin/` never references
`../harness-core` at runtime. Plugin-specific domain logic and config/metrics
*fields* stay in each plugin crate.

## What it provides

| Module | What it shares |
|---|---|
| `store` | Durable Obsidian-compatible note store, per-project, with parallel-session-safe fallback (a harness invariant) |
| `hook` | Hook stdin payload struct + `run_hook` wrapper (catches any panic, logs it to stderr, always exits 0). **Only for verdict-free observability hooks** — a hook that returns a decision must use `gate::run::run_guarded` instead, because exit 0 with no decision is indistinguishable from "allow" |
| `hook_latency` | Central append-only Stop-hook latency ledger (`<base_dir>/state/hook-latency.jsonl`) — one shared file so aggregation is a single read; best-effort (IO errors are swallowed; observability only, not a verdict) |
| `install` | `~/.claude/settings.json` load / timestamped backup / write + ownership detection by command markers |
| `hash` | FNV-1a (32/64-bit) — the single source of truth for the non-cryptographic hashes that are load-bearing for on-disk addressing |
| `projkey` | Per-project key `<basename>-<fnv1a32-hex>` — single source of truth for run-state file addressing |
| `config` | home/base-dir resolution, tilde expansion, env-var parsing primitives |
| `gate` | Shared Stop-gate machinery: `runner` (bounded-tail subprocess runner with timeout), `state` (per-session `max_attempts` counter), `run` (`run_guarded` panic barrier + one-shot skip), and `protection` (the `Protection` statement every blocking gate declares). `run_guarded` **fails closed**: a panicking gate body emits `{"decision":"block"}` on the first stop; only a second consecutive panic (`stop_hook_active`) is allowed through; interactive runs exit 1 |
| `spans` | Span model + defensive JSONL loader (`~/.tracekit/<run_id>/spans.jsonl` on-disk contract) |
| `session` | Canonical per-session record (`<state_dir>/sessions/<id>.json`) |
| `usage` / `transcript` | Streaming JSONL transcript reader + per-model token/usage aggregation (never loads a whole transcript) |
| `estimate` | The one bundled transcript → (usage, cost) estimator (`estimate_transcript_cost`, re-exported at the crate root), folding `usage::aggregate` + `pricing::session_cost` into a single call — `gauge`, `budgetguard`, and `session-insights` all consume this instead of each hand-rolling the aggregate-then-price pair |
| `metrics` | The append-only JSONL metrics SINK, parallel-safe |
| `pricing` | Model→USD cost table incl. cache read/write multipliers |
| `ledger` | Persistent daily spend ledger (`~/.budgetguard/state/ledger.json`) |
| `daily` | Once-per-calendar-day guard |
| `inject` | Shared substrate for context-injection hooks (`playbook`, `runbook`) |
| `inject_metrics` | Cross-harness UserPromptSubmit injection-size ledger, keyed by `turn_key = hash(session + prompt)` so the five injectors sum a single turn without cross-process coordination |
| `interrogate` | Domain-agnostic gate-by-gate interrogation control structure |
| `shell` | Cross-platform shell invocation, single source of truth |
| `trust` | Workspace-trust gate for honoring command strings from project-local config |
| `scorer` | Deterministic priority scorer (`score`, re-exported at the crate root): `severity_weight × clamp(goal_proximity, 0.0, 1.0) ÷ effort_factor × lens_multiplier`. Pure and referentially transparent — no clock, no RNG, no I/O. `L2`/`L5` (scout's security/safety lenses) carry a >1.0 multiplier ("壊さない・安全側") |
| `verdict` | The one three-valued gate verdict (`Verdict`: `Clean` / `Violation` / `Undetermined`) and the generic `Determination<T>`. `Verdict` has no `Default` / `From<bool>`, is `#[must_use]`, and `Clean` needs a private witness, so a fail-open is a compile error rather than a review finding |
| `boundary` | Fallible input boundaries (directory listings, file reads, subprocesses) that return `Determination`, so "could not observe" survives the call instead of flattening into an empty `Vec` / ignored exit code |
| `undetermined` | Telemetry for the "could not determine" branch: every `Verdict::undetermined` / `Determination::undetermined` records one line (crate, file, line, reason), aggregated by `overwatch undetermined-metrics` |
| `degrade` | Evidence degradation + the verdict-monotonicity property (degrading what a gate can see must never make its verdict more permissive) |
| `git_probe` | Shared "is this a git repository?" probe for the gate crates, which does not collapse "git could not run" into "not a repo" |
| `primary_tree` | "Is this path inside the PRIMARY working tree?" (git-dir vs git-common-dir), three-valued; `refuse_if_primary` refuses state-store writes into main's tree and treats `Undetermined` as a refusal |
| `attribution` | Which of a shared working tree's changed files a gate may stop looking at, using the session transcript as identity |
| `repeat` | Session-scoped repeat ledger: a gate does not get to block the same user instruction twice — the second time it must ask |
| `fleet` | The canonical fleet GATE-crate name set (and `BLOCKING_GATES`), replacing hand-copied literals in condukt/tdd |
| `autonomy` | The one autonomy switch, read directly by every plugin (replaces condukt/ctxrot/autoflow's independent switches) |
| `parallel` | Session-wide concurrency ceiling; enforced only by `parallelguard`'s PreToolUse hook |
| `plugin_bin` | Locate a sibling harness plugin's executable without trusting the hook process's ambient `$PATH` |
| `progress` | "Is this holder progressing or merely alive?" engine shared by cross-session staleness reapers (backlog lock, condukt claims) |
| `append` | Atomic single-`write()` append for JSONL sinks (body + newline in one buffer, so parallel `O_APPEND` writers cannot interleave) |
| `discovery` | Machine-scope append-only "discovery record" store so concurrent compass/scout sessions don't duplicate tasks (fail-soft) |
| `lessons` / `retrieval` | Machine-global cross-project lessons store (`~/.lessons/lessons.jsonl`, idempotent by id) and its read-side retrieval-event ledger |
| `code_index` / `text_index` / `callgraph` / `index_store` | Deterministic, purely lexical code indexes (symbols, full text, caller→callee) and the in-process façade over all three — no embeddings, no external API |

## Install (plugin)

Not applicable — harness-core is a dependency, not a plugin. Plugins depend on
it in their `Cargo.toml`; there are no hooks to wire and nothing to install.

## Build

```sh
cargo test
```

Built as part of the workspace (`cargo build --workspace --release`). It has no
committed `bin/` of its own — it is compiled into each plugin's binary.
