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

Before spawning, `task.command` is checked against the same `blastguard` detector
the PreToolUse hook uses. A command blastguard **denies** (destructive) is refused —
never spawned — and reported as `name (blocked: <reason>)` in the summary/report
instead of running. An `Ask` verdict (a construct blastguard cannot analyse and
refuses to guess about) blocks too: no human is present at SessionStart to answer it,
so `run_task` in `src/main.rs` hardens `Ask` to `Deny` (`Decision::hardened`) and the
command is reported as `name (blocked: <reason>)` without being spawned.

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
- exit non-zero → `name (fail exit N: <first salient line>)` (prefers `error`/`warning`/`RUSTSEC` lines);
  killed by a signal (no exit code) → `name (fail killed: …)`
- command couldn't spawn → `name (error: …)`
- command refused by blastguard before spawn → `name (blocked: <reason>)`
- nothing was due today → stays silent

Each task runs in its `dir` (or the session `cwd`). `dir` is canonicalized and must be
the session `cwd` or a directory **under** it: a `dir` that does not exist, cannot be
canonicalized, or resolves outside the session `cwd` (e.g. `../../etc`, or an absolute
path elsewhere) is **refused**: the task is not spawned (it is never run in the session
`cwd` as a substitute) and is reported as `name (blocked: task dir … )`.
`$CARGO_HOME/bin` is prepended to `PATH`, so `cargo` subcommands like `cargo-deny` resolve even when `~/.cargo/bin` isn't
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
- **`backlog` isn't installed** (observed absent from both the plugin cache and `$PATH`)
  → no driver framework means no driver, so run.
- **No answer could be obtained** — `backlog` could not be located (plugin cache
  unreadable, or a `$PATH` copy that cannot be spawned), could not be started,
  `backlog lock status` exited non-zero, or its output is neither `none` nor JSON →
  treated as **a driver is active**: this session's run is skipped. A JSON
  `kind: undetermined` answer from backlog is likewise read as active.

Set `skip_when_driver_active = false` to always run regardless.

## A failed task still counts as "ran"

Each task's daily gate is stamped **after it runs, regardless of exit code** — a failed
task counts as "ran today" and will not retry until tomorrow. The failure isn't lost: it
is recorded in the report (and surfaced in the session summary) instead of blocking or
looping.

## Report

Every task run is appended as one JSON line to `~/.daily/reports.jsonl`
(`{date, at, task, status, code, detail}`, `status` ∈ `ok`/`fail`/`error`/`blocked`). If an append fails, a
`warning: daily log: … this record is LOST` line goes to stderr. Inspect it
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
dir = "tools"             # optional; must resolve to the session cwd or below — else the task is refused (blocked)
```

- `enabled` defaults to **true** when omitted; set `enabled = false` to turn the runner
  off entirely.
- `[[task]]` entries are the registered tasks. **Register none** and the built-in
  `security` task runs; register **one or more** and only those run (add `security`
  yourself if you still want it).
- Each `name` must be unique — it is the per-task daily state key.
- An **unparseable** config is ignored with a stderr warning and treated as the default
  (enabled, nothing registered → the built-in `security` task runs).

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
| `daily install` | (not yet implemented — installs nothing, prints a notice and exits 1) — add the hook to `~/.claude/settings.json` manually |

## Install

### As a Claude Code plugin (recommended)

```text
# in Claude Code:
/plugin marketplace add yukineko/claude-harnesses
/plugin install daily@yukineko
```

The hook calls `${CLAUDE_PLUGIN_ROOT}/bin/daily session-start`. `bin/daily` is a POSIX
launcher that selects the right per-platform binary (`bin/daily-<os>-<arch>`). If a host
has no matching binary it does **not** stay silent: for `session-start` it emits a
`systemMessage` + `additionalContext` saying today's tasks did NOT run and their result
is UNKNOWN (exit 0); any other verb exits 127 with the reason on stderr. `cargo-deny` must be installed separately
(`cargo install cargo-deny`) for the security task to do anything.

### Build from source

```sh
scripts/build-plugin-bin.sh daily   # stages bin/daily-<os>-<arch> for the host
```

Platform-tagged binaries are **never committed** (`.gitignore` ignores
`crates/*/bin/*-<os>-<arch>`); only the `bin/daily` launcher is tracked. Local deploy
builds from source via `scripts/rollout-plugins.sh`.

## Platform support

The launcher resolves `bin/daily-<os>-<arch>` (`darwin`/`linux`/`windows` ×
`arm64`/`x86_64`). No platform binary is committed; a host has one only after it is
built there (`scripts/build-plugin-bin.sh` / `scripts/rollout-plugins.sh`).

## Plugin layout

```
.claude-plugin/plugin.json     # plugin manifest
hooks/hooks.json               # SessionStart=session-start → ${CLAUDE_PLUGIN_ROOT}/bin/daily
bin/daily                      # POSIX launcher → daily-<os>-<arch>
bin/daily-<os>-<arch>          # locally built binaries (git-ignored)
src/main.rs … Cargo.toml       # the Rust crate (uses harness-core::daily::DailyGuard)
```

## Development

```sh
cargo test -p daily
cargo build -p daily
```

## License

MIT
