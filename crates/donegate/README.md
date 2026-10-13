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

1. reads the hook JSON from stdin (`session_id`, `cwd`, `stop_hook_active`);
2. loads `./donegate.toml` (or `~/.donegate/config.toml`);
3. asks `git` which files changed, and selects the checks whose `when_changed`
   globs match (unscoped checks always run);
4. runs each selected check as a subprocess, with a per-check timeout, capturing
   a **bounded tail** of its output (never the whole log);
5. **all green** → exits 0, the stop proceeds;
   **any required failure** → emits `{"decision":"block","reason":…}` so Claude
   keeps working, with the failing command + output tail in the reason.

### Loop safety

- A per-session **attempt counter** gives up after `max_attempts` consecutive
  blocks (default 3) and allows the stop, so a genuinely stuck agent isn't
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
donegate install       # merges the Stop hook into ~/.claude/settings.json (backs it up)
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
