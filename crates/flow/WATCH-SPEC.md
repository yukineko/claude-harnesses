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
- `STATE <run_id> <task>=<status> ...` — emitted when a run file's task-status vector changes (and once
  at startup for runs with any task not in a terminal status). Run files = `run-*.json` in the condukt
  state dir(s) whose `tasks` is a list of objects with `id`/`status`.
- `TASK-STALL <run_id> <task> status=<status> idle=<secs>s` — a task in `running` whose run file mtime
  AND the run's journal (`<run_id>.journal.jsonl`, if present) mtime are both older than
  `--task-stall-secs`; re-emitted at most once per `--task-stall-secs`.
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
