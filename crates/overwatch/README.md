# overwatch

**Project-global cross-session execution ledger** for Claude Code, written in Rust.

A distributed multi-session system requires coordination. overwatch keeps a single, authoritative **claim registry** that tracks which keys (tasks, hypotheses, resources) are claimed by which session, with heartbeat-based liveness. When a new session tries to begin work on an already-claimed key, overwatch reports it (exit 1 + skip JSON) so the caller skips duplicate execution.

- On **SessionStart** it injects the project-wide progress view (via `overwatch status`) so the session knows what is in-flight, what is pending, and what has been reaped.
- On **Stop** it refreshes the status view for the human to review.
- Any session can call `overwatch begin --key <k>` to atomically claim a key; if another session already holds a live lease, the request is denied.
- Dedup contract: **exit 1 + skip JSON** = "another session has this"; caller must NOT proceed.
- Liveness is heartbeat-TTL; dead leases (no heartbeat for N seconds) can be manually reaped or auto-expired.

Subscription-native: one Rust binary, two hooks (SessionStart + Stop), no API key.

## What it manages

A per-project registry at `<base>/<project-key>/overwatch/`:

```
leases.json              # current lease snapshot: { "key": { "holder": "session-id", "updated_at": "...", ... } }
events.jsonl             # append-only event log
```

## Install (plugin)

```
/plugin install overwatch@yukineko
```

## Manual install

```sh
cargo install --path .
overwatch install
```

## Commands

```sh
overwatch begin --key <k>                   # try to acquire exclusive lease on key <k>; exit 1 if held by another session
overwatch status                            # show project-wide progress: active leases, events, sessions
overwatch sessions                          # list all sessions (live or dead)
overwatch pause --run <id>                  # pause a run (HOTL-gated)
overwatch resume --run <id>                 # resume a paused run (HOTL-gated)
overwatch reassign --key <k> --to <sid>    # reassign lease from current holder to <sid> (HOTL-gated)
overwatch end --key <k>                     # explicitly release a lease (HOTL-gated)
overwatch reap [--ttl-secs <N>]            # delete dead leases (no heartbeat for N secs) (HOTL-gated)
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

## Update with `/overwatch`

Run `/overwatch` any time to:

- View project-wide registry state (`status`)
- List sessions (`sessions`)
- Control in-flight runs (pause/resume, reassign, reap)

All side-effect commands (pause/resume/reassign/end/reap) are gated by AskUserQuestion before execution.

## Liveness & TTL

Each lease has a `updated_at` timestamp and a configurable heartbeat TTL (default: 5 minutes).

- A session calls `overwatch begin` → timestamp recorded, lease acquired.
- Session keeps the lease alive by calling `overwatch heartbeat --key <k>` periodically.
- If heartbeat updates stop (session crashed, hung, or disconnected), the timestamp stales.
- When TTL expires, `overwatch reap` can delete the dead lease, freeing the key.

## Config

```toml
# overwatch.toml (in project root or per-session override)
enabled = true
# base = "~/.local/share/claude-harnesses"  # default storage location
ttl_secs = 300                              # heartbeat TTL (default: 5 minutes)
```

Disable per-session with `OVERWATCH_DISABLED=1`.

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
