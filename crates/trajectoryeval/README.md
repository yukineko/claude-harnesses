`trajectoryeval` is a trajectory-match verifier [test: crates/trajectoryeval/tests/integration.rs::help_describes_the_verifier].

`trajectoryeval` checks the **PATH** the worker took to get there: the ordered
sequence of tool calls it made, against an *expected* trajectory spec [test: crates/trajectoryeval/tests/integration.rs::check_matching_trajectory_passes_exit_zero] [test: crates/trajectoryeval/tests/integration.rs::check_deviating_trajectory_exits_one].

[record: 2026-06-28 rev: c961b9c2] It is inspired by the trajectory matchers in
[langchain-ai/agentevals](https://github.com/langchain-ai/agentevals).

`trajectoryeval check --expected <spec.json> --actual <actual.json> [--json]`
compares an actual ordered tool sequence against an expected spec and reports
`{ pass, missing, unexpected, out_of_order }` [test: crates/trajectoryeval/tests/prose_claims.rs::claim_check_json_flag].
With `--json` it prints the serialized result as JSON instead of a human report [test: crates/trajectoryeval/tests/prose_claims.rs::claim_check_json_flag].

`trajectoryeval tier --config <cfg.json> --flow <id> [...]` classifies a flow as **core** (business-critical) or **non-core** from a config
allowlist [test: crates/trajectoryeval/src/tier.rs::tier_core_vs_non_core].

- The tier config is JSON of this shape [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero]:
  ```json
  { "core": ["checkout", "payment"],
    "diff_strategy": "structured_data",
    "sample_one_in": 20 }
  ```
  `core` = the allowlist (exact-match) [test: crates/trajectoryeval/tests/prose_claims.rs::claim_core_allowlist_exact_match]. `diff_strategy` defaults to
  `structured_data` [test: crates/trajectoryeval/tests/prose_claims.rs::claim_diff_strategy_default_structured]. `sample_one_in` = sample non-core flows `1 in N` runs [test: crates/trajectoryeval/src/tier.rs::seeded_sampling_rate_is_approximately_one_in_n] [test: crates/trajectoryeval/src/tier.rs::sample_one_in_one_always_samples] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_sampling_decision_reported].
  `sample_one_in` 0 disables sampling → existence check only [test: crates/trajectoryeval/src/tier.rs::non_core_sampling_disabled_is_existence_only].

- A **core** flow diffs a snapshot against a baseline on every run
  (`--baseline <base.json> --snapshot <snap.json>`) [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero] [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_mismatched_snapshot_exits_one].
  The structured-data comparison is normalized-shape JSON equality: object key
  order is normalized/ignored, values and structure must match [test: crates/trajectoryeval/src/tier.rs::structured_match_key_order_normalized] [test: crates/trajectoryeval/src/tier.rs::structured_mismatch_value] [test: crates/trajectoryeval/src/tier.rs::structured_mismatch_missing_key_and_len].
  Differences are reported as `/`-joined paths [test: crates/trajectoryeval/src/tier.rs::structured_mismatch_value] [test: crates/trajectoryeval/src/tier.rs::structured_diff_is_deterministic_ordering].
  Selecting `diff_strategy: "screenshot"` returns `DiffOutcome::Stubbed` (not
  implemented) [test: crates/trajectoryeval/src/tier.rs::screenshot_strategy_is_stubbed].

- `diff_strategy: "fuzzy_hash"` is a tolerance-based structured-data
  comparison [test: crates/trajectoryeval/src/tier.rs::fuzzy_diff_small_drift_under_threshold_is_match]. It computes a deterministic drift distance
  `distance_permille = round(1000 × differing leaves / total leaves)` (floored at
  `1` whenever any leaf differs, so real drift never rounds to `0`) [test: crates/trajectoryeval/tests/repro_fuzzy_hash_default_threshold_rounds_drift_away.rs::zero_tolerance_fuzzy_hash_does_not_pass_a_changed_leaf] and treats
  `distance_permille <= threshold_permille` as a **match** [test: crates/trajectoryeval/src/tier.rs::fuzzy_diff_boundary_distance_equal_threshold_is_match] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_fuzzy_distance_rounding]; above that it returns
  `DiffOutcome::DriftedBeyondThreshold` (distance + differing `/`-joined paths) [test: crates/trajectoryeval/src/tier.rs::fuzzy_diff_drift_over_threshold_reports_distance_and_paths],
  which is a **needs-human** verdict (exit `3`), not a fail [test: crates/trajectoryeval/src/tier.rs::verdict_for_diff_drifted_beyond_threshold_is_needs_human_not_fail] [test: crates/trajectoryeval/tests/integration.rs::tier_fuzzy_core_flow_drift_over_threshold_exits_three_needs_human]. `threshold_permille`
  is a config key whose default is `0` [test: crates/trajectoryeval/tests/prose_claims.rs::claim_threshold_permille_default_zero].
  [`examples/tier-config-fuzzy.json`](examples/tier-config-fuzzy.json)
  sets `threshold_permille: 250` [test: crates/trajectoryeval/tests/prose_claims.rs::claim_example_fuzzy_threshold_250].

- A **non-core** flow gets a lightweight **existence check**
  (`--exists true|false`) [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_absent_flow_exits_one]; `--exists` has no default, and omitting it for a
  non-core flow is a harness error (exit `2`) [test: crates/trajectoryeval/tests/repro_tier_exists_defaults_true.rs::omitted_exists_does_not_pass_a_non_core_flow] plus, when
  `sample_one_in > 0`, a deterministic **seeded low-frequency sampling** decision
  keyed by `(flow_id, --seed, --run-index)` — same inputs always yield the same decision [test: crates/trajectoryeval/src/tier.rs::seeded_sampling_is_reproducible] [test: crates/trajectoryeval/src/tier.rs::seeded_sampling_varies_by_run_index] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_sampling_keyed_by_seed_and_flow].
  The sampling decision is only
  **reported** (`present (sampled this run)` vs `present (not sampled this run)`) [test: crates/trajectoryeval/tests/prose_claims.rs::claim_sampling_decision_reported].
  Both are a pass [test: crates/trajectoryeval/src/tier.rs::verdict_for_non_core_never_needs_human]. Only
  `--exists false` fails a non-core flow [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_existing_flow_passes_exit_zero] [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_absent_flow_exits_one] [test: crates/trajectoryeval/src/tier.rs::non_core_absent_fails_existence] [test: crates/trajectoryeval/src/tier.rs::verdict_for_non_core_never_needs_human].

The CLI reports the core/non-core classification and the diff (match/mismatch)
result, or the non-core existence/sampling decision, as a human report or with
`--json` [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero] [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_existing_flow_passes_exit_zero] [test: crates/trajectoryeval/tests/integration.rs::tier_json_output_reports_verdict_field] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_tier_json_report_core_and_non_core].

- `tier` exit code `0` is a pass [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero].
- `tier` exit code `1` is a real deviation (mismatch, or existence check
  failed) [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_mismatched_snapshot_exits_one] [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_absent_flow_exits_one].
- `tier` exit code `2` is a harness error (unreadable/unparseable input, or a core flow
  without both `--baseline` and `--snapshot`) [test: crates/trajectoryeval/tests/prose_claims.rs::claim_tier_exit_two_harness_error].
- `tier` exit code `3` is **needs-human** — a core flow
  used an unimplemented diff strategy (currently `screenshot`) or its
  `fuzzy_hash` drift exceeded `threshold_permille`, so no automated verdict could
  be rendered [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_screenshot_stub_is_needs_human_not_silently_red] [test: crates/trajectoryeval/tests/integration.rs::tier_fuzzy_core_flow_drift_over_threshold_exits_three_needs_human]. `3` is distinct
  from `1` [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_screenshot_stub_is_needs_human_not_silently_red].
  An unimplemented strategy must not silently gate a run red: it never exits `1`
  or reports `result: fail` [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_screenshot_stub_is_needs_human_not_silently_red].

- A runnable example config ships at
  [`examples/tier-config.json`](examples/tier-config.json) [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero]:
  ```sh
  trajectoryeval tier --config examples/tier-config.json --flow checkout \
    --baseline base.json --snapshot snap.json     # core flow → real diff [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero]
  trajectoryeval tier --config examples/tier-config.json --flow settings \
    --exists true --seed 42 --run-index 3          # non-core → existence check [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_existing_flow_passes_exit_zero]
  ```

- The **expected** spec is JSON of this shape [test: crates/trajectoryeval/src/match_traj.rs::spec_deserializes_from_json]:
  ```json
  { "mode": "strict",
    "steps": [ { "tool": "Read" }, { "tool": "Edit", "optional": true } ] }
  ```
  `mode` is required (`strict` / `unordered` / `subsequence`) [test: crates/trajectoryeval/tests/prose_claims.rs::claim_mode_required].
  `optional` defaults to `false` [test: crates/trajectoryeval/src/match_traj.rs::step_optional_defaults_false].
  `steps` may be omitted and then defaults to empty [test: crates/trajectoryeval/tests/prose_claims.rs::claim_steps_default_empty].
- The **actual** JSON is an array of tool-name strings, for example `["Read", "Edit"]`
  (pipe the output of `extract` straight in) [test: crates/trajectoryeval/tests/integration.rs::extract_then_check_pipeline_passes_on_matching_trajectory].

`trajectoryeval extract --transcript <jsonl>` reads a Claude Code transcript
and prints the ordered `tool_use` names as a JSON array
on stdout — ready to feed into `check --actual` [test: crates/trajectoryeval/src/extract.rs::extracts_tool_names_in_order] [test: crates/trajectoryeval/tests/integration.rs::extract_then_check_pipeline_passes_on_matching_trajectory]. An unreadable file exits `2` [test: crates/trajectoryeval/tests/prose_claims.rs::claim_extract_unreadable_exits_two].
A line that is not valid JSON, or a transcript with no events, also exits `2` —
it is never skipped into an empty trajectory [test: crates/trajectoryeval/src/extract.rs::unparseable_line_is_invalid_data_not_skipped] [test: crates/trajectoryeval/tests/repro_extract_garbage_transcript_exits_zero.rs::extract_of_unparseable_transcript_does_not_exit_zero].

- **strict** — the actual sequence must equal the expected REQUIRED steps in
  order [test: crates/trajectoryeval/src/match_traj.rs::strict_exact_match_passes]. Optional steps may be absent, but if present must sit in their slot [test: crates/trajectoryeval/src/match_traj.rs::strict_optional_absent_passes] [test: crates/trajectoryeval/src/match_traj.rs::strict_optional_present_in_slot_passes] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_strict_optional_out_of_slot_fails].
  `missing` = required steps not matched [test: crates/trajectoryeval/src/match_traj.rs::strict_missing_required_step]; `unexpected` = actual tools with no
  place in the expected order [test: crates/trajectoryeval/src/match_traj.rs::strict_extra_tool_is_unexpected]; `out_of_order` = the right set appeared but in the
  wrong order [test: crates/trajectoryeval/src/match_traj.rs::strict_wrong_order_flags_out_of_order].
- **unordered** — order is ignored [test: crates/trajectoryeval/src/match_traj.rs::unordered_ignores_order]. `missing` = required expected tools absent
  from actual (as a set); `unexpected` = actual tools not in the expected set [test: crates/trajectoryeval/src/match_traj.rs::unordered_reports_missing_and_unexpected_sets];
  `out_of_order` is always false [test: crates/trajectoryeval/src/match_traj.rs::unordered_ignores_order].
- **subsequence** — the required steps must appear in `actual` in order but not
  necessarily contiguously (other tools may interleave) [test: crates/trajectoryeval/src/match_traj.rs::subsequence_interleaved_passes]. `missing` = required
  steps not found as an in-order subsequence [test: crates/trajectoryeval/src/match_traj.rs::subsequence_out_of_order_is_missing_not_reorder]; extras are allowed, so `unexpected`
  stays empty; `out_of_order` is false [test: crates/trajectoryeval/src/match_traj.rs::subsequence_interleaved_passes].
  A `subsequence` spec with no required step constrains nothing, so `check`
  refuses it with exit `2` instead of passing [test: crates/trajectoryeval/tests/repro_empty_subsequence_spec_passes_anything.rs::empty_subsequence_spec_does_not_exit_zero].

In every mode: `pass = missing.is_empty() && unexpected.is_empty() && !out_of_order` [test: crates/trajectoryeval/src/match_traj.rs::strict_exact_match_passes] [test: crates/trajectoryeval/src/match_traj.rs::strict_missing_required_step] [test: crates/trajectoryeval/src/match_traj.rs::strict_extra_tool_is_unexpected] [test: crates/trajectoryeval/src/match_traj.rs::strict_wrong_order_flags_out_of_order] [test: crates/trajectoryeval/src/match_traj.rs::unordered_ignores_order] [test: crates/trajectoryeval/src/match_traj.rs::unordered_reports_missing_and_unexpected_sets] [test: crates/trajectoryeval/src/match_traj.rs::subsequence_interleaved_passes] [test: crates/trajectoryeval/src/match_traj.rs::subsequence_out_of_order_is_missing_not_reorder].

- `check` exit code `0`: trajectory matched the spec (pass) [test: crates/trajectoryeval/tests/integration.rs::check_matching_trajectory_passes_exit_zero].
- `check` exit code `1`: a deviation (missing / unexpected / out-of-order steps) [test: crates/trajectoryeval/tests/integration.rs::check_deviating_trajectory_exits_one] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_check_exit_one_unexpected_and_out_of_order].
- `check` exit code `2`: harness error (unreadable or unparseable input) [test: crates/trajectoryeval/tests/integration.rs::check_unreadable_input_exits_two] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_check_exit_two_unparseable].

This is a plain CLI **gate**, not a lifecycle hook [test: crates/trajectoryeval/tests/prose_claims.rs::claim_not_a_lifecycle_hook].

condukt's task schema carries an
optional `expected_trajectory` field (see
`crates/condukt/src/model.rs`) [test: crates/condukt/src/model.rs::task_with_expected_trajectory_round_trips] [test: crates/condukt/src/model.rs::task_without_expected_trajectory_defaults_none].

```sh
trajectoryeval extract --transcript session.jsonl > actual.json
echo '{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}' > spec.json
trajectoryeval check --expected spec.json --actual actual.json --json
```
