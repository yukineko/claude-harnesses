# overwatch

**Project-global cross-session execution ledger** for Claude Code, written in Rust.

A distributed multi-session system requires coordination. overwatch keeps a single, authoritative **claim registry** that tracks which keys (tasks, hypotheses, resources) are claimed by which session, with heartbeat-based liveness. When a new session tries to begin work on an already-claimed key, overwatch reports it (exit 1 + skip JSON) so the caller skips duplicate execution.

- On **SessionStart** it injects the project-wide progress view (via `overwatch status`) so the session knows what is in-flight, what is pending, and what has been reaped.
- On **Stop** it refreshes the status view for the human to review.
- Any session can call `overwatch begin --key <k>` to atomically claim a key; if another session already holds a live lease, the request is denied.
- Dedup contract: **exit 1 + skip JSON** = "another session has this"; caller must NOT proceed.
- Liveness is heartbeat-TTL; dead leases (no heartbeat for 1800s / 30 min) are reaped on demand via `overwatch reap` (HOTL-gated).

Subscription-native: one Rust binary, two hooks (SessionStart + Stop), no API key.

## What it manages

A per-project registry at `~/.overwatch/<project-key>/overwatch/`:

```
leases.json              # current lease snapshot: { "<key>": { "key", "title", "session_id", "run_id", "claimed_at", "heartbeat_at", "scope", "done_criteria" } }
events.jsonl             # append-only event log
```

## Install (plugin)

```
/plugin install overwatch@yukineko
```

## Manual install

```sh
cargo install --path .
```

There is no `overwatch install` step: the plugin ships a bundled binary and the
registry/store is created lazily on first use. Installing is a plain copy — no
setup command to run.

## Commands

```sh
overwatch begin --key <k> --title <t> [--session <sid>] [--scope <csv>] [--done-criteria <text>]   # try to acquire exclusive lease on key <k>; exit 1 + skip JSON if held by another live session
overwatch lease --session <sid> [--json]    # print the live lease (PDO anchor) held by a session; exit 1 (silent) if none
overwatch run --key <k> [--note <text>]     # record a running heartbeat + event for a held lease (fail-soft if the key isn't held)
overwatch status [--json]                   # show project-wide progress: active leases, events, sessions
overwatch sessions [--json]                 # list all sessions (live or dead)
overwatch pause --run <id>                  # pause a run (HOTL-gated)
overwatch resume --run <id>                 # resume a paused run (HOTL-gated)
overwatch reassign --key <k> --to <sid>    # reassign lease from current holder to <sid> (HOTL-gated)
overwatch end --key <k> --status <s>        # release a lease, recording its terminal status (HOTL-gated)
overwatch reap                              # delete dead leases (no heartbeat within TTL) (HOTL-gated)
overwatch heartbeat --key <k>               # reset the TTL for a key (keep the lease alive)
```

## The dedup contract

```bash
overwatch begin --key "hypothesis-v2.3"
```

Returns:

- **exit 0**: Lease acquired. You may proceed.
- **exit 1** + JSON `{ "skip": "reason", ... }`: Lease is held by another live session. Do NOT proceed.

The caller's job is to check the exit code and skip if it's 1.

## PDO session anchor (`--scope` / `--done-criteria`)

A lease can carry two optional PDO-anchor fields that record what the session is
responsible for and what "done" means for it. Both are `#[serde(default)]`, so
existing `leases.json` files (written before these fields existed) still load
unchanged:

- `scope: Vec<String>` — files/globs this session owns (same vocabulary as
  condukt's `touched_files`). Empty = not yet fixed (e.g. still investigating);
  such leases are excluded from overlap detection to avoid false positives.
- `done_criteria: Option<String>` — the session's definition of done.

`overwatch begin` gained two optional flags to set them; omitting both keeps the
exact prior behavior (and the exit-code contract is unchanged):

```bash
overwatch begin --key "hypothesis-v2.3" --title "cache the router table" \
  --scope "crates/fugu-router/src/**,crates/condukt/src/schedule.rs" \
  --done-criteria "all fugu-router tests green, no clippy warnings"
```

On a successful `begin` (exit 0) an advisory JSON summary is printed to stdout:

```json
{ "scope_overlap": [ { "key": "...", "title": "...", "scope": ["..."] } ],
  "possible_duplicate": [ { "key": "...", "title": "...", "similarity": 0.72 } ] }
```

- `scope_overlap` lists other live leases (different key) whose scope overlaps
  this one, via a coarse glob-prefix match. It is a **non-blocking early
  warning** — scope overlap is not necessarily a conflict; condukt's Phase 3.5
  `conflict-check` makes the precise call.
- `possible_duplicate` lists other live leases whose title/done_criteria are
  lexically similar (Jaccard ≥ threshold, default 0.6, override via
  `OVERWATCH_DUP_THRESHOLD`), reusing `harness_core::lessons::text_similarity`.
- Both default to empty arrays and **never change the exit code**.

### Reading a session's anchor

```bash
overwatch lease --session <sid> [--json]
```

Prints the live lease (PDO anchor) held by `<sid>` — the most-recently-claimed
one if the session holds several — or exits 1 silently if none. This is the read
path for ctxrot's anchor re-injection (re-anchoring the session's title /
done_criteria back into its context; see `docs/DESIGN-pdo-session-anchor.md`
§4.3).

## Update with `/overwatch`

Run `/overwatch` any time to:

- View project-wide registry state (`status`)
- List sessions (`sessions`)
- Control in-flight runs (pause/resume, reassign, reap)

All side-effect commands (pause/resume/reassign/end/reap) are gated by AskUserQuestion before execution.

## Liveness & TTL

Each lease carries a `heartbeat_at` timestamp (plus `claimed_at`). Staleness is
judged against a **fixed** heartbeat TTL of **1800 seconds (30 minutes)** —
`store::LEASE_TTL_SECS`. The TTL is a compile-time constant, not configurable.

- A session calls `overwatch begin` → `claimed_at`/`heartbeat_at` recorded, lease acquired.
- Session keeps the lease alive by calling `overwatch heartbeat --key <k>` periodically (refreshes `heartbeat_at`).
- If heartbeat updates stop (session crashed, hung, or disconnected), `heartbeat_at` stales.
- Once `now - heartbeat_at > 1800s`, `overwatch reap` can delete the dead lease, freeing the key.

## Storage

State lives under `~/.overwatch/<project-key>/overwatch/` (`leases.json` +
`events.jsonl`), keyed per repository. There is no config file and no TTL knob;
the only behaviors above are the ones the binary implements today.

## Storage layout

All data lives in version-controlled locations (or user-owned caches):

```
<base>/<project-key>/overwatch/
  leases.json          # snapshot of active leases
  events.jsonl         # append-only ledger
```

- `<base>` defaults to `~/.local/share/claude-harnesses` (configurable in overwatch.toml).
- `<project-key>` is derived from the project name (e.g., `claude-harnesses`).
- Leases are **not** persisted across full reapers; they are session-scoped ephemeral records.

## PDO positioning

overwatch aggregates pending hypotheses and awaiting-measurement states, exposing them as a **progress view** (`status` output). Each lease key encodes a specific piece of pending data (hypothesis version, design variant, measurement target). By querying the registry, a session learns what prior sessions left unfinished and can decide whether to inherit the work, reassign it, or reap it.

This aligns with the **PDO** (Pending Data Object) pattern: hypothesis-as-pending, measurement-as-awaiting, and overwatch-as-aggregator.

## License

MIT
