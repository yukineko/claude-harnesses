On each `Stop`, gauge aggregates the session's JSONL transcript into one record
per session (`<store>/sessions/<session_id>.json`).
The record is rewritten each turn so the latest write holds the whole session to date:

- input / output / **cache-write (5m & 1h)** / **cache-read** tokens, per model
- number of model requests (turns) and per-tool call counts (Bash, Edit, …)
- first/last timestamp → session duration

The **estimated cost** is priced when a record is read, from a built-in price table (overridable per model).

`gauge report` then rolls these up by project, model, and day.
`gauge subagents` reads each sub-agent (`Task`) transcript **live**
(`<session>/subagents/agent-<id>.jsonl`) and attributes cost per `Task`
invocation.

`GAUGE_DISABLE=1` turns recording off entirely.

As a Claude Code plugin, the bundled `bin/gauge` is invoked by `hooks/hooks.json`
(`${CLAUDE_PLUGIN_ROOT}/bin/gauge record`). Install standalone:

```sh
cargo install --path .
gauge install        # merges the Stop hook into ~/.claude/settings.json
gauge uninstall      # removes it again
```

```sh
gauge report                       # totals + breakdown by project / model / day
gauge report --project myrepo      # filter by project
gauge report --since 2026-06-01    # filter by day
gauge session                      # details for the most recent session
gauge session --session <ID> --json  # a specific session as {session_id, cost_usd, models, agents}
gauge subagents                    # per-sub-agent (Task) cost, read live from transcripts
gauge subagents --session <ID> --json  # JSON array; time fields are null when unknown
gauge status                       # resolved config, store path, session count
gauge init [--force]               # write a starter ./gauge.toml (--force overwrites)
```

`gauge install` / `gauge uninstall` accept `--dry-run`.

```sh
gauge config show                  # print the currently registered window, if any
```

Built-in rates (USD per 1M tokens, input/output): **Opus** 5/25 · **Sonnet**
3/15 · **Haiku** 1/5 · **Fable/Mythos** 10/50. Cache writes bill at 1.25× input
(5-minute TTL) or 2× (1-hour TTL); cache reads at 0.1× input. An unrecognized
model with usage has an **unknown** cost, not $0: gauge prints `unknown`.
In JSON the cost is `cost_usd: null` plus `cost_undetermined`.
budgetguard treats the session as unmeasured.
Declare a genuinely free model with an explicit `input = 0.0` /
`output = 0.0` override.
Override or add a model in `gauge.toml`:

```toml
[[pricing]]
pattern = "opus"   # substring match against the model id; first match wins
input = 5.0
output = 25.0
```

Cost is recomputed on every report, so editing the
table re-prices history.

Project `./gauge.toml` over `~/.gauge/config.toml` over built-in defaults (first
file that exists wins). See `gauge.example.toml`.
Store defaults to
`~/.gauge/store`.

A project `./gauge.toml` that exists but cannot be read or parsed (including any unknown
key, such as a misspelled `[[pricng]]` or `enabeld`), or a `[[pricing]]` row
missing `pattern`/`input`/`output` or with a rate that is not a finite
non-negative number, is not silently replaced by defaults: the read commands (`report`, `session`,
`subagents`, `status`) print a message naming the file and exit 1, and the
`record` hook records nothing and says why on stderr.

```sh
cargo build --release
cargo test
```
