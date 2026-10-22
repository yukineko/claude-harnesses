When wired as a Stop hook (not by default), propguard derives *semantic properties* (invariants) from the current task's `done_criteria` and checks the generated code against them before the agent is allowed to declare a turn done.

 Property derivation follows **PGS** (Property-Generated Solver, [arXiv:2506.18315](https://arxiv.org/pdf/2506.18315)).

The plugin does not wire a Stop hook by default (`hooks/hooks.json` registers none); the Stop gate runs only if you wire it explicitly with `propguard install` below.

The binary deterministically *derives* the properties and enforces the count→threshold block; the semantic *judgement* of whether each property holds is done either by the agent already running on your subscription (inject mode) or by a headless checker you configure (subprocess mode).

Derivation is a deterministic rule set over a bilingual keyword taxonomy, capped at `max_properties`.

| id | invariant |
|----|-----------|
| `error-path` | 失敗パスは panic せず Err/エラーを返す (error handling) |
| `output-schema` | 出力スキーマ/フォーマットが安定している (schema stability) |
| `determinism` | 決定論的: 同一入力は同一出力を返す (determinism) |
| `idempotence` | 冪等: 複数回実行しても結果が変わらない (idempotence) |
| `bounds-monotonicity` | 境界・単調性・閾値が守られる (bounds/monotonicity) |
| `no-partial-write` | 部分書き込みが起きない (atomic / no-partial-write) |

Properties whose keywords appear in the done_criteria are derived; if fewer than `min_properties` match, the set is padded with the baseline *universal* invariants (`error-path`, `output-schema`, `determinism`).

Preview the derivation offline:

```
propguard derive "must be idempotent, never panic, keep the output schema stable"
```

done_criteria is sourced, in priority order, from:

1. the `PROPGUARD_CRITERIA` environment variable,
2. a `criteria_file` (default `.propguard-criteria`) in the project root,
3. the inline `done_criteria` value in `propguard.toml`.

With no source found, **every stop is allowed**.

| mode | what it does |
|------|--------------|
| `inject` | Blocks the stop once per new diff and injects the **property checklist**; the checklist describes the checked files as uncommitted changes whose author propguard does not verify. |
| `subprocess` | Runs `checker_cmd` (default `claude -p`) as a checker that emits one `PROP <id>: PASS\|FAIL` line per property; propguard counts the PASSes. |

The default mode is `inject`.

At `gate::below_threshold` the stop is **blocked iff `satisfied < threshold`**.
In inject mode a new diff is *unverified*, so it blocks once to inject the checklist; on the next stop the same diff is allowed — *trust-after-one-block*: propguard does not observe whether the agent actually checked the properties, it only guarantees the checklist was shown once.
In subprocess mode the PASS count is compared directly.
The threshold is clamped to the number of properties actually derived.

propguard hashes `(diff, properties)`.
The hash is recorded on a subprocess PASS (`properties-satisfied`) or on inject mode's trust-after-one-block.
A stop matching a recorded hash is allowed (`already-verified`).
A subprocess below-threshold result records no hash, so the checker re-runs on the next stop.
Each blocked round counts toward `max_attempts` (default 2; the counter resets after `reset_after_secs` of idle, default 600).
Once exceeded, a below-threshold result is **allowed** with tag `giveup` (properties NOT satisfied).

- No git repo, nothing checkable, no done_criteria source configured at all → **allow**.
- A config file (`propguard.toml` / `~/.propguard/config.toml`) that exists but cannot be read or parsed → **block** (tag `config-unreadable`, no give-up) — it is never silently replaced by built-in defaults.
- A config that parses but has an unrecognised `mode` value → **block** (tag `config-invalid`, no give-up) — it is not mapped to either mode.
- A `criteria_file` that exists but cannot be read, with no inline `done_criteria` to fall back to → **block** (tag `criteria-unreadable`, no give-up).
- A `PROPGUARD_CRITERIA` that is set but not valid UTF-8 → **block** (tag `criteria-unreadable`; no fallback to the file or inline value).
- A `checker-error-giveup` is recorded in overwatch's violation ledger; when it recurs across sessions it escalates to a **block** (`checker-outage-systemic`), and an unreadable ledger blocks too (`checker-outage-undetermined`).
  The escalation mapper (`gate::escalate_giveup_on_systemic`) also maps a systemic `git-scan-failed-giveup` or `diff-read-failed-giveup` to a block.
  `truncated-giveup` is not an outage and is not escalated.
- A checker that crashes / times out / emits unusable output → **block** (bounded), then give up.
- A `git` command that errors inside a real repo (changed set undetermined) → **block** (bounded, tag `git-scan-failed`), then give up (`git-scan-failed-giveup`).
- A truncated (too-large) diff has an unchecked tail → **block** (bounded), then give up.
- A diff that could not be **read** — any of `git diff`, `git diff --cached`, the untracked `ls-files`, or an untracked file's body failing to decode → **block** (bounded, tag `diff-read-failed`), then give up.
  This covers the partial case too: if one of those reads succeeds and another does not, the readable half is *not* handed onward as if it were the whole diff.
  An unread diff is undetermined, never "empty ⇒ nothing changed".
- A genuine harness panic → fail-closed **block** on the first stop: the `check` Stop entry goes through `harness_core::gate::run::run_guarded`, and a panic under it emits a block decision.

The old shared `.propguard-skip` file in the project root no longer waves a stop through.
Escape hatches: `propguard skip --reason "<why>"` — one-shot, reason required, scoped to the session that issued it, and recorded in the gate log at both issue and consumption.
Or `enabled = false` in the config, or `PROPGUARD_DISABLE=1`.

## Install the plugin

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install propguard@yukineko
```

## Install from source

```
cargo build --release -p propguard
./target/release/propguard install     # wires the Stop hook into ~/.claude/settings.json
propguard init                         # writes a starter ./propguard.toml
```

`propguard uninstall` removes the Stop hook.
`propguard --protects` prints what the gate protects, from what, and why.
Session state lives under `~/.propguard/state` (config key `state_dir`; the `PROPGUARD_STATE_DIR` env var overrides it).

See [`propguard.example.toml`](./propguard.example.toml).
Key knobs: `mode`, `min_properties`/`max_properties` (`max_properties` is capped at 5; `min_properties` = 1 is accepted, not raised to 3), `threshold`, `max_attempts`, `reset_after_secs`, `min_changed_files`, `max_diff_bytes`, `criteria_file`, `include`/`exclude`, `enabled`, and (subprocess mode) `checker_cmd` / `checker_timeout_secs`.
A project `propguard.toml` is honored only once the root is **trusted**.
Trust a root with `propguard trust`.

`propguard status` shows the resolved config and the properties derived for the current task.

In `subprocess` mode, `checker_cmd` runs as a real OS subprocess and is bounded by a timeout:

- **Default: 300 seconds**.
- Configurable via the top-level `checker_timeout_secs` key in `propguard.toml` (or `~/.propguard/config.toml`):

  ```toml
  checker_cmd = "claude -p"
  checker_timeout_secs = 45   # override the 300s default
  ```

- A value of `0` is sanitized back to the 300s default rather than disabling the timeout.
  An invalid/negative value fails TOML parsing, which makes the whole config unreadable → **block** (`config-unreadable`), not a silent default.
- **On timeout, propguard kills the checker's backgrounded descendants too**, not just the immediate child: a grandchild that a shell-wrapped `checker_cmd` backgrounds with `&` does not outlive the timeout.
  A descendant that keeps the stdout pipe open does not hang the stop.
- A timeout is reported as a checker `Error` (never silently "all pass") and goes through the same fail-closed, bounded `max_attempts` give-up path as any other checker failure.
