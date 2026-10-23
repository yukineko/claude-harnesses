The **SessionStart** hook injects pending work as context when a session opens.

To drain the whole queue automatically, use `/flow` instead.

## Use the subcommands

- `add` — Append a task (`--repro-test`); see **File only with a reproducing test**
- `list` — List the store's tasks, filterable by `--tag` / `--status` (`pending` / `done` / `failed` / `cancelled` / `claimed`; `open` is not a status; `claimed` is derived) and `--json`. For a repo store `--project` is an assertion, not a filter — see **Scope**
- `next` — Print the next highest-priority pending task that carries a reproduced repro, as JSON (`--claim` reserves it — see below)
- `done <id>` — Mark a task done **with evidence**: `--test CMD --red-rev REV` (`--reason fixed|already-fixed|obsolete`), `--doc-only COMMIT`, or `--duplicate-of ID`. Exactly one of those forms is required. A bare `done <id>` is refused
  - The RED run is `--red-rev` in a detached temp worktree with HEAD's test files overlaid (every test file for `cargo test`, the named files for pytest, the script for `bash`/`sh`), so a test added with the fix exists there. A refusal because the test passes at `--red-rev` says so. When the fix itself lives in a test file, that overlay copies the fix in. `--red-overlay PATH` (repeatable) narrows the overlay to the named repro side. It only narrows: each path must already be in the default overlay. A test that passes at `--red-rev` is still refused. The narrowed set is recorded as `closure.red.overlay`.
- `fail <id>` — Mark a task failed (`--reason`). It defers the re-run by 2 days
- `cancel <id> --reason R` — Close as "decided not to do it": terminal, never requeued, recorded as a `discard` closure, no test needed
- `audit-closures` — Read-only classification of every terminal row's closure evidence (`--json`); never writes the store
- `overlap` — Group queued tasks by declared `touched_files` scope (advisory; undeclared scopes listed separately)
- `edit <id>` — Update a task's title / tags / notes / status
- `sync` — Reconcile the store against its GitHub issues (one-way, local wins). It is report-only unless `--apply`. `--only both|create|close|body` selects which arms run. `--limit` truncates the plan to that many tasks
- `session-start` — SessionStart hook: inject pending tasks as context
- `install` / `uninstall` — Wire/remove the SessionStart hook in `~/.claude/settings.json` (standalone use)
- `merge-driver` — git merge driver for the task store — see below
- `lock {acquire,release,status,heartbeat,probe}` — Manage the per-project exclusive lock `~/.backlog/locks/<project-slug>.lock`. `probe` judges the holder's progress (git HEAD + transcript growth). Only a `stalled` holder may be reaped by `acquire`. While the lock is held (active or heartbeat-stale), `next --claim` from any session other than the holder is refused with a non-zero exit. A claimer without `CLAUDE_CODE_SESSION_ID` is refused too, and so is any claim while the lock file is unreadable
- `driver {register,heartbeat,unregister,status}` — Announce/observe a project's queue drivers WITHOUT excluding them. `heartbeat` also refreshes this session's `next --claim` leases


## Install as a plugin

The plugin's `hooks/hooks.json` registers the SessionStart hook (`backlog session-start`).
The registered hook's timeout is 10s.
`backlog install` is for standalone (cargo) use: it merges a `SessionStart` group into `~/.claude/settings.json` (idempotent, ownership-marked).

## Store writes need a linked worktree

`add` / `done` / `fail` / `edit` / `cancel` / `sync --apply` are **refused** (non-zero exit,
nothing written) when the store lies inside the *primary* working tree of a git repo, so run them
from a linked worktree (`git worktree add …`).
If git cannot say which kind of checkout this is, `add` is refused too.
`list` works in the primary tree.
`next --claim` is allowed there too and leaves `git status` clean.
The SessionStart hook skips its requeue in the primary tree and says so.

## Run standalone from cargo

```sh
cargo install --path .
backlog add --title "Fix X" --project "$PWD" --priority p1 \
  --repro-test "bash tests/repro_fix_x.sh"                  # file an item ONLY if the committed test fails at HEAD
backlog list --status pending                                # see the queue
backlog next                                                 # pick the next item
backlog done <id> --test "cargo test -p x" --red-rev <rev>  # resolve it (RED at rev, GREEN at HEAD)
backlog done <id> --doc-only <commit>   # or: a doc-only ancestor commit
backlog done <id> --duplicate-of <id>   # or: a duplicate
backlog fail <id> --reason "blocked"   # defer it 2 days (it comes back as pending)
backlog cancel <id> --reason "won't do"   # discard it (terminal, never requeued; recorded as a `discard` closure, no test needed)
backlog lock status         # who holds the project's lock
backlog install             # merge the SessionStart hook into settings.json
backlog uninstall           # remove it again
```

`install`/`uninstall` accept `--dry-run` to print the resulting settings without
writing.

## File only with a reproducing test

- `add` writes a task **only** when `--repro-test CMD` fails behaviourally at HEAD; the task lands
  `pending` with the reproduced repro recorded.
- Any other outcome is refused with a non-zero exit and nothing written to the store: no `--repro-test`,
  not reproduced, undetermined.
- `next` / `next --claim` never hand out a row without a reproduced repro.
- A store holding a row with status `unconfirmed` or `needs-ruling` is refused by `list` and `next`,
  naming the row.
- `backlog audit-open [--json]` audits OPEN rows (pending / failed) as `has-test` / `no-test` /
  `undetermined`, read from the recorded repro.
- `audit-open` runs no test.
- Determined results go to the ledger `~/.backlog/audit-open/<project-slug>.json`.
- A determined row is skipped on later runs, even if a repro is added later.
- An undetermined row is re-examined on the next run.
- An unreadable ledger fails the command.

`add` derives a **content hashkey** from the title and project (title trimmed →
Unicode NFKC → lowercased → runs of whitespace collapsed to one space →
leading/trailing punctuation stripped, folded with `project` via 64-bit FNV-1a
into a 16-hex-digit key), and rejects the add when either of the following is true. `done` never
blocks a re-add of the same title:

- an existing `pending` or `failed` task already has that hashkey, or
- `condukt` (looked up in the plugin cache first, then on `PATH`) is found and `condukt state is-claimed --hashkey <h>` exits 0
  (another live session holds a claim on it). Exit 1 counts as "not claimed"
  only when stdout also carries `"claimed": false`. Any other outcome — `condukt`
  not found or failing to spawn,
  an exit code other than 0/1 (for example 3 = claim registry unreadable),
  exit 0/1 without the matching `claimed` field,
  exit 1 with `"claimed": true` (exit code and field disagree), termination by signal, or no
  answer in time — means the claim check could not be made, and the `add` is
  **refused** with the reason (a possible duplicate of a live claimed task
  cannot be ruled out). "Cannot determine" is never read as "not claimed".

Each rejection can be bypassed intentionally with `backlog add --force`.

Each element of `backlog list --json` carries a `hashkey` field (computed from
title + project, not stored).

The store resolves per repo — `<repo root>/.backlog/tasks.toml`.

- **Reads apply no project filter:** every task in the file is in scope,
  whichever checkout wrote it.

  - In this repo's own store, pending-or-failed tasks by label: macOS 265, WSL 70, `C:/…` 5.

  Run this to re-measure:

  ```sh
  python3 -c "import collections,tomllib; d=tomllib.load(open('.backlog/tasks.toml','rb')); \
    print(collections.Counter(t['project'] for t in d['task'] if t['status'] in ('pending','failed')).most_common())"
  ```

  `--project` therefore becomes an **assertion** about which store you meant —
  naming this repo changes nothing, naming a different one is an error, not a
  filtered listing. `--all` is accepted.
- **A cwd with no repo above it has no store:** `add` and `list` refuse with
  the reason instead of falling back to the cross-project `~/.backlog`. `next` refuses the same way. To opt into a shared store anyway, pin `store_dir` in
  `~/.backlog/config.toml`; a pinned store may hold several projects, so there
  `--project` stays an ordinary filter.

The `project` field is still recorded — it says which checkout filed the task,
and `list` marks it `[project unresolved: …]` when that label was a guess — but
it no longer decides what you can see.

A store is two sibling files: `tasks.toml` (the live queue) and
`tasks.done.toml` (every terminal row — `done` or `cancelled`).
The done file of `.backlog/tasks.toml` is `.backlog/tasks.done.toml`.

- **Reads see the union:** readers return both files' rows.
  `list --status done`, `done <id>` and `edit <id>` see rows that live only in the done file.
  `sync` sees them too, and the near-duplicate scan does not offer a terminal row as a peer.
  The duplicate guard decides exactly as before: a `done` row does
  not block a re-add, wherever it is stored.
- **Terminal wins, and terminal is final:** when an id appears in both files
  (for example a git merge re-introduced a completed task as `pending` in
  `tasks.toml`), the terminal row is the one returned, once; `next` never hands
  it out. `edit --status pending|failed` on a `done`/`cancelled` task is refused
  with a non-zero exit, and `fail` on one is refused too; the task stays
  terminal. `done` on an already-done task is still an idempotent success.
- **A bad done file is an error, not an empty one:** a missing
  `tasks.done.toml` just means nothing was completed yet. One that exists but
  cannot be read or parsed makes `list` and `next` exit non-zero with an error naming that file — never a listing that silently lacks the done rows. `add`, `done`, `edit`, `fail` and `sync` exit non-zero the same way, and never write a store that would overwrite the done rows. The
  SessionStart hook injects a "store UNREADABLE" notice instead of an empty
  queue.
- **Writes partition:** every save writes the terminal rows to the done file and
  the rest to `tasks.toml`. Newly finished rows are appended, so a completion's git diff is
  a pure append. Rows already in the done file keep their position, and are rewritten in place only if their content changed (for example a `sync` stamp or a notes edit). The done file is written first and is not rewritten when nothing
  in it changed. `tasks.toml` is replaced atomically (fsync'd temp + rename). A
  crash between the two writes leaves the row in both files, which
  terminal-wins resolves.
- **Old binaries migrate on the next write:** a `done`/`cancelled` row still in `tasks.toml` is listed normally and
  moves to the done file the next time any command saves the store.

`.gitattributes` marks both `.backlog/tasks.toml` and `.backlog/tasks.done.toml` `merge=backlog`.
`backlog merge-driver <base> <ours> <theirs>` (git's `%O %A %B`) merges them per
task id instead of per line:

- the union of ids; a task deleted on one side (relative to base) and left
  unchanged on the other stays deleted; a task changed on only one side takes
  that side;
- the same id edited differently on both sides is a **conflict**: no side is
  picked, the driver exits non-zero. The same holds for edit/delete and for the same new id added with different content. `<ours>` is left untouched and git marks
  the file unmerged for a human;
- an input that does not parse is a non-zero exit with `<ours>` untouched;
- a row without a string `id`, a duplicate id in one input, or an unknown top-level key is also a non-zero exit
  with `<ours>` untouched. A clean result is written atomically.

Output is rendered with the same writer `backlog` uses when every row
round-trips through the task schema, so merging a store with itself is
byte-identical; rows carrying fields this binary does not know are kept (the
generic TOML rendering is used then).

Run this to configure the driver per repository:

```sh
backlog merge-driver --install   # run inside the repo
```

This sets the repo's `merge.backlog.driver` to run `backlog merge-driver %O %A %B`.

The store follows the checkout on purpose: `<repo root>/.backlog/tasks.toml`,
where a linked worktree counts as its own root.

`next --claim` takes a second, WIDER lock first, and records the claim in a
machine-global ledger keyed by project IDENTITY, not by store location:

- lock order, never inverted: `~/.backlog/claims/<slug>.lock` (project-wide),
  then `<store>.tasks.toml.lock` (this checkout)
- ledger: `~/.backlog/claims/<project-slug>.json`; a linked worktree normalizes to its main working tree, so every checkout of one project shares one ledger. `<project-slug>` is the same FNV-1a project hash `backlog lock` uses
- an entry (a **lease**) stops excluding once its holder has not been seen
  alive for 1h (`CLAIM_STALE_SECS`), so a dead claimant cannot strand a task
  everywhere. Entries are kept for 7 days for a human reading the file
- "seen alive" = the newer of the claim time and the holder's last lease
  heartbeat. `next --claim` records the claiming session
  (`CLAUDE_CODE_SESSION_ID`), and `backlog driver heartbeat --session-id S
  --project P` also refreshes S's still-live leases in P's ledger, so a driver
  heartbeating at least hourly keeps its claims however long the task takes
  (backlog 486e83ca). An already-expired lease is not revived. A lease with no
  session (an older binary's entry, or a claim outside a session) ages by claim
  time alone. If the leases cannot be refreshed (for example the ledger is unreadable) the heartbeat exits non-zero saying so; a
  malformed heartbeat field makes the ledger unparseable, which refuses claims
  rather than treating the holder as dead

Every undetermined condition on this path **refuses the claim** and exits
non-zero with the reason on stderr — never `no pending tasks` on exit 0, which
a driver reads as "there is no work". That covers: the ledger lock not being acquired, the ledger being
unreadable/unparseable, the tasks-file lock not being held, and a
project identity that cannot be resolved. A refusal is not an empty queue. The ledger directory not being creatable and the ledger being unwritable are refused the same way.

### The lease never touches the tracked store; `claimed` is derived

A claim writes **only** the ledger. The tracked `.backlog/tasks.toml` is not
modified by `next --claim`, and SessionStart's requeue never rewrites rows on account of
claims — so claiming leaves the git worktree clean. `claimed` is a DERIVED status, decided per row from its stored status and its live lease (if any):

- a `pending` row with a live lease is shown `claimed`;
- a `failed` row with a live lease is shown `claimed` only if the lease was
  taken strictly AFTER the row's `updated_at` (a re-claim of an older failed
  task). A row updated at or after the claim — the claimant ran `fail` — is
  not shown `claimed`, and `--status failed` selects it;
- a `done`/`cancelled` row is never shown `claimed`.

This rule is display-only. Exclusion from `next` / `next --claim` is decided
by the live lease alone, so a task its claimant just failed is not listed `claimed`
yet is still not handed out until the lease ages out.

- `next --claim` prints the task with `"status": "claimed"`, as before.
- plain `next` skips leased tasks; `list` (text and `--json`) shows the
  derived status above. The status filter applies to the derived view: `--status claimed` selects the
  rows shown `claimed`. `--status pending` omits leased tasks.
- a row that an older binary persisted as `status = "claimed"` is read as
  `pending`; only a live lease excludes it. It is rewritten as `pending` the
  next time an unrelated command saves the store.
- `done` and `fail` do not release a lease. `edit --status pending` does not release a lease either. A leased
  non-terminal task stays excluded until the lease ages out (1h after the
  holder was last seen alive).
- `list`, plain `next` and `next --claim` locate the ledger the same way (the
  project identity, see above); outside any repo that identity is the
  canonical cwd. If the identity cannot be resolved (for example a dangling worktree
  `.git` link) or the ledger exists but cannot be read or parsed, `list` and
  plain `next` **refuse** (non-zero exit, reason on stderr, nothing on
  stdout) rather than show a possibly-claimed task as pending.
- the store-divergence check does not count leased tasks as queued work.

## Build and test

```sh
cargo test
```

Only the `bin/backlog` launcher is tracked in git.
It execs the per-platform `backlog-<os>-<arch>` binary placed beside it in the deployed plugin, and exits
non-zero (not 0 with empty output) when no build exists for the host.
