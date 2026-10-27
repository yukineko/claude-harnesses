Session-end auto-flow gate for Claude Code — a **Stop** hook that, once the `/record` prompt has been issued, keeps a session from ending while the in-flight condukt run still has pending tasks.
When a turn finishes, autoflow prompts `/record` once, then loops `/condukt` while the run's pending set keeps shrinking, escalating *visibly* if progress stalls.
A **PreCompact** + **UserPromptSubmit** pair keeps that loop alive across a `/compact`: PreCompact drops a resume marker when this session is driving the project's backlog queue (per `backlog lock status`), and the next UserPromptSubmit consumes it to re-inject a "resume /flow" instruction exactly once.

<!-- record: 2026-08-20 rev: 5289312f -->
The backlog queue went out of scope on 2026-08-20. Until then an empty condukt pending set fell through to the queue and blocked the Stop with "/backlog を実行してください" every turn (behind a compass-freshness gate), and a **SessionStart** hook made the same request up front as "バックログに N 件…/flow で開始しますか？". All three — the proposal, the Stop arm, and its compass gate — were retired on the user's instruction.
<!-- /record -->

The backlog queue is out of scope: a pending backlog item does not make the Stop block.

The Stop hook only ever emits a `block` decision with a reason — it never runs the work itself (its only side effects are its own per-session state file, marking the pending condukt tasks `running` on a routine continuation, and an overwatch violation record per block).
An unreadable condukt run-state **blocks**.
The launcher likewise emits a `block` for `stop` when no binary exists for the host.
The Stop also ends silently when the gate is disabled, when the condukt run file is missing (condukt never ran here — observed as "no pending task"), and on the stand-downs listed below.

- **Idle** (enough turns + tool events this session): block → `/session-insights:record`.
- **RecordRequested / Continuing** (condukt tasks still pending): block → `/condukt`, continuing while the pending set keeps *shrinking*, with no call-count ceiling.
- **RecordRequested / Continuing** (pending set fails to shrink `stuck_threshold` Stops in a row): escalate *visibly*.
- `stuck_threshold` defaults to 3.
- **RecordRequested / Continuing** (condukt run-state could not be read): block with a "判定不能" (undetermined) reason; the phase is left untouched so the next Stop re-observes.
- **Done** (no pending task in the condukt run): allow the turn to end, and the queue is not consulted.

The escalation is never silent: a stall does not end the Stop silently.
The escalation blocks with a message worded by the shared autonomy switch (`harness_core::autonomy` — autonomous: "continuing"; otherwise: "ask the user").
The escalating Stop is the one Stop that does *not* re-mark the pending tasks `running`.

- **Explicit user stop** (before the state machine): if the user's most recent transcript signal — a typed message, or their answer to an `AskUserQuestion` — is an explicit stop instruction ("止めて", "stop", …), *this* Stop is allowed through untouched.
- An explicit stop is not latched into `Done`; nudging resumes if the user keeps working.
- An unreadable or absent signal never counts as a stop.
- **Delegation-record advisory** (in RecordRequested / Continuing): if this session's transcript shows `/flow` drove a condukt run to completion without a `fugu-router record` for it, block once per session asking for `fugu-router record --class flow-delegation …`.

autoflow stands down entirely (allows the Stop) while `backlog lock status` reports a live driver for this project, so a running `/flow` or `/backlog` driver is never double-driven.
Backlog being *observably* not installed does not stand down.
Only a *positive* report of a live driver stands down.
When liveness cannot be determined (`backlog lock status` exits non-zero, prints empty or uninterpretable output, or reports its own `undetermined` object, or the plugin cache is unreadable), the Stop **blocks** with a "判定不能" (undetermined) reason and leaves the phase untouched.
A Stop with no session id (none in the payload and `CLAUDE_CODE_SESSION_ID` unset) blocks the same way.
To avoid an endless block, these undetermined blocks are bounded by `stop_hook_active`.
On the re-entry after such a block, the stop is allowed with exactly one short `systemMessage` line, `autoflow: 判定不能のまま通しました` (user ruling 2026-10-27: one-line results at Stop, no logs), and a `{"event":"bounded_allow","session_id","what","why"}` line is appended to `<state_dir>/log.jsonl` so the allow is still recorded and distinguishable from a clean pass.
An explicit user stop still takes precedence over an undetermined liveness.

At **PreCompact**, if this session is the exclusive lock holder or one of the registered drivers in `backlog lock status` (that is, a `/flow` loop is actually driving from this session) and the user hasn't opted out (`resume_flow_on_compact = false`), autoflow writes a resume marker.
PreCompact never blocks compaction.
When PreCompact cannot determine whether this session is driving (no session id, or `backlog lock status` failed or was uninterpretable), it writes no marker and prints a user-visible `systemMessage` saying so.
The following **UserPromptSubmit** consumes that marker (if any) and injects a "resume `/flow`" instruction exactly once.
Every ordinary turn without a marker stays silent.

Installed via the plugin marketplace, the bundled `hooks/hooks.json` wires the **Stop**, **PreCompact**, and **UserPromptSubmit** hooks to `${CLAUDE_PLUGIN_ROOT}/bin/autoflow` automatically.
 The **SessionStart** hook was removed.
Thresholds (min turns, min tool events, stuck threshold, resume-flow-on-compact) come from config defaults; the gate is on by default.
To override, write `~/.autoflow/config.toml`:

```toml
enabled = true                # default true
min_turns = 2                 # turns before the /record prompt
min_tool_events = 3           # tool events before the /record prompt
stuck_threshold = 3           # no-progress Stops before visible escalation
resume_flow_on_compact = true
state_dir = "~/.autoflow/state"
```

An unparseable config file is ignored (defaults apply).

```sh
cargo install --path .
autoflow stop            # Stop hook: run the record→condukt state machine
autoflow pre-compact     # PreCompact hook: drop a resume-flow marker if this session holds the lock
autoflow prompt-submit   # UserPromptSubmit hook: consume the marker and inject a "resume the /flow loop" instruction once
```

`autoflow stop` reads the hook JSON on stdin and prints a `block` decision (or nothing).
`autoflow pre-compact` and `autoflow prompt-submit` are silent unless the resume-marker gate is met.
The exception is `pre-compact` when the gate cannot be determined: it then prints a visible `systemMessage`.
`AUTOFLOW_DISABLE=1` silences the gate.

```sh
cargo test
```

Only the `bin/autoflow` launcher is committed.
It execs the per-platform `bin/autoflow-<os>-<arch>` binary.
That binary is gitignored.
Run the rollout again when you change behavior the hook relies on.
