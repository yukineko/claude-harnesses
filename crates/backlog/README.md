# backlog

Cross-project task queue for Claude Code — a durable queue of work items, tagged
by cycle type, that outlives any one session and any one repo. backlog surfaces
pending work the moment a session opens (a **SessionStart** hook injects it as
context) and exposes a small binary for adding, picking, and resolving items.
The lock→pick→`/condukt`→done driver loop lives in `/flow`; the bundled
`/backlog` skill is a thin alias to it.

Subscription-native: a skill, one hook, and a bundled Rust binary, **no API
key**. The SessionStart hook is fail-soft — malformed stdin is logged to stderr
and skipped, and the hook always exits 0 so a turn is never broken.

## What it does

The `backlog` binary owns the queue and its exclusive run-lock:

| Subcommand | What it does |
|---|---|
| `add` | Append a task (`--title`, `--project`, `--tag`, `--priority p0/p1/p2`, `--notes`, `--weight`, `--force`) |
| `list` | List the store's tasks, filterable by `--tag` / `--status` (vocabulary is `pending`/`done`/`failed` — not `open`, that's `hypothesis`'s vocabulary). For a repo store `--project` is an assertion, not a filter — see **Scope** |
| `next` | Print the next highest-priority pending task as JSON |
| `done <id>` | Mark a task done |
| `fail <id>` | Mark a task failed (`--reason`); defers re-run by 2 days |
| `edit <id>` | Update a task's title / tags / notes / status |
| `session-start` | SessionStart hook: inject pending tasks as context |
| `install` / `uninstall` | Wire/remove the SessionStart hook in `~/.claude/settings.json` |
| `lock {acquire,release,status}` | Manage the `~/.backlog/run.lock` exclusive lock |

The lock is how concurrent sessions serialize: a `/flow` driver acquires it
before draining the queue, and other sessions back off when `lock status`
reports an active holder.

The bundled `/backlog` skill is a thin entry point over queue/state
operations (`list` / `next` / `done` / `fail` / `lock`), passed a subcommand as
its argument. To drain the whole queue automatically, use **`/flow`** instead —
it's the superset driver (lock acquire → pick item → `/condukt` → done/fail →
lock release), also wired for the compass freshness gate, budgetguard, and
fugu-router model selection.

## Why it exists

Sessions are volatile: close the conversation and "the thing I meant to do next"
goes with it, and once you start work in another repo the items you parked in a
different project drop out of view entirely. Leaning on chat history or memory,
pending tasks quietly get lost. backlog closes that failure mode — once an item
is queued it survives across sessions and repos, the SessionStart hook re-injects
pending work as context wherever you open next, and the exclusive run-lock keeps
concurrent sessions from draining the queue at the same time and colliding.

## Install (plugin)

Installed via the plugin marketplace, the bundled `/backlog` skill is available
immediately. The SessionStart hook is registered by running `backlog install`,
which merges a `SessionStart` group into `~/.claude/settings.json` (idempotent,
ownership-marked) so pending work shows up at every session open.

## Standalone (cargo)

```sh
cargo install --path .
backlog add --title "Fix X" --project "$PWD" --priority p1   # queue an item
backlog list --status pending                                # see the queue
backlog next                                                 # pick the next item
backlog done <id>            # resolve it
backlog fail <id> --reason "blocked"   # defer it 2 days
backlog lock status         # who holds the run-lock
backlog install             # merge the SessionStart hook into settings.json
backlog uninstall           # remove it again
```

`install`/`uninstall` accept `--dry-run` to print the resulting settings without
writing.

## Duplicate-task rejection (content hashkey)

`add` derives a **content hashkey** from the title and project (title trimmed →
Unicode NFKC → lowercased → runs of whitespace collapsed to one space →
leading/trailing punctuation stripped, folded with `project` via 64-bit FNV-1a
into a 16-hex-digit key) and rejects the add when either is true (`done` never
blocks a re-add of the same title — requeuing the same work later is
legitimate):

- an existing `pending` or `failed` task already has that hashkey, or
- `condukt` is on `PATH` and `condukt state is-claimed --hashkey <h>` exits 0
  (another live session holds a claim on it). If `condukt` is missing or errors
  for any other reason, this check fails soft to "no claim" — a missing/broken
  `condukt` never fails the `add`.

Either rejection can be bypassed intentionally with `backlog add --force`.

Each element of `backlog list --json` carries a `hashkey` field (computed from
title + project, not stored) so upstream drivers like `/flow` can gate on
`condukt state is-claimed` for free.

## Scope: one queue per repo, and the file IS the scope

The store resolves per repo — `<repo root>/.backlog/tasks.toml`, a tracked file
that merges like any other — so it holds that repo's tasks and nothing else.
Two consequences, both deliberate:

- **Reads apply no project filter.** Every task in the file is in scope,
  whichever checkout wrote it. Filtering rows by the writing checkout's absolute
  path split one repo's queue into one queue per machine. In this repo's own
  store, counting pending-or-failed tasks by label:

  | measurement point | macOS label | WSL label | `C:/…` label |
  |---|---|---|---|
  | `bb046648` (2026-08-20) | 258 | 66 | 5 |
  | `89feaddb` (2026-08-20) | 265 | 70 | 5 |

  A `list` from the WSL checkout matched only the WSL label and silently dropped
  every other row — all of them tasks of the very repo whose file it was
  reading. Re-measure with:

  ```sh
  python3 -c "import collections,tomllib; d=tomllib.load(open('.backlog/tasks.toml','rb')); \
    print(collections.Counter(t['project'] for t in d['task'] if t['status'] in ('pending','failed')).most_common())"
  ```

  `--project` therefore becomes an **assertion** about which store you meant —
  naming this repo changes nothing, naming a different one is an error, not a
  filtered listing. `--all` is accepted and is the default here.
- **A cwd with no repo above it has no store.** `add`/`list`/`next` refuse with
  the reason instead of falling back to the cross-project `~/.backlog`. That
  fallback is how a process running in a tempdir wrote its fixtures into the
  real queue, and how a read from such a cwd was answered with another
  project's work. To opt into a shared store anyway, pin `store_dir` in
  `~/.backlog/config.toml`; a pinned store may hold several projects, so there
  `--project` stays an ordinary filter.

The `project` field is still recorded — it says which checkout filed the task,
and `list` marks it `[project unresolved: …]` when that label was a guess — but
it no longer decides what you can see.

## Two files: live queue and done file

A store is two sibling files: `tasks.toml` (the live queue) and
`tasks.done.toml` (every terminal row — `done` or `cancelled`). In general the
done file of `<dir>/<stem>.toml` is `<dir>/<stem>.done.toml`.

- **Reads see the union.** Every reader goes through one loader that returns
  both files' rows. `list --status done`, `done <id>`, `edit <id>`, `sync`, the
  duplicate guard and the near-duplicate scan all see rows that live only in the
  done file. The duplicate guard decides exactly as before: a `done` row does
  not block a re-add, wherever it is stored.
- **Terminal wins, and terminal is final.** When an id appears in both files
  (for example a git merge re-introduced a completed task as `pending` in
  `tasks.toml`), the terminal row is the one returned, once; `next` never hands
  it out. `edit --status pending|failed` on a `done`/`cancelled` task is refused
  with a non-zero exit, and `fail` on one is refused too; the task stays
  terminal. (`done` on an already-done task is still an idempotent success.)
- **A bad done file is an error, not an empty one.** A missing
  `tasks.done.toml` just means nothing was completed yet. One that exists but
  cannot be read or parsed makes `list`, `next`, `add`, `done`, `edit`, `fail`
  and `sync` exit non-zero with an error naming that file — never a listing that
  silently lacks the done rows, and never a write that would overwrite them. The
  SessionStart hook injects a "store UNREADABLE" notice instead of an empty
  queue.
- **Writes partition.** Every save writes the terminal rows to the done file and
  the rest to `tasks.toml`. Rows already in the done file keep their position
  (rewritten in place only if their content changed, e.g. a `sync` stamp or a
  notes edit); newly finished rows are appended, so a completion's git diff is a
  pure append. The done file is written first and is not rewritten when nothing
  in it changed. Each file is replaced atomically (fsync'd temp + rename); a
  crash between the two writes leaves the row in both files, which
  terminal-wins resolves.
- **Old binaries migrate on the next write.** A `done`/`cancelled` row still in
  `tasks.toml` (written by a backlog older than 0.3.7) is listed normally and
  moves to the done file the next time any command saves the store under its
  lock.

## Cross-checkout claim exclusion (`next --claim`)

The store follows the checkout on purpose: `<repo root>/.backlog/tasks.toml`,
where a linked worktree counts as its own root (CLAUDE.md §8 forbids a worktree
writing the main tree's tracked file). Two checkouts of one project therefore
hold two files that diverge — and the claim's mutual exclusion used to be a
lockfile beside the store, i.e. per checkout, so both handed out the SAME task.

`next --claim` now takes a second, WIDER lock first, and records the claim in a
machine-global ledger keyed by project IDENTITY, not by store location:

- ledger: `~/.backlog/claims/<project-slug>.json` (`<project-slug>` is the same
  FNV-1a project hash `backlog lock` uses; a linked worktree normalizes to its
  main working tree, so every checkout of one project shares one ledger)
- lock order, never inverted: `~/.backlog/claims/<slug>.lock` (project-wide),
  then `<store>.tasks.toml.lock` (this checkout)
- an entry stops excluding after 1h (`CLAIM_STALE_SECS`), matching the store's
  own stale-claim reclaim, so a dead claimant cannot strand a task everywhere;
  entries are kept for 7 days for a human reading the file

Every undetermined condition on this path **refuses the claim** and exits
non-zero with the reason on stderr — never `no pending tasks` on exit 0, which
a driver reads as "there is no work". That covers: the ledger directory not
being creatable, the ledger lock not being acquired, the ledger being
unreadable/unparseable/unwritable, the tasks-file lock not being held, and a
project identity that cannot be resolved. A refusal is not an empty queue.

## Build

```sh
cargo test
```

The committed `bin/backlog-*` binaries are what the plugin ships, so end users
need neither cargo nor an API key. Rebuild and recommit them (the workspace
builds with `cargo build --workspace --release`) when you change behavior the
skill or hook relies on.
