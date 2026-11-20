# flow-watch — spec (user request 2026-11-20)

User: 「止まってましたっていうのは恥ずかしいこと…処理していってほしいという要望に対して…改善をもとめる」「それはflowなりの仕組みでいれて」「LLMのメモリーなどあてにならない」.
So the anti-stall measure must be a deterministic mechanism shipped by the flow plugin, not a note the
driver LLM is supposed to remember. `/flow` arms it with the Monitor tool; every stdout line is an event
the driver must act on.

## Command

`crates/flow/bin/flow-watch` (executable launcher, git index mode 100755; Python 3 stdlib only, run as
`python3 -I`-safe code). Usage:

```
flow-watch --session <SESSION_ID> --project <DIR>
           [--relay-queue <path>]      # default ~/.blastguard/relay/<SESSION_ID>.jsonl
           [--condukt-state <dir>]     # default: every ~/.condukt/state/*/ dir
           [--poll-secs 5] [--state-secs 60] [--heartbeat-secs 600]
           [--relay-stall-secs 120] [--task-stall-secs 1200] [--max-secs 1700]
```

Environment `FLOW_WATCH_CONDUKT` / `FLOW_WATCH_BACKLOG` override the `condukt` / `backlog` executables
(for tests). One event per line, flushed immediately. Event grammar (first word is the kind):

- `RELAY <id> agent=<agent_id> cwd=<cwd>` — a relay row with `"event":"queued"` whose id has no later
  `executed` / `dismissed` row. Emitted once per id, including rows already pending at startup.
- `RELAY-STALL <id> age=<secs>s` — a still-unadjudicated relay older (by its `ts`) than
  `--relay-stall-secs`; re-emitted at most once per `--relay-stall-secs`.
- `STATE <run_id> <task>=<status> ...` — see "Run tracking (v2)" below.
- `TASK-STALL <run_id> <task> status=running idle=<secs>s` — see "Run tracking (v2)" below.
- `WATCH-UNOWNED <run_id> label=<terminal_label>` — a new run whose owner session is undetermined;
  see "Run ownership" under "Run tracking (v2)" below.
- `HEARTBEAT ok` is NOT printed (silence on success). On failure: `HEARTBEAT-FAILED <which> rc=<rc>`
  where which ∈ {condukt, backlog}. Heartbeats run at start and every `--heartbeat-secs`:
  `condukt state heartbeat --run flow-<SESSION>` and
  `backlog driver heartbeat --session-id <SESSION> --project <DIR>`. A missing executable is a failure
  (rc=127), never silence.
- `WATCH-ERROR <what>: <reason>` — fail-closed visibility (CLAUDE.md §3): an unreadable / unparsable
  relay queue line, an unreadable run file, etc. A queue file that does not exist yet is NOT an error
  (no relays yet) — but a path that exists and cannot be read is.
- `EXPIRED re-arm` — printed as the last line when `--max-secs` elapses; then exit 0. (Monitor caps at
  30 min, so the driver re-arms on this line.)

Exit codes: 0 after EXPIRED; 2 on bad arguments (usage on stderr).

## SKILL.md change

Add a step right after Step 2 (driver registration), before the loop: arm `flow-watch` with the Monitor
tool (`timeout_ms` 1800000), re-arm it whenever `EXPIRED re-arm` or the Monitor expiry notice arrives,
and react to each event immediately:
RELAY / RELAY-STALL → run the relayed command byte-for-byte in the main session now and SendMessage the
result to the agent (or `askrelay dismiss` with the reason if it cannot run);
TASK-STALL → inspect that run/task (state, worktree log, the driver agent) and unblock it now —
do not wait for a task notification; STATE → sink finished items;
HEARTBEAT-FAILED / WATCH-ERROR → report verbatim and fix. Step 4 stops the Monitor (TaskStop).
Waiting for a background-agent notification is not a substitute for these events.

## Version

flow 0.2.34 → 0.2.35 in `crates/flow/.claude-plugin/plugin.json` and the `flow` entry of
`.claude-plugin/marketplace.json` (flow has no Cargo.toml).

## Run tracking (v2, 2026-11-21) — supersedes v1's run-file globbing

Observed on the first live arm of v1 (flow 0.2.36): globbing `~/.condukt/state/*/run-*.json` read 9082
state dirs of every project, treated per-run siblings (`run-<id>.decomposition.json`,
`run-<id>.checkpoints.json`) as run files (spurious `WATCH-ERROR … task entry without id/status`),
emitted `STATE` for months-old all-`verified` runs (`verified` was not in the terminal set) and
`TASK-STALL` for abandoned runs of other sessions (`idle=9393790s`). v2 replaces the source and the
scoping:

- **Source = condukt itself.** Every `--state-secs`, run `condukt state list-tasks` with cwd =
  `--project` (executable overridable by `FLOW_WATCH_CONDUKT`). Its stdout is a JSON array of objects
  with `run_id`, `task_id`, `status` (only non-terminal tasks of open runs; condukt's own project
  scoping and terminal semantics). flow-watch no longer reads condukt state files or globs state dirs;
  `--condukt-state` is removed (passing it is a bad argument, exit 2).
  A non-zero exit, a missing executable (rc=127), non-JSON stdout, or a row without string
  `run_id`/`task_id`/`status` → `WATCH-ERROR condukt list-tasks: <reason>` (once per distinct reason),
  and that poll changes nothing (no STATE, no stall bookkeeping) — it is never read as "no tasks".
- **Watched runs.** Runs present in the FIRST successful poll of a fresh watch are the baseline and are
  NOT watched (they belong to other drivers or predate this one): no STATE / TASK-STALL for them.
  A run_id that first appears in a later poll becomes watched **only when it is owned by `--session`**
  (see "Run ownership" below). `--run <run_id>` (repeatable) adds a run to the watched set explicitly,
  baseline or not, and regardless of its owner.
- **Run ownership (2026-11-21).** Observed: STATE events in one session for
  `run-20261121-040142-44869`, a parallel run of a different session (its only run-level identity was
  `terminal_label=pid-44869`, which names no session). condukt `state init` now records
  `owner_session` (`$CLAUDE_CODE_SESSION_ID`, or explicit `null` when unset/empty) and `state
  list-tasks` repeats it on every row. For a run first listed after the baseline, from the
  `owner_session` of all its rows:
  - every row equals `--session` → watched as before;
  - every row names the same other non-blank string → nothing is emitted for that run (not this
    session's to drive);
  - anything else — `null`, key missing (e.g. an older condukt), not a string, blank, or rows that
    disagree → the owner is undetermined: exactly one `WATCH-UNOWNED <run_id> label=<terminal_label>`
    per run (`label=unknown` when no row has a string `terminal_label`), and no STATE / TASK-STALL for
    it. An unowned run is **surfaced, not dropped**: the driver must look at it and decide
    (`--run <run_id>` on the next arm watches it).
  The foreign / unowned classification is in memory only: it is forgotten when the run leaves the
  listing, and a re-armed watch classifies such a run again (so WATCH-UNOWNED can repeat once per arm).
  Baseline runs are not classified (unchanged: never watched, no WATCH-UNOWNED).
- **The watcher output is advisory.** flow-watch blocks nothing and authorises nothing; its events are
  prompts for the driver to act. Silence for a run means "not this session's to watch", never "that
  run is fine".
- **Persistence across re-arm.** The watched set and, per watched task, its last status and the time
  that status was first observed are stored in `--watch-state` (default
  `${XDG_STATE_HOME:-$HOME/.local/state}/claude-harnesses/flow-watch/<SESSION>.json`; parent dirs are
  created). On start an existing file is loaded, so a re-armed watch keeps watching the runs the
  previous arm watched (they are not baselined away). A file that exists but cannot be read or parsed →
  `WATCH-ERROR watch-state: <reason>`, start with an empty watched set, and do not overwrite that file
  (never silently discard it). A failed write → `WATCH-ERROR watch-state: <reason>` (once per reason).
  The file is written after every poll that changes it.
- **STATE `<run_id> <task>=<status> ...`** — for a watched run, whenever its set of listed
  (task_id, status) pairs changes, print all of its currently listed tasks. When a watched run
  disappears from the listing entirely (all its tasks terminal / run closed), print
  `STATE <run_id> finished` once and drop it from the watched set.
- **TASK-STALL** — a watched task whose listed status is `running` and has stayed unchanged, as observed
  by flow-watch (persisted first-seen time), for at least `--task-stall-secs`; `idle` = now − first-seen.
  Re-emitted at most once per `--task-stall-secs`.

Version: flow → 0.2.37.
