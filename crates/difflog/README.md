# difflog

**Session diff-log for Claude Code**, written in Rust.

On `SessionStart` it snapshots the current HEAD SHA. On `SessionEnd` it runs
`git diff <start>..HEAD` and writes a structured Markdown log (commits, stat,
file list, bounded diff body) to a local log directory. A bundled `/difflog`
skill can then have an LLM summarise the log into a plain-English narrative
for review or handoff.

> Developer acceptance rate of agent-generated changes is **89%** when the
> agent provides a diff summary, versus **62%** for raw output.
> — Anthropic 2026 Agentic Coding Trends Report

Subscription-native: one Rust binary, two hooks (SessionStart + SessionEnd),
no API key for the deterministic part. The LLM narrative via `/difflog` runs
on your subscription.

## What it writes

`~/.difflog/logs/<YYYY-MM-DD>-<session8>.md`:

```markdown
# difflog — myrepo

- **session**: `abc12345…`
- **started**: 2026-06-23T09:00:00Z
- **ended**:   2026-06-23T09:47:12Z
- **range**:   `f5a2807..3c8d91a`

## Commits

```
3c8d91a add budgetguard: real-time cost gate
a7b2f4c fix: ctxrot preguard deny precedence
```

## Files changed

**Added** (2)
- `crates/budgetguard/src/main.rs`
- `crates/budgetguard/src/gate.rs`

**Modified** (3)
- `Cargo.toml`
- `README.md`
- `crates/harness-core/src/pricing.rs`

## Stat

 crates/budgetguard/src/main.rs | 95 ++++++++++++++++++++++++

## Diff

```diff
+++ b/crates/budgetguard/src/main.rs
…
```
```

## Install (plugin)

```
/plugin install difflog@yukineko
```

## Manual install

```sh
cargo install --path .
difflog install
```

## Commands

```sh
difflog session-start   # SessionStart hook (reads stdin JSON)
difflog session-end     # SessionEnd hook (writes the log)
difflog last            # print the most recent log
difflog list            # list log files, newest first (--limit N, default 10)
difflog init            # write a starter ./difflog.toml (--force to overwrite)
difflog install         # merge hooks into ~/.claude/settings.json (--dry-run)
difflog uninstall       # remove them (--dry-run)
difflog status          # show resolved config
```

## Config

`./difflog.toml` (project), else `~/.difflog/config.toml` — the first that exists
wins; see [`difflog.example.toml`](difflog.example.toml): `enabled`, `log_dir`
(default `~/.difflog/logs`), `diff_body_limit` (bytes of verbatim diff, default
4096; `0` = stat only), `exclude_globs` (built-in default: `Cargo.lock`,
`package-lock.json`, `*.min.js`, `*.min.css`). `DIFFLOG_DISABLE` turns it off.

## Narrative with `/difflog`

After the session, run `/difflog` to get an LLM-generated narrative:

```
/difflog
```

The skill reads the last log (or a specific session with `/difflog --session <id>`)
and produces a one-page human-readable summary — what changed, why, and what
comes next.

## License

MIT
