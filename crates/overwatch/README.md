Every mutating command (begin, run, end, heartbeat, reap, reassign, pause, resume) exits non-zero when the action did not happen: lease lock contended, lease root unresolvable, key not held (end, reassign), unknown `end --status`, or condukt failing (pause, resume). Exit 0 from `heartbeat`/`reap` still only means the call ran; read the printed `refreshed`/`reaped` count to see what changed.

Do NOT proceed when begin exits 1 with skip JSON (another session has this key).

- Any session can call `overwatch begin --key <k>` to atomically claim a key; if another session already holds a live lease, the request is denied.
- When a new session tries to begin work on an already-claimed key, overwatch reports it (exit 1 + skip JSON) so the caller skips duplicate execution.
- On **SessionStart** it injects the project-wide progress view (via `overwatch status`) so the session knows which sessions hold live leases, what is pending (PDO hypotheses, backlog), which condukt runs exist, and the compass gap.
- The helper binaries (`backlog` / `hypothesis` / `condukt` / `compass`) are resolved from the plugin cache first and `PATH` second.
- Liveness is heartbeat-TTL; a lease with no heartbeat for more than 1800s (30 min) is stale.
- Stale leases are dropped by **every** lease mutation (`begin` / `run` / `end` / `heartbeat` / `reassign` / `reap`), and ignored by `status` / `sessions` / `lease`, so a dead holder never blocks a `begin`. Each drop by a mutation is appended to `events.jsonl` as an `ended` event with status `reaped`.
- `overwatch reap` just runs that cleanup on its own.

Hooks (all with a 5s hook timeout): SessionStart and Stop run `overwatch status`; SessionStart, PostToolUse (matcher `Edit|Write|MultiEdit|NotebookEdit`) and SessionEnd run the hook-only `overwatch presence`, which reads the Claude Code hook payload JSON from stdin and records the session in `presence.json` (session id, cwd, worktree = git toplevel of cwd, transcript path, deduplicated written paths, `last_seen`, `ended`). This registers sessions that never ran `overwatch begin`. Entries whose `last_seen` is over 24h old are pruned on write. A corrupt or unreadable `presence.json` makes the hook exit non-zero with the error on stderr; it is never overwritten with an empty registry.

`status --json` gains `active_sessions` (`session_id`, `cwd`, `worktree`, `written_paths`, `liveness`, `last_seen`). `liveness` is judged from the transcript file's mtime against the 1800s lease TTL: `live` (within), `stale` (older), `unknown` (transcript missing/unreadable), `ended` (SessionEnd seen). An unreadable `presence.json` adds an `undetermined` entry with source `active_sessions`, and the text `== Sessions ==` section prints `(unknown: ...)` instead of `(none)`. That section lists the lease rosters plus each non-ended presence session (id, liveness, worktree/cwd, written paths).

A cached status is discarded once `aggregate::STATUS_CACHE_TTL_SECS` has passed, when `leases.json` or `presence.json` was written at or after the second the cache was built, or when either mtime cannot be observed (`aggregate::cache_is_fresh`).

`(none)` is only printed when the source was actually observed.

Every section of `overwatch status` prints `(none)` when it is empty.

A source that could **not** be read — an unreadable/corrupt lease ledger, an absent helper binary, undecodable JSON — renders `(unknown: <reason>)` instead.

A WARNING listing every such source goes to stderr.

`status --json` also carries a machine-readable `undetermined` array (`[{source, reason}]`), omitted entirely when every source was observed.

```
/plugin install overwatch@yukineko
```

```sh
cargo install --path .
```

There is no `overwatch install` subcommand.

```sh
overwatch begin --key <k> --title <t> [--session <sid>] [--scope <csv>] [--done-criteria <text>]   # try to acquire exclusive lease on key <k>; exit 1 + skip JSON if held by another live session or the lease lock is contended
overwatch lease --session <sid> [--json]    # print the live lease (PDO anchor) held by a session; exit 1 (silent) if none
overwatch run --key <k> [--note <text>]     # record a running heartbeat + event for a held lease
overwatch status [--json]                   # project-wide progress view: Sessions / PDO hypotheses / Backlog / Condukt runs / Compass gap
overwatch sessions [--json]                 # only the Sessions roster: sessions holding a live (non-stale) lease
overwatch presence                           # hook-only: record a hook payload (stdin) in presence.json
overwatch pause --run <id>                  # delegate to `condukt state pause --run <id>` and record a control event
overwatch resume --run <id>                 # delegate to `condukt state resume --run <id>` and record a control event
overwatch reassign --key <k> --to <sid>     # RELEASE the lease on <k> and record a "reassign to=<sid>" event (does not create a lease for <sid>)
overwatch end --key <k> --status <done|abandoned>   # release a lease, recording its terminal status (any other status is rejected, non-zero, lease stays held)
overwatch reap                              # drop stale leases (no heartbeat within TTL); prints {"reaped": N}
overwatch heartbeat --key <k>               # refresh heartbeat_at for a key AND every other lease held by the same session; prints {"refreshed": N} (0 when the key is not held)
overwatch --protects                        # print what this gate protects, from what, and on what grounds
```

Session id resolution for `begin`/`run`: `--session`, else `$CLAUDE_CODE_SESSION_ID`, else `pid-<pid>`.

The run id comes from `$OVERWATCH_RUN_ID`, else a timestamp-based `run-…` id.

`run`, `heartbeat`, `reap`, `reassign` and `end` exit non-zero when the lease lock is contended or the lease root cannot be resolved (nothing is written); `end` additionally leaves the lease NOT released. `reassign` of a key nobody holds, and `pause`/`resume` when `condukt` fails, also exit non-zero...

`end` exits non-zero ("nothing was released") when the key is not held.

The binary itself does **not** ask for approval before `pause` / `resume` / `reassign` / `end` / `reap`.

Calling the binary directly performs the action immediately.

Do NOT proceed when begin exits non-zero.

```bash
overwatch begin --key "hypothesis-v2.3"
```

- **exit 0**: Lease acquired (or refreshed, if the same session already held it — `claimed_at` is kept).
- **exit 1** + JSON `{ "skipped": true, "holder": { "session_id", "run_id", "heartbeat_at" } }`: Lease is held by another live session. Do NOT proceed.
- **exit 1** + JSON `{ "skipped": true, "reason": "lock_contended" }`: a live peer held the lease-registry lock for the whole wait, so whether the key is free was not checked. Do NOT proceed (retry later). A lock I/O failure (the lock dir cannot be created or written) is NOT reported as `lock_contended`: it exits non-zero with an undetermined error on stderr.

Check the exit code and skip if it is non-zero.

A corrupt `leases.json` also exits **1**, with the error on stderr and no skip JSON on stdout.

An unresolvable store root also exits **1**, with stderr saying the root is undetermined.

A lease can carry two optional PDO-anchor fields, `scope` and `done_criteria`.

Both are `#[serde(default)]`, so existing `leases.json` files (written before these fields existed) still load unchanged:

- `scope: Vec<String>` — files/globs this session owns.
- `done_criteria: Option<String>` — the session's definition of done.

`overwatch begin` takes two optional flags, `--scope` and `--done-criteria`, to set them:

```bash
overwatch begin --key "hypothesis-v2.3" --title "cache the router table" \
  --scope "crates/fugu-router/src/**,crates/condukt/src/schedule.rs" \
  --done-criteria "all fugu-router tests green, no clippy warnings"
```

On a successful `begin` (exit 0) an advisory JSON summary is printed to stdout:

```json
{ "scope_overlap": [ { "key": "...", "title": "...", "scope": ["..."] } ],
  "scope_undeclared": [ { "key": "...", "title": "..." } ],
  "scope_declared": true,
  "possible_duplicate": [ { "key": "...", "title": "...", "similarity": 0.72 } ] }
```

- `scope_overlap` lists other live leases (different key) whose scope overlaps this one.
- `scope_undeclared` lists other live leases whose overlap is **undetermined** because this lease or that peer declared no scope.
- Undeclared is not "no overlap": a peer missing from both lists has a declared, non-overlapping scope.
- `scope_declared` is `false` when this lease itself declared no scope; then every peer is listed in `scope_undeclared` (backlog 56eb3f9a).
- `possible_duplicate` lists other live leases whose title is lexically similar (Jaccard similarity; default threshold 0.6, override via `OVERWATCH_DUP_THRESHOLD`).
- The lists default to empty arrays and **never change the exit code**.

```bash
overwatch lease --session <sid> [--json]
```

Prints the live lease (PDO anchor) held by `<sid>` — the most-recently-claimed one if the session holds several — or exits 1 silently if none.

Each lease carries a `heartbeat_at` timestamp (plus `claimed_at`). Staleness is judged against a **fixed** heartbeat TTL of **1800 seconds (30 minutes)** — `store::LEASE_TTL_SECS`. The TTL is a compile-time constant, not configurable.

- Once `now - heartbeat_at > 1800s` the lease is stale: the next lease mutation (any of `begin` / `run` / `end` / `heartbeat` / `reassign` / `reap`) drops it from `leases.json`, and a `begin` on that key by another session succeeds.
- `overwatch reap` is simply that cleanup with nothing else attached.

State lives under `~/.overwatch/<project-key>/overwatch/` (`leases.json` + `presence.json` + `events.jsonl`), keyed per repository on the MAIN worktree root.

The main checkout, every linked `git worktree`, and any subdirectory of a linked worktree share one store (so `overwatch status` run from the main checkout sees sessions working in linked worktrees).

If the main worktree root cannot be resolved (an unparseable `.git` file), `begin`, `lease` and `end` fail rather than fall back to a per-worktree store.

A cwd outside any git repo is keyed on the cwd itself.

- `<base>` is `~/.overwatch` (`$HOME/.overwatch`).
- `<project-key>` is `<basename>-<fnv1a32 of the canonical path>` of the repository's MAIN worktree root (shared by all its linked worktrees).
- A lease stays in `leases.json` until it is `end`ed, released by `reassign`, or dropped as stale by a later lease mutation; overwatch does not tie it to the Claude Code process, and no overwatch hook removes it when the session exits (the SessionEnd hook only marks the session `ended` in `presence.json`).

Alongside the lease registry, overwatch records gate-violation events with a **normalized signature** and escalates a signature as a **systemic issue** when it recurs N+ times within a time window across tasks/sessions.

```sh
overwatch record-violation --source <gate> --discriminator <d> --task <key> [--symbol <sym>] [--session <sid>] [--detail <text>]
overwatch violations  [--json] [--threshold N] [--window-secs S]   # recurrence stats for every signature
overwatch escalations [--json] [--threshold N] [--window-secs S]   # only the signatures judged systemic
```

- `--source` accepts `blastguard | propguard | specguard | mutategate | donegate | reviewgate | tdd | budgetguard | autoflow | ctxrot`; an unknown token does not parse as a source.
- Recurrence detection (`violation::detect_recurrence`) takes the clock as an argument.
- Default policy: 3+ occurrences within a 24h window (`RecurrencePolicy::default`), overridable per call with `--threshold` / `--window-secs`.
- A signature is systemic only when it reaches the threshold **and** spans more than one distinct task or session; one task retrying the same gate is a local retry loop, not systemic.
- Stored in `violations.jsonl` (append-only, separate from `events.jsonl`) under the same `<base>/<project-key>/overwatch/` directory.

`overwatch --help` lists every subcommand with a full description.

- canary rollout: `canary-plan` (pure stage split), `canary-gate` (PROCEED/ROLLBACK JSON; **exit 3** when rollback is advised), `canary-rollback-plan` (pure restore plan), `record-rollback` (records a rollback event in `rollbacks.jsonl`)
- review surface: `review-queue [--json] [--since] [--limit]` (merges five sources — systemic violations, canary rollbacks, AI review findings, condukt escalations and unresolved merge conflicts — into one severity-ordered list), `record-finding`, `record-disposition`, `review-metrics`, `compact-findings`, `resolve-merge-conflict`, `reconcile-fixed [--since-ref\|--range\|--last-n] [--dry-run]` (default last 50 commits), `auto-approved [--since] [--sample 5] [--seed 0]`
- Continuous-Audit: `audit-round record` / `audit-round close`, `audit-metrics [--window 3]`
- telemetry: `undetermined-metrics [--window-days N]`, `gate-outcomes` (donegate / tdd / reviewgate firing outcomes from `~/.<gate>/state/log.jsonl`)

Exit-code convention for the read side: when a ledger/journal **cannot be read** (as opposed to being absent or empty), `review-queue`, `review-metrics`, `reconcile-fixed` and `gate-outcomes` exit **3**, and `auto-approved` exits non-zero. The git history counts as such an input: when `git log` cannot be read (not a repository, git missing, an unknown ref), `reconcile-fixed` exits 3 with `commits_scanned: null` and `git log` in `undetermined_sources`, and `review-metrics` reports a `null` stale-undisposed count instead of 0.

MIT
