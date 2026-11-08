# donegate

> **Completion-verification gate for Claude Code**, written in Rust.

Agents declare victory too early. `donegate` is the harness that makes "done"
mean *verified*: on every **Stop**, it runs your acceptance commands
(`build` / `test` / `lint` / `typecheck`) as **subprocesses**, and refuses to let
the turn end until every required check is green. A failing check's output is fed
straight back into the conversation, so the agent enters an **auto-fix loop**
instead of stopping on red.

It is the *dynamic* sibling of two static gates:

| Gate | When | Asks |
|---|---|---|
| `precommit-audit` | pre-commit | does the diff obey policy? (static) |
| `specguard` | on demand | did the impl drift from spec? (LLM) |
| **`donegate`** | **on Stop** | **does it actually build & pass? (runs it)** |

No API key. The LLM labor (fixing the failures) runs in your Claude Code
subscription; donegate is a deterministic Rust binary that only spawns processes
and reads exit codes — so it can never hit a rate limit.

## How it works

`donegate gate` is wired to the Claude Code **Stop** hook. On each stop it:

1. reads the hook JSON from stdin (`session_id`, `cwd`, `stop_hook_active`).
   **donegate judges only the hook `cwd`.** User rulings 2026-11-08, verbatim:
   "donegateのworktreeテストは問題ありすぎるので即時に停止" (donegate's worktree
   tests are too problematic; stop them immediately) and
   "donegateそのものが他のworktreeを見たり検証したりテストしたりすることは禁止とする"
   (donegate itself may not look at, verify or test other worktrees). Other
   worktrees or roots the session wrote into are not resolved, listed, run,
   blocked on or named. The transcript is read only to learn whether a written
   path lies inside the cwd (a path comparison; nothing outside the cwd is
   opened); an unreadable transcript blocks. A write into a worktree nested
   inside the cwd (e.g. `.claude/worktrees/…`) counts as a write into the cwd.
   A session that wrote into the cwd whose config exists but declares no check
   blocks naming the cwd. If the cwd has no donegate config at all, the stop is
   allowed with an empty stdout (no `systemMessage`; user ruling 2026-10-27).
   The allow is still recorded: stderr says "donegate not configured for <cwd>: no
   checks ran", and a `"verdict":"unconfigured"` entry naming the session and the
   cwd (`roots`) is appended to `<state_dir>/log.jsonl`.
   A session with no recorded writes into the cwd is unaffected;
2. loads `./donegate.toml` (or `~/.donegate/config.toml`);
3. asks `git` which files changed, and selects the checks whose `when_changed`
   globs match (unscoped checks always run);
4. runs each selected check as a subprocess, with a per-check timeout, streaming
   its combined output to a temporary log and reading back only a bounded window;
5. **all green** → exits 0, the stop proceeds;
   **any required failure** → emits `{"decision":"block","reason":…}` so Claude
   keeps working. The reason is **quiet**: per failing check it shows the name,
   exit code and command, then only the decision-relevant digest of the output
   (for cargo-style output: each `panicked at` line with its message, the
   `failures:` name list, `test result: FAILED`, the diagnostics and a final
   FAILED line; otherwise the diagnostics, the failing-test lines of the common
   runners — lines containing `FAILED`, go's `--- FAIL: …`, python unittest's
   `FAIL: …`, jest's/go's `FAIL <path>` (up to 10) — and the last 5 lines.
   Diagnostics are `error[…]`/`error:`/`warning:`
   headers with their first `--> file:line:col` location (up to 5, errors
   first; the same header in different files is kept once per location),
   single-line located diagnostics in the same cap — tsc
   `path(3,5): error TS2322: …` and gcc/clang `path:3:5: error: …` — plus
   unlocated `error:` summaries such as `could not compile` and compiler count
   summaries (tsc `Found N errors…`, gcc/clang `N errors generated.`). Capped
   at 24 lines, never empty), and the **absolute path of the full raw output**.
   Raw `--- stdout ---` / `--- stderr ---` sections and progress lines are never
   shown.

### SubagentStop

(The deferral of roots that only still-running background subagents wrote was
removed with the 2026-11-08 rulings: the parent's Stop judges only its cwd.)

`donegate subagent-gate` is the **SubagentStop** hook (registered in
`hooks/hooks.json`, and by `donegate install`). It judges the hook cwd only,
and only when the stopping subagent's transcript (`agent_transcript_path`, or
`<transcript dir>/<stem>/subagents/agent-<agent_id>.jsonl`) records a write
inside the cwd; no other worktree or root is judged. Green allows, red blocks
the subagent's stop with `{"decision":"block"}`, an identical repeat is waived
with a `systemMessage`, and `max_attempts` gives up. The counter and the waiver
memory are keyed `<session>-agent-<agent_id>`, so the subagent's chain is
separate from the parent's. A subagent that wrote nothing inside the cwd is
allowed silently. A cwd with no config or no declared checks is not judged
here. A cwd whose checks donegate refuses to run blocks. An unlocatable or
unreadable agent transcript also blocks. `donegate skip` is not consumed at
SubagentStop because it belongs to the parent session.

### Duplicate `cargo test` denial (dup-test hooks)

`hooks/hooks.json` also wires `donegate dup-test pre` (PreToolUse, Bash) and
`donegate dup-test post` (PostToolUse and PostToolUseFailure, Bash). `pre`
denies (`permissionDecision: "deny"`) a plain single `cargo test ...` that
already PASSED with the same normalized argv on the same tree; the reason says
so, and that a genuine re-run means changing the tree or the command. Key:
canonical cwd + git toplevel + argv + the passcache tree state (HEAD, `main`,
working-tree content incl. untracked files). `post` records a pass only for
`hook_event_name == "PostToolUse"` with `tool_response.interrupted == false`
and a key equal to the one `pre` snapshotted for the same `tool_use_id`. Any
doubt — compound commands, `cd`, quotes/`$`, `--manifest-path`/`--config`, not
a git repo, a git error, a corrupt store, an unknown event — means no deny and
no record. Store: `~/.donegate/dup-test/`. Non-`cargo test` calls return right
after the stdin JSON parse. `DONEGATE_DISABLE=1` turns it off.

### Session records of failing output

Every failing check's full raw output is saved byte-for-byte to
`<state_dir>/records/<session>/<unix-ms>-a<attempt>-<check>.log` (`<session>`
is the sanitised session id, `nosession` if empty). Retention keeps the newest
20 session directories and the newest 50 records per session; the current
session's directory is never deleted. A retention failure (anything but
"not found") never undoes the save; it is shown as a
`record retention failed: …` line under the record path.

If the record cannot be saved, the check still runs and the stop still blocks;
the reason says the full output could not be saved (with why) and shows **more**,
not less: the digest plus the last `max(output_tail_lines, 40)` raw lines.
The repeat ledger keys a block on its reason with the per-run record path
normalised (the attempt counter `attempt N/max` stays in that key).

**Identical-finding waiver** (backlog b80023b3). Within one chain of consecutive
blocks, a Stop whose root tree (HEAD, tracked diff, untracked files) and failing
check outputs are identical to that root's previous block is NOT blocked again:
donegate allows it with a JSON `systemMessage` saying nothing new was verified
and the checks are still RED. A waived stop does not count toward
`max_attempts`; a changed tree still blocks. It applies only to a Stop hook
payload with a non-empty `session_id` (manual `donegate gate` keeps exiting 1;
a payload without a session id blocks). The memory is cleared when the chain
ends (green, give-up, skip, or the counter's `reset_after_secs` idle reset).
Separately, the shared `harness_core::repeat` ledger (ruling 2026-09-18) can
still waive a block whose text recurs in the same session. A tree or
signature that cannot be determined blocks. Waived stops are logged
(`repeat-waived` in the gate log) but write no overwatch ledger entry.
The bounded exit is the attempt cap plus its
give-up sentinel (`donegate:giveup:<check>`, see Loop safety).

### Loop safety

- A per-session **attempt counter** gives up after `max_attempts` consecutive
  blocks (default 2) and allows the stop, so a genuinely stuck agent isn't
  trapped. The counter resets after `reset_after_secs` of idle, or on any green.
- **Escape hatch**: `donegate skip --reason "<why>"` — consumed once, allows
  the next stop, and applies only to the session that issued it. A reason is
  required, and both the issue and the consumption are appended to the gate log,
  so a bypass cannot happen unrecorded. (This replaces the old `.donegate-skip`
  file in the project root, which was removed: a shared one-shot marker is
  consumed by whichever session stops next, waving *that* session's legitimate
  gate through — CLAUDE.md section 5 forbids exactly that under parallel
  sessions.)
- **Kill switch**: `DONEGATE_DISABLE=1`, effective only in the environment
  Claude Code itself was started with — exporting it from a tool call does not
  reach this hook, which inherits the app's environment.
- **Opt-in**: with no config file at all (neither `<root>/donegate.toml` nor
  `~/.donegate/config.toml`), or a loaded config that declares no `[[check]]`,
  the gate allows every stop. Installing the hook does not block a project that
  never opted in.

When donegate **cannot judge**, it blocks rather than allowing:

- a project `donegate.toml` exists but the root is **not trusted** (refused);
- a config file exists but cannot be read or parsed;
- a declared check's name falls in the reserved `undetermined:` namespace.

These refusals are bounded by the same `max_attempts` counter: once exhausted,
donegate gives up loudly and allows the stop with "NOTHING WAS VERIFIED"
(logged as `giveup-refusal`). A panic in donegate itself goes through
`harness_core::gate::run::run_guarded` and fails **closed** (blocks the first
stop; only a second consecutive crash on the `stop_hook_active` re-entry is
allowed through).

## Install

```sh
cargo install --path .
cd your/project
donegate init          # writes a starter donegate.toml for your stack
donegate trust         # honor this project's donegate.toml (project configs are trust-gated)
donegate install       # merges the Stop + SubagentStop hooks into ~/.claude/settings.json (backs it up)
```

A project `donegate.toml` is only honored once the project is trusted
(`donegate trust`), so a hostile repo can't ship arbitrary Stop-time commands.
An untrusted project file is **not** replaced by the home config's checks: the
stop is blocked as a refusal (bounded, see above) until you run `donegate trust`
(or set `HARNESS_TRUST_ALL=1`). Only the knobs (`enabled`, `max_attempts`,
`state_dir`, timeouts) still come from `~/.donegate/config.toml`. Trusting a
repo's main working tree also covers its linked git worktrees.

Then check what would run:

```sh
donegate status        # resolved config + which checks apply to the current diff
donegate gate          # run the checks by hand (human report; exit 1 on failure)
```

Remove with `donegate uninstall`.

## Config

See [`donegate.example.toml`](donegate.example.toml). Each `[[check]]`:

| key | meaning |
|---|---|
| `name` | label in the block reason |
| `cmd` | shell command (`sh -c` / `cmd /C`) |
| `when_changed` | globs vs `git diff HEAD` + untracked; omit ⇒ always run |
| `timeout_secs` | per-check timeout (default `default_timeout_secs`) |
| `optional` | `true` = warn on failure, don't block |
| `workdir` | run in this subdir of the project root |

`donegate init` auto-detects Cargo / npm / Python / Go and writes matching
checks.

## Logs

Every decision appends one JSONL line to `~/.donegate/state/log.jsonl`
(`verdict` ∈ `green` / `blocked` / `giveup` / `giveup-reentry` / `skip` /
`refused` / `giveup-refusal` / `giveup-refusal-reentry`). Local only. Give-ups
are additionally recorded in overwatch's violation ledger
(`donegate:giveup:<check>` / `donegate:undetermined:<kind>`).

## License

MIT
