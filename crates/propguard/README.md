# propguard

A **property gate** for Claude Code. On every `Stop`, propguard derives 3–5
*semantic properties* (invariants) from the current task's `done_criteria` and
checks the generated code against them before the agent is allowed to declare a
turn done. It is the "does the code hold the *invariants*?" complement to
[tdd](https://github.com/yukineko/tdd)'s "do the concrete tests pass?".

`tdd` runs specific test cases; that proves examples but never *formalizes* the
semantic invariants the code must satisfy. Following **PGS** (Property-Generated
Solver, [arXiv:2506.18315](https://arxiv.org/pdf/2506.18315)), propguard turns
free-text done_criteria into a small, checkable property set and blocks when
fewer than a `threshold` of them hold.

It is subscription-native: one Stop hook plus a bundled Rust binary, **no API
key**. The binary deterministically *derives* the properties and enforces the
count→threshold block; the semantic *judgement* of whether each property holds is
done either by the agent already running on your subscription (inject mode) or by
a headless checker you configure (subprocess mode).

## The derived properties

Derivation is a deterministic rule set over a bilingual keyword taxonomy, capped
at 3–5. The catalog:

| id | invariant |
|----|-----------|
| `error-path` | 失敗パスは panic せず Err/エラーを返す (error handling) |
| `output-schema` | 出力スキーマ/フォーマットが安定している (schema stability) |
| `determinism` | 決定論的: 同一入力は同一出力 (determinism) |
| `idempotence` | 冪等: 複数回実行しても結果が変わらない (idempotence) |
| `bounds-monotonicity` | 境界・単調性・閾値が守られる (bounds/monotonicity) |
| `no-partial-write` | 部分書き込みが起きない (atomic / no-partial-write) |

Properties whose keywords appear in the done_criteria are surfaced first; if
fewer than `min_properties` match, the set is padded with the baseline
*universal* invariants (`error-path`, `output-schema`, `determinism`) that any
generated code should satisfy. Preview the derivation offline:

```
propguard derive "must be idempotent, never panic, keep the output schema stable"
```

## Where done_criteria comes from

Sourced, in priority order, from:

1. the `PROPGUARD_CRITERIA` environment variable,
2. a `criteria_file` (default `.propguard-criteria`) in the project root —
   condukt or the agent writes the current task's done_criteria there,
3. the inline `done_criteria` value in `propguard.toml`.

With no source found, **every stop is allowed** — the gate never invents a
finding.

## Two modes

| mode | what it does | cost |
|------|--------------|------|
| `inject` (default) | Blocks the stop once per new diff and injects the **property checklist**; the running agent self-verifies its own code against each property and fixes what fails before finishing. | free (no extra process) |
| `subprocess` | Runs `checker_cmd` (default `claude -p`) as an **independent** checker that emits one `PROP <id>: PASS\|FAIL` line per property; propguard counts the PASSes. | one headless check per round |

## The block threshold

The one enforcement point (`gate::below_threshold`): the stop is **blocked iff
`satisfied < threshold`**. In inject mode a new diff is *unverified*
(`satisfied = 0`), so it blocks once to inject the checklist; once the agent has
addressed it, the same diff (hashed together with its property set) is allowed.
In subprocess mode the PASS count is compared directly. The threshold is clamped
to the number of properties actually derived, so it can never be permanently
unsatisfiable.

## How it converges / stays safe

propguard hashes `(diff, properties)`. A stop matching the last one it forced a
check of is allowed — the agent already addressed exactly that. A *changed* diff
costs one more round, capped by `max_attempts` (default 2), so the agent is never
trapped. Fail-closed but bounded:

- No git repo, nothing checkable, no done_criteria → **allow**.
- A checker that crashes / times out / emits unusable output → **block**
  (bounded), then give up loudly — a broken checker never becomes a bypass.
- A truncated (too-large) diff has an unchecked tail → **block** (bounded), then
  give up loudly.
- A genuine harness panic → swallowed to exit 0 (never-break-a-turn).

Escape hatches: create `.propguard-skip` (one-shot, with a one-line reason) or
set `PROPGUARD_DISABLE=1`.

## Install

### As a plugin (subscription, no build)

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install propguard@yukineko
```

### From source

```
cargo build --release -p propguard
./target/release/propguard install     # wires the Stop hook into ~/.claude/settings.json
propguard init                         # writes a starter ./propguard.toml
```

## Config

See [`propguard.example.toml`](./propguard.example.toml). Key knobs:
`mode`, `min_properties`/`max_properties` (3–5), `threshold`, `max_attempts`,
`criteria_file`, `include`/`exclude`, and (subprocess mode) `checker_cmd`. A
project `propguard.toml` is honored only once the root is **trusted**
(`propguard trust`), since `checker_cmd` runs as a subprocess.

`propguard status` shows the resolved config and the properties derived for the
current task.

### Checker subprocess timeout (`checker_timeout_secs`)

In `subprocess` mode, `checker_cmd` runs as a real OS subprocess and is bounded
by a timeout so a hung or slow checker can never trap the turn indefinitely:

- **Default: 300 seconds.**
- Configurable via the top-level `checker_timeout_secs` key in
  `propguard.toml` (or `~/.propguard/config.toml`) — the same file as the
  other subprocess-mode knobs (`checker_cmd`), not a separate `[check]`
  section:

  ```toml
  # subprocess mode only:
  checker_cmd = "claude -p"
  checker_timeout_secs = 45   # override the 300s default
  ```

- A value of `0` (or an invalid/negative value that fails TOML parsing) is
  sanitized back to the 300s default rather than disabling the timeout — the
  timeout can never be turned into an unbounded wait.
- **On timeout, propguard kills the checker's whole process tree**, not just
  the immediate child: the checker is spawned in its own process group
  (Unix), so a shell-wrapped `checker_cmd` (e.g. one containing `;`, `&&`, or
  backgrounding a grandchild with `&`) is fully reaped via a single
  group-kill — a backgrounded/exec'd descendant can't outlive the timeout and
  keep running (or keep the stdout pipe open) after `run_checker` returns.
  On non-Unix platforms, only the direct child can be killed (best-effort).
- A timeout is reported as a checker `Error` (never silently "all pass") and
  goes through the same fail-closed, bounded `max_attempts` give-up path as
  any other checker failure (see "How it converges / stays safe" above).
