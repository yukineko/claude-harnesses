`trajectoryeval` is a trajectory-match verifier.

`trajectoryeval` checks the **PATH** the worker took to get there: the ordered
sequence of tool calls it made, against an *expected* trajectory spec.

 It is inspired by the trajectory matchers in
[langchain-ai/agentevals](https://github.com/langchain-ai/agentevals).

`trajectoryeval check --expected <spec.json> --actual <actual.json> [--json]`
compares an actual ordered tool sequence against an expected spec and reports
`{ pass, missing, unexpected, out_of_order }`.
With `--json` it prints the serialized result as JSON instead of a human report.

`trajectoryeval tier --config <cfg.json> --flow <id> [...]` classifies a flow as **core** (business-critical) or **non-core** from a config
allowlist.

- The tier config is JSON of this shape:
  ```json
  { "core": ["checkout", "payment"],
    "diff_strategy": "structured_data",
    "sample_one_in": 20 }
  ```
  `core` = the allowlist (exact-match). `diff_strategy` defaults to
  `structured_data`. `sample_one_in` = sample non-core flows `1 in N` runs.
  `sample_one_in` 0 disables sampling → existence check only.

- A **core** flow diffs a snapshot against a baseline on every run
  (`--baseline <base.json> --snapshot <snap.json>`).
  The structured-data comparison is normalized-shape JSON equality: object key
  order is normalized/ignored, values and structure must match.
  Differences are reported as `/`-joined paths.
  Selecting `diff_strategy: "screenshot"` returns `DiffOutcome::Stubbed` (not
  implemented).

- `diff_strategy: "fuzzy_hash"` is a tolerance-based structured-data
  comparison. It computes a deterministic drift distance
  `distance_permille = round(1000 × differing leaves / total leaves)` (floored at
  `1` whenever any leaf differs, so real drift never rounds to `0`) and treats
  `distance_permille <= threshold_permille` as a **match**; above that it returns
  `DiffOutcome::DriftedBeyondThreshold` (distance + differing `/`-joined paths),
  which is a **needs-human** verdict (exit `3`), not a fail. `threshold_permille`
  is a config key whose default is `0`.
  [`examples/tier-config-fuzzy.json`](examples/tier-config-fuzzy.json)
  sets `threshold_permille: 250`.

- A **non-core** flow gets a lightweight **existence check**
  (`--exists true|false`); `--exists` has no default, and omitting it for a
  non-core flow is a harness error (exit `2`) plus, when
  `sample_one_in > 0`, a deterministic **seeded low-frequency sampling** decision
  keyed by `(flow_id, --seed, --run-index)` — same inputs always yield the same decision.
  The sampling decision is only
  **reported** (`present (sampled this run)` vs `present (not sampled this run)`).
  Both are a pass. Only
  `--exists false` fails a non-core flow.

The CLI reports the core/non-core classification and the diff (match/mismatch)
result, or the non-core existence/sampling decision, as a human report or with
`--json`.

- `tier` exit code `0` is a pass.
- `tier` exit code `1` is a real deviation (mismatch, or existence check
  failed).
- `tier` exit code `2` is a harness error (unreadable/unparseable input, or a core flow
  without both `--baseline` and `--snapshot`).
- `tier` exit code `3` is **needs-human** — a core flow
  used an unimplemented diff strategy (currently `screenshot`) or its
  `fuzzy_hash` drift exceeded `threshold_permille`, so no automated verdict could
  be rendered. `3` is distinct
  from `1`.
  An unimplemented strategy must not silently gate a run red: it never exits `1`
  or reports `result: fail`.

- A runnable example config ships at
  [`examples/tier-config.json`](examples/tier-config.json):
  ```sh
  trajectoryeval tier --config examples/tier-config.json --flow checkout \
    --baseline base.json --snapshot snap.json     # core flow → real diff
  trajectoryeval tier --config examples/tier-config.json --flow settings \
    --exists true --seed 42 --run-index 3          # non-core → existence check
  ```

- The **expected** spec is JSON of this shape:
  ```json
  { "mode": "strict",
    "steps": [ { "tool": "Read" }, { "tool": "Edit", "optional": true } ] }
  ```
  `mode` is required (`strict` / `unordered` / `subsequence`).
  `optional` defaults to `false`.
  `steps` may be omitted and then defaults to empty.
- The **actual** JSON is an array of tool-name strings, for example `["Read", "Edit"]`
  (pipe the output of `extract` straight in).

`trajectoryeval extract --transcript <jsonl>` reads a Claude Code transcript
and prints the ordered `tool_use` names as a JSON array
on stdout — ready to feed into `check --actual`. An unreadable file exits `2`.
A line that is not valid JSON, or a transcript with no events, also exits `2` —
it is never skipped into an empty trajectory.

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
  A `subsequence` spec with no required step constrains nothing, so `check`
  refuses it with exit `2` instead of passing.

In every mode: `pass = missing.is_empty() && unexpected.is_empty() && !out_of_order`.

- `check` exit code `0`: trajectory matched the spec (pass).
- `check` exit code `1`: a deviation (missing / unexpected / out-of-order steps).
- `check` exit code `2`: harness error (unreadable or unparseable input).

This is a plain CLI **gate**, not a lifecycle hook.

condukt's task schema carries an
optional `expected_trajectory` field (see
`crates/condukt/src/model.rs`).

```sh
trajectoryeval extract --transcript session.jsonl > actual.json
echo '{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}' > spec.json
trajectoryeval check --expected spec.json --actual actual.json --json
```
