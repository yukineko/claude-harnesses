# trajectoryeval

A **trajectory-match verifier** — the sibling of an output verifier.

condukt's online verifier checks a task's **OUTPUT** (its `done_criteria`).
`trajectoryeval` checks the **PATH** the worker took to get there: the ordered
sequence of tool calls it made, against an *expected* trajectory spec. It is
inspired by the trajectory matchers in
[langchain-ai/agentevals](https://github.com/langchain-ai/agentevals).

It is **subscription-native**: one bundled Rust binary, no API key, no network.

## Subcommands

### `trajectoryeval check --expected <spec.json> --actual <actual.json> [--json]`

Compares an actual ordered tool sequence against an expected spec and reports
`{ pass, missing, unexpected, out_of_order }` (human report, or `--json` for the
serialized result).

### `trajectoryeval tier --config <cfg.json> --flow <id> [...]`

**Risk-tiered e2e verification.** A config-driven "core" flow allowlist
classifies a flow as **core** (business-critical) or **non-core** and applies
tier-appropriate verification. Classification, diff, and sampling are all
deterministic pure functions (no LLM, no network, no clock).

- **config** JSON:
  ```json
  { "core": ["checkout", "payment"],
    "diff_strategy": "structured_data",
    "sample_one_in": 20 }
  ```
  `core` = the allowlist (exact-match). `diff_strategy` defaults to
  `structured_data`. `sample_one_in` = sample non-core flows `1 in N` runs
  (0 disables sampling → existence check only).

- **core** flows: capture a snapshot and actually **diff** it every run
  (`--baseline <base.json> --snapshot <snap.json>`). Because this repo has NO
  deployed runtime UI, the primary always-available mechanism is a deterministic
  **structured-data comparison** (normalized-shape JSON equality: object key
  order is normalized/ignored, values and structure must match). Differences are
  reported as JSON-pointer paths. A **perceptual-hash / screenshot** comparison
  (`diff_strategy: "screenshot"`) is provided behind a trait/enum boundary but is
  an honest **stub** (not implemented) — selecting it returns
  `DiffOutcome::Stubbed` so a real implementation can be dropped in later without
  changing the tiering logic.

- **non-core** flows: a lightweight **existence check** (à la specguard
  spec-audit — `--exists true|false`, default true) plus, when
  `sample_one_in > 0`, a deterministic **seeded low-frequency sampling** decision
  keyed by `(flow_id, --seed, --run-index)` (seedable, no unseeded randomness) —
  same inputs always yield the same decision.

The CLI reports the core/non-core classification and the diff (match/mismatch)
result, or the non-core existence/sampling decision, as a human report or with
`--json`.

- **exit codes**: `0` pass, `1` a real deviation (mismatch, or existence check
  failed), `2` harness error (unreadable/unparseable input), `3`
  **needs-human** — a core flow used an unimplemented diff strategy (currently
  `screenshot`), so no automated verdict could be rendered. `3` is deliberately
  distinct from `1`: an unimplemented strategy is a missing capability, not a
  real regression, and must not silently gate every run red.

- a runnable example config ships at
  [`examples/tier-config.json`](examples/tier-config.json):
  ```sh
  trajectoryeval tier --config examples/tier-config.json --flow checkout \
    --baseline base.json --snapshot snap.json     # core flow → real diff
  trajectoryeval tier --config examples/tier-config.json --flow settings \
    --exists true --seed 42 --run-index 3          # non-core → existence/sampling
  ```

- **expected** spec JSON:
  ```json
  { "mode": "strict",
    "steps": [ { "tool": "Read" }, { "tool": "Edit", "optional": true } ] }
  ```
  `optional` defaults to `false`.
- **actual** JSON: an array of tool-name strings, e.g. `["Read", "Edit"]`
  (pipe the output of `extract` straight in).

### `trajectoryeval extract --transcript <jsonl>`

Streams a Claude Code transcript **line-by-line** (it never loads the whole
transcript into memory) and prints the ordered `tool_use` names as a JSON array
on stdout — ready to feed into `check --actual`.

## Modes

- **strict** — the actual sequence must equal the expected REQUIRED steps in
  order. Optional steps may be absent, but if present must sit in their slot.
  `missing` = required steps not matched; `unexpected` = actual tools with no
  place in the expected order; `out_of_order` = the right set appeared but in the
  wrong order.
- **unordered** — order is ignored. `missing` = required expected tools absent
  from actual (as a set); `unexpected` = actual tools not in the expected set;
  `out_of_order` is always false.
- **subsequence** — the required steps must appear in `actual` in order but not
  necessarily contiguously (other tools may interleave). `missing` = required
  steps not found as an in-order subsequence; extras are allowed, so `unexpected`
  stays empty; `out_of_order` is false.

In every mode: `pass = missing.is_empty() && unexpected.is_empty() && !out_of_order`.

## Exit codes

Mirrors the evalkit / schemaguard 0/1/2 gate policy:

| code | meaning |
|------|---------|
| `0`  | trajectory matched the spec (pass) |
| `1`  | a deviation (missing / unexpected / out-of-order steps) |
| `2`  | harness error (unreadable or unparseable input) |

This is a plain CLI **gate**, not a lifecycle hook.

## Wired into condukt (Phase 6)

`trajectoryeval` is not just a standalone binary — condukt's task schema carries an
optional `expected_trajectory: {mode, steps:[{tool}]}` field (see
`crates/condukt/src/model.rs`), which condukt-interpreter may fill in when a task's
correctness depends on *how* it's done, not just the output. When a task declares it,
condukt's Phase 6 (`crates/condukt/skills/condukt/SKILL.md`) runs `extract` on the
worker sub-agent's transcript and feeds the result into `check` **alongside** the
normal output verification — a second, path-based verifier dimension. If the
`trajectoryeval` binary isn't on `PATH`, or the task has no `expected_trajectory`, or
the worker transcript can't be resolved, this step is skipped entirely (fail-soft; it
never blocks Phase 6). A trajectory deviation (exit `1`) does **not** override the
output verdict — a task whose output satisfies `done_criteria` still gets `verified`,
with the deviation recorded as a `reason` for HOTL visibility.

## Example

```sh
trajectoryeval extract --transcript session.jsonl > actual.json
echo '{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}' > spec.json
trajectoryeval check --expected spec.json --actual actual.json --json
```
