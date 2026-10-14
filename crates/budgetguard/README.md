# budgetguard

> 🌐 **English** ・ [日本語](README.ja.md)

**Real-time cost budget gate for Claude Code**, written in Rust.

gauge observes; budgetguard *controls*. On every Stop it reads the session
transcript, computes the estimated USD cost (same pricing table as gauge), and
blocks the stop if session or daily limits are exceeded — feeding the overage
notice back to the agent so it can save its work and wrap up gracefully.

It is the cost-control sibling of the verification gates:

| Gate | When | Asks |
|---|---|---|
| `donegate` | on Stop | does it build and pass? |
| `reviewgate` | on Stop | is the code quality acceptable? |
| **`budgetguard`** | **on Stop** | **is the cost within budget?** |

No API key. The transcript is already on disk; budgetguard reads it
deterministically. Nothing leaves the machine.

## How it works

`budgetguard gate` is wired to the Claude Code **Stop** hook. On each stop it:

1. takes this session's per-model token usage — from gauge's persisted
   session record when it is at least as new as the transcript, otherwise by
   re-parsing the transcript (JSONL), including sub-agent spend;
2. estimates the USD cost using the built-in pricing table;
3. updates a local **daily ledger** (`~/.budgetguard/state/ledger.json`) with
   this session's latest cost, serialized against concurrent sessions by a
   ledger lock (the day key rolls over at **local** midnight);
4. checks the session total and the day's cumulative total against configured
   limits;
5. **within limits** → exits 0, the stop proceeds;
   **warn threshold** → emits `{"additionalContext":"…"}` (advisory only, no block);
   **block threshold** → emits `{"decision":"block","reason":"…"}` asking the
   agent to save and commit before the turn ends.
   Thresholds are inclusive (spend at or above the limit); block beats warn.

It also reports **cache-hit health** from gauge's session record (to stderr,
every stop): once `input + cache_read` reaches `[cache].min_tokens` (default
200000), a hit rate below `[cache].min_rate` (default 0.5; out-of-range values
fall back to the default) is promoted to an advisory `additionalContext`
warning when nothing louder is being said. It never blocks.

A blocking stop also records an overwatch violation event.

### What is allowed, and what is not

Allowed (exit 0, no decision):

- No config file at all, or `[session]` / `[daily]` limits all `0` → every
  stop is allowed (a limit of 0 means "no limit").
- No transcript data to price.
- A re-entry with `stop_hook_active` (a previous Stop hook already blocked) —
  an over-budget session is warned once, then allowed to stop.
- `BUDGETGUARD_DISABLE=1` → hook is a no-op (checked before everything else).

*Cannot determine* is **not** treated as within budget:

- A config file that exists but cannot be read or parsed → **block**
  (`config-undetermined`), whatever the limits were meant to be.
- Session spend that cannot be measured (e.g. a sub-agent transcript that
  could not be read) → **block** if any block limit is armed, else **warn** if
  any warn limit is armed.
- A day total that cannot be measured (ledger lock held by another session, or
  a corrupt `ledger.json`, which is preserved untouched) → block/warn per the
  armed *daily* limits; a session already over its own block limit still gets
  that precise reason.
- A panic inside the gate fails **closed** (`harness_core::gate::run::run_guarded`
  blocks and surfaces the crash), bounded by `stop_hook_active`.
- Unmeasured totals print as `unknown` on stderr, never as `$0`.

## Why

Left running, an LLM agent's cost climbs quietly. An observation tool like
gauge tells you *how much you spent* after the fact — but that is a post-hoc
report; it does not stop the turn that is running. A runaway loop or an
unexpectedly expensive session keeps growing until someone looks at a dashboard.

budgetguard fills that gap. By turning cost into a Stop gate, it enforces
**hard** per-session and per-day limits and hands the overage back to the agent
itself so it can land safely. It is the harness for when you need control, not
just observation.

## Install (plugin)

Via the marketplace (the catalog lives at the root of this repo, `yukineko/claude-harnesses`):

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install budgetguard@yukineko
```

**Subscription-native** — it runs on a single Stop hook plus the bundled Rust
binary; no `ANTHROPIC_API_KEY`, no extra install.

## Manual install (from source)

```sh
cargo install --path .
budgetguard init          # write a starter ./budgetguard.toml
budgetguard install       # merge the Stop hook into ~/.claude/settings.json
```

## Configuration

Drop a `budgetguard.toml` in the project root, or `~/.budgetguard/config.toml`
for a global default. See `budgetguard.example.toml` for all options.

```toml
[session]
warn_usd  = 0.50
block_usd = 2.00

[daily]
warn_usd  = 5.00
block_usd = 20.00
```

The project file, if present, is used *instead of* the home file (they are not
merged). There is no trust gate: a repo-shipped `budgetguard.toml` is honored
as-is. Other keys: `enabled`, `state_dir` (default `~/.budgetguard/state`),
`[[price]]`, and `[cache]` (`min_rate`, `min_tokens`).

## Commands

```sh
budgetguard gate           # Stop hook (reads stdin JSON, emits decision)
budgetguard status         # resolved config + today's spend
budgetguard status --json  # machine-readable budget pressure (feeds fugu-router):
                           #   {day_usd, daily_warn_usd, daily_block_usd, pressure};
                           #   an unreadable config prints {error, undetermined:true}
                           #   and exits 2
budgetguard init [--force]          # write a starter budgetguard.toml
budgetguard install [--dry-run]     # merge the hook into ~/.claude/settings.json
budgetguard uninstall [--dry-run]   # remove it
```

## Pricing

Uses `harness-core`'s built-in rate table (same as gauge):

| Family | Input $/1M | Output $/1M |
|---|---|---|
| Fable / Mythos | 10 | 50 |
| Opus | 5 | 25 |
| Sonnet | 3 | 15 |
| Haiku | 1 | 5 |

Override any model with a `[[price]]` stanza in `budgetguard.toml` (`pattern`
is a case-insensitive substring of the model id). A model with usage that is
neither in the table nor matched by an override has an **unknown** cost, not
$0: the gate treats the session as unmeasured (`cost-undetermined`). Declare a
genuinely free model with `input = 0.0` / `output = 0.0`. A `[[price]]` row
with an empty `pattern` (it would match every model) or a rate that is not a
finite non-negative number makes the config unusable, exactly like a parse
error.

## License

MIT
