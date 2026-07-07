# daily

> **Daily-once task runner for Claude Code**, written in Rust.
> A `SessionStart` hook that runs **registered tasks at most once per calendar day** and
> feeds a summary of what ran back into the session.

Some checks are worth running regularly but pointless to run on every single session
start. Time-based (cron) firing is fragile across machines and shells, so `daily`
instead gates on the **first session of each calendar day**: the first session pays the
cost, every later session that day skips silently.

It is **subscription-native**: no API key, nothing leaves the machine. The hook is a
deterministic Rust binary; it never calls an LLM. A one-line summary of what ran is
injected as `additionalContext` for the agent — **non-blocking** (it never breaks a turn).

## What it runs

`daily` runs the tasks you **register** in `~/.daily/config.toml` (see
[Configuration](#configuration)). Each `[[task]]` is a `name` + a shell `command`
(optionally pinned to a `dir`), run once per calendar day via `sh -c`.

With **no tasks registered**, a built-in default `security` task runs, preserving the
original behavior:

```sh
cargo deny check advisories bans sources licenses
```

After running the due tasks, `daily` injects a single summary line listing every task
that ran today (both successes and failures):

```
📋 daily: ran security (ok), deploy-check (ok)
⚠️ daily: ran security (fail exit 1: error[…]: advisory …), notes-sync (ok)
```

- exit 0 → `name (ok)`
- exit non-zero → `name (fail exit N: <first salient line>)` (prefers `error`/`warning`/`RUSTSEC` lines)
- command couldn't spawn → `name (error: …)`
- nothing was due today → stays silent

Each task runs in its `dir` (or the session `cwd`) with `$CARGO_HOME/bin` prepended to
`PATH`, so `cargo` subcommands like `cargo-deny` resolve even when `~/.cargo/bin` isn't
on the ambient PATH. `cargo-deny` must still be installed separately
(`cargo install cargo-deny`) for the default security task to do anything.

## The once-per-day gate

The deterministic "ran today?" logic lives in the shared
`harness-core::daily::DailyGuard`:

- State file: `~/.daily/state/<task>-daily.txt` holds the last run's `YYYY-MM-DD`.
- `should_run()` is true only when the stored date ≠ today; `mark_done()` stamps today.
- The gate keys on **calendar day** (local time), not wall-clock hours — so exactly one
  run per day regardless of how many sessions open.

## The hook

| Hook | Event | What it does |
|---|---|---|
| **`daily session-start`** | `SessionStart` (startup/resume/clear) | if no driver is working, for each registered task not yet run today, runs it, stamps `mark_done()`, records it to the report, and injects a summary. Always exits 0. |

## Don't-interfere-while-working gate

`daily` should run maintenance during downtime, not while you're mid-task. Before
running anything, the hook checks whether a `/flow` or `/backlog` **driver is actively
holding the backlog lock** (via `backlog lock status`):

- **A live driver holds the lock** → skip silently. Tasks stay `pending today` and run at
  the next session that starts while no driver is working.
- **Lock is free, or held by a dead (stale) process** → run normally (a crashed driver
  never wedges `daily`).
- **`backlog` isn't installed** → fail-open: no driver framework means no driver, so run.

Set `skip_when_driver_active = false` to always run regardless.

## A failed task still counts as "ran"

Each task's daily gate is stamped **after it runs, regardless of exit code** — a failed
task counts as "ran today" and will not retry until tomorrow. The failure isn't lost: it
is recorded in the report (and surfaced in the session summary) instead of blocking or
looping.

## Report

Every task run is appended as one JSON line to `~/.daily/reports.jsonl`
(`{date, at, task, status, code, detail}`, `status` ∈ `ok`/`fail`/`error`). Inspect it
with `daily report`:

```sh
daily report                 # today's runs
daily report --date 2026-07-01
daily report --last 20       # the 20 most recent entries across all days
```

```
daily report — 2026-07-03
  ✓ security         [10:31:04] ok
  ✗ deploy-check     [10:31:05] fail exit 1: error[…]: advisory …
```

## Configuration

`~/.daily/config.toml` (optional — a missing config means enabled with the default
security task):

```toml
enabled = true                   # set to false to disable all daily tasks
skip_when_driver_active = true   # skip while a /flow or /backlog driver holds the lock (default true)

[[task]]
name = "security"         # unique name; also the once-per-day state key
command = "cargo deny check advisories bans sources licenses"

[[task]]
name = "notes-sync"
command = "git -C ~/notes pull --ff-only"
dir = "/home/me/notes"    # optional; defaults to the session cwd
```

- `enabled` defaults to **true** when omitted; set `enabled = false` to turn the runner
  off entirely.
- `[[task]]` entries are the registered tasks. **Register none** and the built-in
  `security` task runs; register **one or more** and only those run (add `security`
  yourself if you still want it).
- Each `name` must be unique — it is the per-task daily state key.

## Registering tasks

Edit `~/.daily/config.toml` directly, or use the CLI:

```sh
daily add --name notes-sync --command "git -C ~/notes pull --ff-only"
daily add --name build-cache --command "cargo fetch" --dir /path/to/repo
daily list     # show registered tasks and whether each has run today
```

`daily add` appends a `[[task]]` block (preserving existing content/comments) and rejects
a duplicate name.

## Subcommand surface

| Subcommand | Purpose |
|---|---|
| `daily session-start` | SessionStart hook: run each registered task not yet run today (skips if a driver is working) |
| `daily list` | show registered tasks + whether each already ran today |
| `daily report [--date <d>] [--last <n>]` | show the results report of past runs (today by default) |
| `daily add --name <n> --command <c> [--dir <d>]` | register a new daily task in `~/.daily/config.toml` |
| `daily install` | (not yet implemented) — add the hook to `~/.claude/settings.json` manually |

## Install

### As a Claude Code plugin (recommended)

```text
# in Claude Code:
/plugin marketplace add yukineko/claude-harnesses
/plugin install daily@yukineko
```

The hook calls `${CLAUDE_PLUGIN_ROOT}/bin/daily session-start`. `bin/daily` is a POSIX
launcher that selects the right per-platform binary (`bin/daily-<os>-<arch>`); if a host
has no matching binary it exits 0 silently. `cargo-deny` must be installed separately
(`cargo install cargo-deny`) for the security task to do anything.

### Build from source

```sh
scripts/build-plugin-bin.sh
git add bin/ && git update-index --chmod=+x bin/daily bin/daily-*
```

## Platform support

| Host | File | Status |
|---|---|---|
| macOS Apple Silicon | `bin/daily-darwin-arm64` | bundled |
| Linux x86_64 | `bin/daily-linux-x86_64` | bundled |
| macOS Intel | `bin/daily-darwin-x86_64` | bundled |

## Plugin layout

```
.claude-plugin/plugin.json     # plugin manifest
hooks/hooks.json               # SessionStart=session-start → ${CLAUDE_PLUGIN_ROOT}/bin/daily
bin/daily                      # POSIX launcher → daily-<os>-<arch>
bin/daily-<os>-<arch>          # prebuilt binaries
src/main.rs … Cargo.toml       # the Rust crate (uses harness-core::daily::DailyGuard)
```

## Development

```sh
cargo test -p daily
cargo build -p daily
```

## License

MIT
