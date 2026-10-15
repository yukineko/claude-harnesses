Do not read this crate's existence as coverage. Treat it as scoring machinery, not as coverage.

Never trigger the gate from hosted CI: any replacement trigger must be local per §7 (pre-push or an explicit script).

No git hook under `.githooks/` references `scripts/mutation-gate.sh` and there is no `.github/workflows/` directory, so the kill-rate gate is INERT: it runs only when a human invokes it. [test: crates/mutategate/tests/prose_claims.rs::claim_inert_no_trigger]

Internal tooling — not a distributed Claude Code plugin (no `plugin.json`). [test: crates/mutategate/tests/prose_claims.rs::claim_not_a_plugin_a] [test: crates/mutategate/tests/prose_claims.rs::claim_not_a_plugin_b]

See Meta's Automated Compliance Hardening (ACH) and PRIMG (arXiv:2505.05584) for background.

The gate parses `cargo-mutants`'s `outcomes.json`, computes the kill-rate, and exits non-zero when it is below the threshold. [test: crates/mutategate/tests/overwatch_violation.rs::fail_outcome_appends_a_signed_nonempty_violation]

Kill-rate definition used here: [test: crates/mutategate/src/lib.rs::viable_and_killed_exclude_unviable] [test: crates/mutategate/src/lib.rs::kill_rate_is_killed_over_viable] [test: crates/mutategate/src/lib.rs::unknown_summary_does_not_inflate_kill_rate] [test: crates/mutategate/src/lib.rs::no_viable_mutants_fails_the_gate]

```
viable   = caught + missed + timeout + unknown   (unviable mutants excluded — no signal)
killed   = caught + timeout                      (a timeout is a test-exposed misbehaviour)
kill_rate = killed / viable                      (undefined -> gate fails)
```

`unknown` is a mutant whose `summary` state this crate does not recognise, or a record with no `summary`. [test: crates/mutategate/src/lib.rs::unknown_summary_states_are_tracked_not_dropped]

A record whose `scenario` is neither `"Baseline"` nor a `Mutant` object (null or absent included) is also counted as `unknown`. [test: crates/mutategate/src/lib.rs::ca_mutategate_03_null_scenario_dropped_flips_fail_to_pass]

It counts toward `viable` but not toward `killed`, so an unrecognised state is scored as a survivor instead of vanishing from the denominator and inflating the rate. [test: crates/mutategate/src/lib.rs::unknown_summary_does_not_inflate_kill_rate]

A run with any `unknown` also carries a warning in its reason. [test: crates/mutategate/tests/prose_claims.rs::claim_unknown_warning_in_reason_a] [test: crates/mutategate/tests/prose_claims.rs::claim_unknown_warning_in_reason_b] [test: crates/mutategate/tests/prose_claims.rs::claim_unknown_warning_in_reason_c_no_unknown_no_warning]

```sh
# Gate an existing outcomes.json [test: crates/mutategate/tests/overwatch_violation.rs::fail_outcome_appends_a_signed_nonempty_violation]
cargo run -p mutategate -- --outcomes mutants.out/outcomes.json --min-kill-rate 0.80

# End-to-end: run the engine on the pilot crate, then gate [test: scripts/test_mutation_gate_status.sh] [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_default_harness_core]
scripts/mutation-gate.sh
PILOT=difflog MIN_KILL_RATE=0.70 scripts/mutation-gate.sh
PILOT=specguard scripts/mutation-gate.sh
PILOT=condukt scripts/mutation-gate.sh
PILOT=blastguard scripts/mutation-gate.sh

# Show what this gate protects, from what, and on what grounds, without a verdict [test: crates/mutategate/tests/protects_flag.rs::protects_prints_three_labelled_nonempty_lines_and_exits_zero]
cargo run -p mutategate -- --protects
```

Defaults: `--outcomes mutants.out/outcomes.json`, `--min-kill-rate 0.80`. [test: crates/mutategate/tests/prose_claims.rs::claim_defaults_a_exactly_080_passes_with_default_path] [test: crates/mutategate/tests/prose_claims.rs::claim_defaults_b_075_fails_with_default_path] [test: crates/mutategate/tests/prose_claims.rs::claim_defaults_c_default_path_is_the_one_read]

A `--min-kill-rate` at or below `1e-9` (zero, negative) is rejected with exit `2`. [test: crates/mutategate/src/lib.rs::negative_min_kill_rate_is_rejected] [test: crates/mutategate/src/lib.rs::sub_epsilon_min_kill_rate_is_rejected_it_still_disables_the_gate] [test: crates/mutategate/tests/overwatch_violation.rs::zero_min_kill_rate_is_rejected_by_the_cli_before_evaluating] [test: crates/mutategate/tests/overwatch_violation.rs::sub_epsilon_min_kill_rate_is_rejected_by_the_cli]

Exit codes: `0` pass, `1` kill-rate below threshold (or no viable mutants), `2` usage/IO/parse error. [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_a_pass_is_0] [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_b_below_threshold_is_1] [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_c_no_viable_mutants_is_1] [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_d_missing_file_is_2] [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_e_malformed_json_is_2] [test: crates/mutategate/tests/prose_claims.rs::claim_exit_codes_f_unknown_flag_is_2]

On a FAIL the gate also appends a violation event to overwatch's fleet violation ledger. [test: crates/mutategate/tests/overwatch_violation.rs::fail_outcome_appends_a_signed_nonempty_violation]

If that write fails, a note is printed and the FAIL verdict still stands. [test: crates/mutategate/tests/overwatch_violation.rs::emit_failure_does_not_gate_the_gate_but_is_announced]

`scripts/mutation-pilots.sh` maps a list of changed paths (stdin) to the calibrated pilot crate(s) to run, falling back to `harness-core` so it never prints an empty list. [test: scripts/test_mutation_pilots.sh]

- Pilot 1: `harness-core` (default). [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_default_harness_core]
- Pilot 2: `specguard` — narrowed by default to `--file crates/specguard/src/similarity.rs`, and by default skips the one test `ack_blocks_when_no_new_commits_since_raised`. [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_specguard_default_a_file] [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_specguard_default_b_skip]
- Pilot 3: `condukt` — narrowed by default to `--file crates/condukt/src/circuit.rs`. [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_condukt_default]
- Pilot 4: `blastguard` — narrowed by default to `--file crates/blastguard/src/classify.rs`. [test: crates/mutategate/tests/prose_claims.rs::claim_pilot_blastguard_default]
- Override the pilot with `PILOT=<crate>` [test: crates/mutategate/tests/prose_claims.rs::claim_mutants_extra_overrides_a_arbitrary_pilot]. `MUTANTS_EXTRA` (for example `MUTANTS_EXTRA="--file crates/harness-core/src/hash.rs"`) is passed to cargo-mutants verbatim, and the script runs cargo-mutants from the repo root whatever the caller's directory [test: crates/mutategate/tests/prose_claims.rs::claim_mutants_extra_overrides_b_path_passed_verbatim_cwd_is_repo_root] [test: crates/mutategate/tests/prose_claims.rs::claim_mutants_extra_overrides_b2_cwd_independent_of_caller_cwd]. A non-empty `MUTANTS_EXTRA` overrides the per-pilot default entirely. [test: crates/mutategate/tests/prose_claims.rs::claim_mutants_extra_overrides_c_explicit_extra_replaces_default]

[record: 2026-09-11 rev: 89b31bb4] Of the GATE crates measured alongside blastguard, stuckguard and propguard scored below the bar and overwatch could not be measured (baseline failure); none of the three was made a pilot.

Add crates one at a time, only once each already clears the threshold.

Raise `MIN_KILL_RATE` as suites harden [test: crates/mutategate/tests/prose_claims.rs::claim_min_kill_rate_env_a_passed_to_mutategate] [test: crates/mutategate/tests/prose_claims.rs::claim_min_kill_rate_env_a2_default_is_080_and_fails_same_data].

The script passes `--output target/mutants-<pilot>` to cargo-mutants and scores `target/mutants-<pilot>/mutants.out/outcomes.json`. [test: crates/mutategate/tests/prose_claims.rs::claim_min_kill_rate_env_b_output_dir_per_pilot]

[record: 2026-09-11 rev: 89b31bb4] At that revision nothing executed `scripts/mutation-gate.sh`; the kill-rate gate ran only when a human invoked it by hand.
