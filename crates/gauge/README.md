# gauge

Local **LLMOps telemetry** for Claude Code. A single Rust binary that doubles as
a **Stop** hook: after every turn it re-reads the session transcript and records
cumulative **token usage, cache hits, tool calls, latency, and estimated cost**
to a local store — then `gauge report` rolls it up by project, model, and day.

Observability for your own agent runs, the same way the rest of the toolkit
works: subscription-native (one hook + a bundled binary), **no API key**, and
**nothing leaves the machine**.

## What it records

On each `Stop`, gauge aggregates the session's JSONL transcript into one record
per session (`<store>/sessions/<session_id>.json`), rewritten each turn so the
latest write holds the whole session to date:

- input / output / **cache-write (5m & 1h)** / **cache-read** tokens, per model
- number of model requests (turns) and per-tool call counts (Bash, Edit, …)
- first/last timestamp → session duration
- **estimated cost**, from a built-in price table (overridable per model)

`gauge report` then rolls these up by project, model, and day; `gauge subagents`
instead reads each sub-agent (`Task`) transcript **live**
(`<session>/subagents/agent-<id>.jsonl`) and attributes cost per `Task`
invocation, so a caller like condukt can record real per-task cost rather than
one lumped session total.

The hook only *observes*: it runs panic-guarded and always exits 0, so bad
stdin, a missing transcript, or an unwritable store record nothing rather than
break the turn. `GAUGE_DISABLE=1` turns recording off entirely.

## Install

As a Claude Code plugin, the bundled `bin/gauge` is invoked by `hooks/hooks.json`
(`${CLAUDE_PLUGIN_ROOT}/bin/gauge record`). Standalone:

```sh
cargo install --path .
gauge install        # merges the Stop hook into ~/.claude/settings.json
gauge uninstall      # removes it again
```

## Use

```sh
gauge report                       # totals + breakdown by project / model / day
gauge report --project myrepo      # filter by project
gauge report --since 2026-06-01    # filter by day
gauge session                      # details for the most recent session
gauge subagents                    # per-sub-agent (Task) cost, read live from transcripts
gauge status                       # resolved config, store path, session count
gauge init                         # write a starter ./gauge.toml
```

Example:

```
gauge — 2 セッション / 310 turns
合計コスト $54.08  ·  トークン 49.32M (49,321,292)

プロジェクト別
  myrepo                      $54.08    49.32M  2 sess

モデル別
  claude-opus-4-8             $54.08  in 47.5k / out 493.0k / cache 48.78M

日別 (直近14日)
  2026-06-20     $54.08    49.32M
```

## Pricing

Built-in rates (USD per 1M tokens, input/output): **Opus** 5/25 · **Sonnet**
3/15 · **Haiku** 1/5 · **Fable/Mythos** 10/50. Cache writes bill at 1.25× input
(5-minute TTL) or 2× (1-hour TTL); cache reads at 0.1× input. An unrecognized
model contributes 0. Override or add a model in `gauge.toml`:

```toml
[[pricing]]
pattern = "opus"   # substring match against the model id; first match wins
input = 5.0
output = 25.0
```

Cost is recomputed from stored token counts on every report, so editing the
table re-prices history.

## Config

Project `./gauge.toml` over `~/.gauge/config.toml` over built-in defaults (first
file that exists wins). See `gauge.example.toml`. Store defaults to
`~/.gauge/store`.

## Build

```sh
cargo build --release        # binary at target/release/gauge
cargo test
make bins                    # refresh bundled bin/gauge-darwin-* and -linux-x86_64
```

The Linux artifact is cross-compiled from macOS with cargo-zigbuild (no Docker),
pinned to an old glibc floor so it runs across distros.
