`budgetguard gate` is wired to the Claude Code **Stop** hook; on each stop it:

1. takes this session's per-model token usage — from gauge's persisted
   session record when it is at least as new as the transcript, otherwise by
   re-parsing the transcript (JSONL), including sub-agent spend;
2. estimates the USD cost using the built-in pricing table;
3. updates a local **daily ledger** (`~/.budgetguard/state/ledger.json`) with
   this session's latest cost (not a sum over stops), under a ledger lock: while
   another session holds the lock the ledger is not written and the day total
   is unmeasured; the day key is the **local** date;
4. checks the session total and the day's cumulative total against configured
   limits;
5. **within limits** → exits 0, the stop proceeds;
   **warn threshold** → emits `{"additionalContext":"…"}` (advisory only, no block);
   **block threshold** → emits `{"decision":"block","reason":"…"}` asking the
   agent to save and commit before the turn ends.
   Thresholds are inclusive (spend at or above the limit); block beats warn.

It also reports **cache-hit health** from gauge's session record (to stderr): once `input + cache_read` reaches `[cache].min_tokens` (default
200000), a hit rate below `[cache].min_rate` (default 0.5; out-of-range values
fall back to the default) is promoted to an advisory `additionalContext`
warning when nothing louder is being said. It never blocks.

A blocking stop also records an overwatch violation event.

### Limits when none is configured

A limit of `0` turns off that one axis. A gate with **no** limit at all
(no config file, or every `[session]` / `[daily]` warn/block limit `0`) is not
"no limit". budgetguard sets the limits itself:

- **derived**: if the ledger holds at least 7 past days (today is excluded),
  the limits come from that history, with Tukey outlier removal (Q1/Q3 by
  linear interpolation, fence = Q3 + 1.5 × IQR). Daily block = the fence over
  per-day totals. Daily warn = p90 of the days at or below the fence;
- **default**: with a shorter history, the built-in limits apply: daily warn
  $1336 / block $1871. These were computed on 2026-10-19 from the
  maintainer's ledger with the same method;
- the **session axis** gets no derived warn and no derived block. The session
  fence (the Tukey fence over every past per-session entry; the default is
  $306) is reported as `session_fence_usd` by `check` / `status --json`, for
  information only. A session
  warn/block applies only when the config sets one;
- a ledger that exists but cannot be read or parsed, or that holds a
  non-finite or negative cost, is **undetermined**: the gate blocks and
  `check`/`status` exit 2. It is never silently replaced by the defaults.

`check` and `status --json` report where the limits came from as
`limits_source`: `config` | `derived` | `default`.

Allowed (exit 0, no decision):

- A re-entry with `stop_hook_active` (a previous Stop hook already blocked) —
  an over-budget session is blocked once, then allowed to stop.
- `BUDGETGUARD_DISABLE=1` → hook is a no-op, even when the config file cannot be parsed.
- `enabled = false` in the config → hook is a no-op for a parseable Stop
  payload (unparseable stdin still blocks).

*Cannot determine* is **not** treated as within budget:

- A config file that exists but cannot be read or parsed → **block**
  (`config-undetermined`), whatever the limits were meant to be.
- Session spend that cannot be measured (for example a sub-agent transcript that
  could not be read, or a model with no known rate) → **block** if any block limit is armed, else **warn** if
  any warn limit is armed.
  This includes a `transcript_path` that does not exist or is a directory, and
  a Stop payload whose `session_id` or `transcript_path` is empty.
- stdin that is not a hook payload at all → **block** (`{"decision":"block"}` on
  stdout, exit 0) in every case, with or without a limit armed: the gate cannot
  process its input, and an exit 1 would be a non-blocking error that lets the
  stop through. This holds even with `enabled = false`. The block is bounded:
  at most 3 consecutive unparseable stops block, and the 4th is allowed with
  a loud `systemMessage` saying the gate gave up. The streak is kept per
  `CLAUDE_CODE_SESSION_ID` (or under one global key) in
  `<state_dir>/unparseable-streak/`, and a parsed stop resets it. A marker that
  cannot be read or written allows the stop loudly. `BUDGETGUARD_DISABLE=1` bypasses it entirely.
- A day total that cannot be measured (ledger lock held by another session, or
  a corrupt `ledger.json`, which is preserved untouched) → block/warn per the
  armed *daily* limits; a session already over its own block limit still gets
  that precise reason.
- Unmeasured totals print as `unknown` on stderr, never as `$0`.

## Install (plugin)

The marketplace catalog at the root of this repo lists budgetguard:

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install budgetguard@yukineko
```

```sh
cargo install --path .
budgetguard init          # write a starter ./budgetguard.toml
budgetguard install       # merge the Stop hook into ~/.claude/settings.json
```

Drop a `budgetguard.toml` in the project root, or `~/.budgetguard/config.toml`
for a global default. `budgetguard.example.toml` is a working starter
config with `enabled`, a commented `state_dir`, `[session]`, `[daily]` and a
commented `[[price]]`.

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

```sh
budgetguard gate           # Stop hook (reads stdin JSON, emits decision)
budgetguard status         # resolved config + today's spend
budgetguard status --json  # machine-readable budget pressure:
                           #   {day_usd, daily_warn_usd, daily_block_usd, pressure};
                           #   an unreadable config prints {error, undetermined:true}
                           #   and exits 2;
                           #   so does a corrupt ledger.json
budgetguard init [--force]          # write a starter budgetguard.toml; without --force an existing file is left unchanged
budgetguard install [--dry-run]     # merge the hook into ~/.claude/settings.json
budgetguard uninstall [--dry-run]   # remove it
```

Uses `harness-core`'s built-in rate table:

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

License: MIT.
