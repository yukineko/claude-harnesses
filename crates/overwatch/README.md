# overwatch

**Project-global cross-session execution ledger** for Claude Code, written in Rust.

A distributed multi-session system requires coordination. overwatch keeps a single, authoritative **claim registry** that tracks which keys (tasks, hypotheses, resources) are claimed by which session, with heartbeat-based liveness. When a new session tries to begin work on an already-claimed key, overwatch reports it (exit 1 + skip JSON) so the caller skips duplicate execution.

- On **SessionStart** it injects the project-wide progress view (via `overwatch status`) so the session knows which sessions hold live leases, what is pending (PDO hypotheses, backlog), which condukt runs exist, and the compass gap. The helper binaries (`backlog` / `hypothesis` / `condukt` / `compass`) are resolved from the plugin cache first and `PATH` second (`harness_core::plugin_bin::resolve`).
- On **Stop** it refreshes the status view for the human to review.
- Any session can call `overwatch begin --key <k>` to atomically claim a key; if another session already holds a live lease, the request is denied.
- Dedup contract: **exit 1 + skip JSON** = "another session has this"; caller must NOT proceed.
- Liveness is heartbeat-TTL; a lease with no heartbeat for more than 1800s (30 min) is stale. Stale leases are dropped by **every** lease mutation (`begin` / `run` / `end` / `heartbeat` / `reassign` / `reap`), and ignored by `status` / `sessions` / `lease`, so a dead holder never blocks a `begin`. `overwatch reap` just runs that cleanup on its own.

Subscription-native: one Rust binary, two hooks (SessionStart + Stop, both running `overwatch status` with a 5s hook timeout), no API key. The two hooks share a 10-second on-disk status cache (`aggregate::STATUS_CACHE_TTL_SECS`), which is discarded early when `leases.json` was written at or after the second the cache was built, or when its mtime cannot be observed (`aggregate::cache_is_fresh`). The other sources in the view (backlog / hypothesis / condukt / compass) can therefore be up to 10s old.

Beyond the lease registry the same binary also hosts fleet-level gate-violation
recurrence detection, the canary rollout plan/gate helpers, and the unified
human review surface (`review-queue` and its ledgers); see
[Other commands](#other-commands).

### `(none)` vs `(unknown)` in the status view

Every section of `overwatch status` prints `(none)` when it is empty. That reads
as "checked, found nothing", so it is only printed when the source was actually
observed. A source that could **not** be read — an unreadable/corrupt lease
ledger, a helper binary that is absent or exited non-zero, undecodable JSON —
renders `(unknown: <reason>)` instead, and a WARNING listing every such source
goes to stderr.

The distinction matters most in the **Sessions** pane: `(none)` there is the
claim "no other session is live", which CLAUDE.md §8 tells every session not to
assume, and which condukt's main-tree guard reads as a liveness input before
allowing a commit in main's shared working tree. `status --json` therefore also
carries a machine-readable `undetermined` array (`[{source, reason}]`), omitted
entirely when every source was observed.

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
overwatch begin --key <k> --title <t> [--session <sid>] [--scope <csv>] [--done-criteria <text>]   # try to acquire exclusive lease on key <k>; exit 1 + skip JSON if held by another live session or the lease lock is contended
overwatch lease --session <sid> [--json]    # print the live lease (PDO anchor) held by a session; exit 1 (silent) if none
overwatch run --key <k> [--note <text>]     # record a running heartbeat + event for a held lease (see caveats below)
overwatch status [--json]                   # project-wide progress view: Sessions / PDO hypotheses / Backlog / Condukt runs / Compass gap
overwatch sessions [--json]                 # only the Sessions roster: sessions holding a live (non-stale) lease
overwatch pause --run <id>                  # delegate to `condukt state pause --run <id>` and record a control event
overwatch resume --run <id>                 # delegate to `condukt state resume --run <id>` and record a control event
overwatch reassign --key <k> --to <sid>     # RELEASE the lease on <k> and record a "reassign to=<sid>" event (does not create a lease for <sid>)
overwatch end --key <k> --status <s>        # release a lease, recording its terminal status
overwatch reap                              # drop stale leases (no heartbeat within TTL); prints {"reaped": N}
overwatch heartbeat --key <k>               # refresh heartbeat_at for a key; prints {"refreshed": 0|1}
overwatch --protects                        # print what this gate protects, from what, and on what grounds
```

Session id resolution for `begin`/`run`: `--session`, else `$CLAUDE_CODE_SESSION_ID`,
else `pid-<pid>`. The run id comes from `$OVERWATCH_RUN_ID`, else a timestamp-based
`run-…` id.

What each mutation reports when it did **not** do its job (from `src/lease.rs` /
`src/control.rs`):

| command | lease lock contended | key not held | downstream failure |
|---|---|---|---|
| `begin` | exit 1, `{"skipped": true, "reason": "lock_contended"}` | — | — |
| `end` | **non-zero**, lease NOT released | **non-zero** ("nothing was released") | — |
| `run` | **exit 0, nothing written** (silent skip) | exit 0, event still recorded with derived ids | — |
| `heartbeat` | exit 0, `{"refreshed": 0}` | exit 0, `{"refreshed": 0}` | — |
| `reap` | exit 0, `{"reaped": 0}` (indistinguishable from "nothing stale") | — | — |
| `reassign` | exit 0, `control_note` JSON, nothing released | exit 0, `control_note`, intent still recorded | — |
| `pause` / `resume` | — | — | exit 0 with a `control_note` JSON when `condukt` is absent or exits non-zero |

Only `begin` and `end` signal failure through the exit code; callers of the
others must read stdout.

The binary itself does **not** ask for approval before `pause` / `resume` /
`reassign` / `end` / `reap`. "HOTL-gated" means the `/overwatch` skill is
instructed to confirm with AskUserQuestion before running them (see below);
calling the binary directly performs the action immediately.

## The dedup contract

```bash
overwatch begin --key "hypothesis-v2.3"
```

Returns:

- **exit 0**: Lease acquired (or refreshed, if the same session already held it — `claimed_at` is kept). You may proceed.
- **exit 1** + JSON `{ "skipped": true, "holder": { "session_id", "run_id", "heartbeat_at" } }`: Lease is held by another live session. Do NOT proceed.
- **exit 1** + JSON `{ "skipped": true, "reason": "lock_contended" }`: the lease-registry lock could not be taken, so whether the key is free was not checked. Do NOT proceed (retry later).

The caller's job is to check the exit code and skip if it is non-zero. Note
that an internal error (store root unresolvable, `leases.json` unreadable or
corrupt) also exits **1** — `main` returns `anyhow::Result`, so the error goes
to stderr and there is no skip JSON on stdout. Either way the key was not
acquired.

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
  "scope_undeclared": [ { "key": "...", "title": "..." } ],
  "scope_declared": true,
  "possible_duplicate": [ { "key": "...", "title": "...", "similarity": 0.72 } ] }
```

- `scope_overlap` lists other live leases (different key) whose scope overlaps
  this one, via a coarse glob-prefix match. It is a **non-blocking early
  warning** — scope overlap is not necessarily a conflict; condukt's Phase 3.5
  `conflict-check` makes the precise call.
- `scope_undeclared` lists other live leases whose overlap is **undetermined**
  because this lease or that peer declared no scope. Undeclared is not "no
  overlap": a peer missing from both lists has a declared, non-overlapping scope.
- `scope_declared` is `false` when this lease itself declared no scope; then
  every peer is listed in `scope_undeclared` (backlog 56eb3f9a).
- `possible_duplicate` lists other live leases whose title/done_criteria are
  lexically similar (Jaccard ≥ threshold, default 0.6, override via
  `OVERWATCH_DUP_THRESHOLD`), reusing `harness_core::lessons::text_similarity`.
- The lists default to empty arrays and **never change the exit code**.

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

The skill instructs the agent to confirm every side-effect command
(pause/resume/reassign/end/reap) with AskUserQuestion before running it. This
gate lives in the skill prose only; the binary does not enforce it.

## Liveness & TTL

Each lease carries a `heartbeat_at` timestamp (plus `claimed_at`). Staleness is
judged against a **fixed** heartbeat TTL of **1800 seconds (30 minutes)** —
`store::LEASE_TTL_SECS`. The TTL is a compile-time constant, not configurable.

- A session calls `overwatch begin` → `claimed_at`/`heartbeat_at` recorded, lease acquired.
- Session keeps the lease alive by calling `overwatch heartbeat --key <k>` periodically (refreshes `heartbeat_at`).
- If heartbeat updates stop (session crashed, hung, or disconnected), `heartbeat_at` stales.
- Once `now - heartbeat_at > 1800s` the lease is stale: the next lease mutation (any of `begin` / `run` / `end` / `heartbeat` / `reassign` / `reap`) drops it from `leases.json`, and a `begin` on that key by another session succeeds. `overwatch reap` is simply that cleanup with nothing else attached.

## Storage

State lives under `~/.overwatch/<project-key>/overwatch/` (`leases.json` +
`events.jsonl`), keyed per repository on the MAIN worktree root: the main
checkout, every linked `git worktree`, and any subdirectory of either share one
store (so `overwatch status` run from the main checkout sees sessions working in
linked worktrees). If the main worktree root cannot be resolved (unreadable or
unparseable `.git` file, a worktree of a submodule), commands fail rather than
fall back to a per-worktree store. A cwd outside any git repo is keyed on the
cwd itself. There is no config file and no TTL knob;
the only behaviors above are the ones the binary implements today.

## Storage layout

All data lives under the user's home directory, outside the repository:

```
<base>/<project-key>/overwatch/
  leases.json          # snapshot of active leases
  events.jsonl         # append-only ledger
  ...                  # other ledgers in the same dir (violations, review findings,
                       # audit rounds, status cache, ...; see the *_path fns in src/store.rs)
```

- `<base>` is `~/.overwatch` (`$HOME/.overwatch`: `harness_core::config::base_dir("overwatch")`,
  called by `storage_root` in `src/store.rs`). It is not configurable — there is no
  config file (see Storage above).
- `<project-key>` is `<basename>-<fnv1a32 of the canonical path>` of the repository's MAIN worktree root (shared by all its linked worktrees).
- A lease stays in `leases.json` until it is `end`ed, released by `reassign`, or dropped as stale by a later lease mutation; overwatch does not tie it to the Claude Code process, and no overwatch hook removes it when the session exits (callers such as `/flow` release their own leases with `overwatch end`).

## Fleet-level correlated-error detection (gate-violation signature recurrence)

Alongside the lease registry, overwatch records gate-violation events with a
**normalized signature** and escalates a signature as a **systemic issue** when
it recurs N+ times within a time window across tasks/sessions.

```sh
overwatch record-violation --source <gate> --discriminator <d> --task <key> [--symbol <sym>] [--session <sid>] [--detail <text>]
overwatch violations  [--json] [--threshold N] [--window-secs S]   # recurrence stats for every signature
overwatch escalations [--json] [--threshold N] [--window-secs S]   # only the signatures judged systemic
```

- `--source` is one of `blastguard | propguard | specguard | mutategate | donegate | reviewgate | tdd | budgetguard | autoflow | ctxrot`; anything else is an error.
- Signature normalization (`violation::normalize_signature`) and recurrence detection (`violation::detect_recurrence`) are pure functions; the clock is passed in.
- Default policy: 3+ occurrences within a 24h window (`RecurrencePolicy::default`), overridable per call with `--threshold` / `--window-secs`.
- A signature is systemic only when it reaches the threshold **and** spans more than one distinct task or session; one task retrying the same gate is a local retry loop, not systemic.
- Stored in `violations.jsonl` (append-only, separate from `events.jsonl`) under the same `<base>/<project-key>/overwatch/` directory.

## Other commands

`overwatch --help` lists every subcommand with a full description; this is a map.

| group | commands |
|---|---|
| canary rollout | `canary-plan` (pure stage split), `canary-gate` (PROCEED/ROLLBACK JSON; **exit 3** when rollback is advised), `canary-rollback-plan` (pure restore plan), `record-rollback` (called by `scripts/rollout-plugins.sh`; a store-write error is warned on stderr and does not fail the caller) |
| review surface | `review-queue [--json] [--since] [--limit]` (merges five sources — systemic violations, canary rollbacks, AI review findings, condukt escalations and unresolved merge conflicts — into one severity-ordered list [test: crates/overwatch/tests/review_queue_five_sources.rs::review_queue_merges_exactly_five_sources]), `record-finding`, `record-disposition`, `review-metrics`, `compact-findings`, `resolve-merge-conflict`, `reconcile-fixed [--since-ref\|--range\|--last-n] [--dry-run]` (default last 50 commits), `auto-approved [--since] [--sample 5] [--seed 0]` |
| Continuous-Audit | `audit-round record` / `audit-round close`, `audit-metrics [--window 3]` |
| telemetry | `undetermined-metrics [--window-days N]`, `gate-outcomes` (donegate / tdd / reviewgate firing outcomes from `~/.<gate>/state/log.jsonl`) |

Exit-code convention for the read side: when a ledger/journal **cannot be read**
(as opposed to being absent or empty), `review-queue` [test: crates/overwatch/tests/undetermined_e2e.rs::undecodable_violation_ledger_is_not_rendered_as_no_systemic_violations],
`review-metrics` [test: crates/overwatch/tests/undetermined_e2e.rs::review_metrics_with_an_unreadable_disposition_ledger_reports_null_not_zero], `reconcile-fixed` [test: crates/overwatch/tests/undetermined_e2e.rs::reconcile_fixed_with_an_unreadable_disposition_ledger_writes_nothing] and
`gate-outcomes` [test: crates/overwatch/tests/gate_outcomes_cli.rs::undetermined_lines_are_counted_with_correct_line_numbers_and_exit_3] exit **3** rather than
printing a zero/empty answer, and `auto-approved` exits non-zero [test: crates/overwatch/tests/auto_approved_cli.rs::unreadable_journal_does_not_exit_clean].
`reconcile-fixed` remains
fail-soft on the **git** side only: a failing `git log` is read as "0 commits
scanned" (exit 0) [test: crates/overwatch/tests/reconcile_fixed.rs::reconcile_fixed_fail_soft_when_not_a_git_repo].

## PDO positioning

overwatch aggregates pending hypotheses and awaiting-measurement states, exposing them as a **progress view** (`status` output). Each lease key encodes a specific piece of pending data (hypothesis version, design variant, measurement target). By querying the registry, a session learns what prior sessions left unfinished and can decide whether to inherit the work, reassign it, or reap it.

This aligns with the **PDO** (Pending Data Object) pattern: hypothesis-as-pending, measurement-as-awaiting, and overwatch-as-aggregator.

## License

MIT
