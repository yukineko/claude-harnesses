On every `Stop`, propguard derives *semantic properties* (invariants) from the current task's `done_criteria` [test: crates/propguard/src/derive.rs::derives_matching_properties_from_criteria] [test: crates/propguard/src/derive.rs::derivation_is_capped_at_max] [test: crates/propguard/src/derive.rs::thin_criteria_are_padded_to_the_minimum] and checks the generated code against them before the agent is allowed to declare a turn done [test: crates/propguard/tests/integration.rs::with_criteria_inject_blocks_first_stop_but_exits_zero] [test: crates/propguard/tests/integration.rs::block_appends_propguard_signed_violation].

[record: 2026-07-01 rev: 10de5291] Property derivation follows **PGS** (Property-Generated Solver, [arXiv:2506.18315](https://arxiv.org/pdf/2506.18315)).

One Stop hook runs the bundled `propguard check` binary [test: crates/propguard/tests/prose_claims.rs::claim_one_stop_hook].

The binary deterministically *derives* the properties and enforces the count→threshold block [test: crates/propguard/src/derive.rs::derivation_is_deterministic] [test: crates/propguard/src/gate.rs::below_threshold_is_the_single_comparison]; the semantic *judgement* of whether each property holds is done either by the agent already running on your subscription (inject mode) or by a headless checker you configure (subprocess mode) [test: crates/propguard/src/gate.rs::inject_mode_block_reason_states_that_propguard_itself_did_not_judge] [test: crates/propguard/tests/integration.rs::block_appends_propguard_signed_violation].

Derivation is a deterministic rule set over a bilingual keyword taxonomy, capped at `max_properties` [test: crates/propguard/src/derive.rs::derivation_is_deterministic] [test: crates/propguard/src/derive.rs::japanese_criteria_derive_properties] [test: crates/propguard/src/derive.rs::derivation_is_capped_at_max].

| id | invariant [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
|----|-----------|
| `error-path` | 失敗パスは panic せず Err/エラーを返す (error handling) [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
| `output-schema` | 出力スキーマ/フォーマットが安定している (schema stability) [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
| `determinism` | 決定論的: 同一入力は同一出力を返す (determinism) [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
| `idempotence` | 冪等: 複数回実行しても結果が変わらない (idempotence) [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
| `bounds-monotonicity` | 境界・単調性・閾値が守られる (bounds/monotonicity) [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
| `no-partial-write` | 部分書き込みが起きない (atomic / no-partial-write) [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |

Properties whose keywords appear in the done_criteria are derived; if fewer than `min_properties` match, the set is padded with the baseline *universal* invariants (`error-path`, `output-schema`, `determinism`) [test: crates/propguard/src/derive.rs::derives_matching_properties_from_criteria] [test: crates/propguard/src/derive.rs::thin_criteria_are_padded_to_the_minimum].

Preview the derivation offline [test: crates/propguard/tests/integration.rs::derive_subcommand_lists_properties]:

```
propguard derive "must be idempotent, never panic, keep the output schema stable"
```

done_criteria is sourced, in priority order, from [test: crates/propguard/src/derive.rs::env_criteria_take_priority] [test: crates/propguard/tests/prose_claims.rs::claim_criteria_file_default]:

1. the `PROPGUARD_CRITERIA` environment variable [test: crates/propguard/src/derive.rs::env_criteria_take_priority],
2. a `criteria_file` (default `.propguard-criteria`) in the project root [test: crates/propguard/tests/prose_claims.rs::claim_criteria_file_default],
3. the inline `done_criteria` value in `propguard.toml` [test: crates/propguard/src/derive.rs::inline_criteria_used_when_no_env_or_file].

With no source found, **every stop is allowed** [test: crates/propguard/tests/integration.rs::no_criteria_allows_the_stop] [test: crates/propguard/src/derive.rs::no_criteria_source_is_none].

| mode | what it does [test: crates/propguard/src/config.rs::known_modes_parse_and_leave_config_valid] |
|------|--------------|
| `inject` | Blocks the stop once per new diff [test: crates/propguard/src/gate.rs::inject_first_pass_blocks_as_unverified] [test: crates/propguard/src/gate.rs::inject_block_arms_trust_after_one_block] and injects the **property checklist** [test: crates/propguard/src/gate.rs::inject_mode_block_reason_still_lists_the_derived_properties_and_target_files]; the checklist describes the checked files as uncommitted changes whose author propguard does not verify [test: crates/propguard/src/gate.rs::inject_mode_block_reason_describes_the_files_as_unattributed_uncommitted_changes]. |
| `subprocess` | Runs `checker_cmd` (default `claude -p`) [test: crates/propguard/src/trust_worktree_tests.rs::linked_worktree_of_untrusted_main_still_ignores_project_config] as a checker that emits one `PROP <id>: PASS\|FAIL` line per property; propguard counts the PASSes [test: crates/propguard/src/gate.rs::parse_counts_only_explicit_pass] [test: crates/propguard/src/gate.rs::zero_exit_with_a_pass_line_still_parses_as_verified]. |

The default mode is `inject` [test: crates/propguard/tests/prose_claims.rs::claim_default_mode_inject].

At `gate::below_threshold` the stop is **blocked iff `satisfied < threshold`** [test: crates/propguard/src/gate.rs::below_threshold_is_the_single_comparison] [test: crates/propguard/src/gate.rs::below_threshold_blocks] [test: crates/propguard/src/gate.rs::at_threshold_allows_and_records_hash].
In inject mode a new diff is *unverified*, so it blocks once to inject the checklist; on the next stop the same diff is allowed — *trust-after-one-block*: propguard does not observe whether the agent actually checked the properties, it only guarantees the checklist was shown once [test: crates/propguard/tests/prose_claims.rs::claim_already_verified] [test: crates/propguard/src/gate.rs::inject_mode_block_reason_states_that_propguard_itself_did_not_judge] [test: crates/propguard/src/gate.rs::inject_first_pass_blocks_as_unverified] [test: crates/propguard/src/gate.rs::inject_block_arms_trust_after_one_block].
In subprocess mode the PASS count is compared directly [test: crates/propguard/src/gate.rs::subprocess_mode_block_reason_keeps_the_count_a_checker_actually_produced].
The threshold is clamped to the number of properties actually derived [test: crates/propguard/src/gate.rs::effective_threshold_is_clamped_to_property_count].

propguard hashes `(diff, properties)` [test: crates/propguard/tests/prose_claims.rs::claim_hash_diff_and_props].
The hash is recorded on a subprocess PASS (`properties-satisfied`) or on inject mode's trust-after-one-block [test: crates/propguard/src/gate.rs::at_threshold_allows_and_records_hash] [test: crates/propguard/src/gate.rs::inject_block_arms_trust_after_one_block] [test: crates/propguard/src/gate.rs::subprocess_block_does_not_arm_already_verified].
A stop matching a recorded hash is allowed (`already-verified`) [test: crates/propguard/tests/prose_claims.rs::claim_already_verified].
A subprocess below-threshold result records no hash, so the checker re-runs on the next stop [test: crates/propguard/src/gate.rs::subprocess_block_does_not_arm_already_verified].
Each blocked round counts toward `max_attempts` (default 2; the counter resets after `reset_after_secs` of idle, default 600) [test: crates/propguard/tests/prose_claims.rs::claim_max_attempts_defaults].
Once exceeded, a below-threshold result is **allowed** with tag `giveup` (properties NOT satisfied) [test: crates/propguard/src/gate.rs::below_threshold_gives_up_after_max_attempts].

- No git repo, nothing checkable, no done_criteria source configured at all → **allow** [test: crates/propguard/tests/git_probe_wiring.rs::genuine_non_repo_with_real_git_still_allows] [test: crates/propguard/src/gate.rs::a_genuinely_clean_repo_still_allows] [test: crates/propguard/tests/integration.rs::no_criteria_allows_the_stop].
- A config file (`propguard.toml` / `~/.propguard/config.toml`) that exists but cannot be read or parsed → **block** (tag `config-unreadable`, no give-up) — it is never silently replaced by built-in defaults [test: crates/propguard/tests/prose_claims.rs::claim_no_giveup_config_errors] [test: crates/propguard/src/config.rs::ca_propguard_03_unreadable_or_unparseable_project_toml_blocks_fail_closed] [test: crates/propguard/src/gate.rs::config_load_error_blocks_fail_closed].
- A config that parses but has an unrecognised `mode` value → **block** (tag `config-invalid`, no give-up) — it is not mapped to either mode [test: crates/propguard/tests/prose_claims.rs::claim_no_giveup_config_errors] [test: crates/propguard/tests/integration.rs::unknown_mode_blocks_config_invalid_naming_the_value] [test: crates/propguard/src/config.rs::unknown_mode_is_recorded_as_config_invalid_not_mapped] [test: crates/propguard/src/gate.rs::config_invalid_blocks_naming_the_value_and_known_mode_does_not].
- A `criteria_file` that exists but cannot be read, with no inline `done_criteria` to fall back to → **block** (tag `criteria-unreadable`, no give-up) [test: crates/propguard/tests/prose_claims.rs::claim_no_giveup_config_errors] [test: crates/propguard/src/gate.rs::ca_propguard_01_unreadable_criteria_file_blocks_fail_closed] [test: crates/propguard/src/derive.rs::unreadable_criteria_file_falls_through_to_inline].
- A `checker-error-giveup` is recorded in overwatch's violation ledger; when it recurs across sessions it escalates to a **block** (`checker-outage-systemic`), and an unreadable ledger blocks too (`checker-outage-undetermined`) [test: crates/propguard/tests/prose_claims.rs::claim_outage_ledger_e2e] [test: crates/propguard/src/gate.rs::escalate_giveup_systemic_becomes_block].
  The escalation mapper (`gate::escalate_giveup_on_systemic`) also maps a systemic `git-scan-failed-giveup` or `diff-read-failed-giveup` to a block [test: crates/propguard/src/gate.rs::ca_propguard_02_git_scan_giveup_also_escalates_when_systemic] [test: crates/propguard/src/gate.rs::ca_propguard_02_diff_read_giveup_also_escalates_when_systemic] [test: crates/propguard/src/gate.rs::ca_propguard_02_outage_scan_covers_new_giveups_but_not_truncated].
  `truncated-giveup` is not an outage and is not escalated [test: crates/propguard/src/gate.rs::ca_propguard_02_outage_scan_covers_new_giveups_but_not_truncated].
- A checker that crashes / times out / emits unusable output → **block** (bounded), then give up [test: crates/propguard/src/gate.rs::shell_wrapped_timeout_kills_the_real_backgrounded_checker] [test: crates/propguard/src/gate.rs::checker_error_blocks_it_does_not_allow] [test: crates/propguard/src/gate.rs::checker_error_gives_up_after_max_attempts_but_never_traps] [test: crates/propguard/src/gate.rs::nonzero_exit_with_a_pass_line_on_stdout_is_an_error_not_a_verdict] [test: crates/propguard/src/gate.rs::parse_unrelated_output_is_error_not_all_pass].
- A `git` command that errors inside a real repo (changed set undetermined) → **block** (bounded, tag `git-scan-failed`), then give up (`git-scan-failed-giveup`) [test: crates/propguard/src/gate.rs::failed_git_scan_blocks_it_does_not_allow] [test: crates/propguard/src/gate.rs::failed_git_scan_gives_up_after_max_attempts_but_never_traps] [test: crates/propguard/tests/git_probe_wiring.rs::unspawnable_git_in_a_repo_blocks_with_the_scan_failed_reason].
- A truncated (too-large) diff has an unchecked tail → **block** (bounded), then give up [test: crates/propguard/src/gate.rs::truncated_diff_blocks_then_gives_up].
- A diff that could not be **read** — any of `git diff`, `git diff --cached`, the untracked `ls-files`, or an untracked file's body failing to decode → **block** (bounded, tag `diff-read-failed`), then give up [test: crates/propguard/src/gate.rs::a_diff_that_could_not_be_read_blocks_it_does_not_allow_as_empty] [test: crates/propguard/src/gate.rs::a_partially_read_diff_is_not_handed_on_as_complete] [test: crates/propguard/src/gate.rs::an_untracked_file_whose_body_could_not_be_read_blocks_as_undetermined] [test: crates/propguard/src/git.rs::diff_text_with_an_unreadable_untracked_scan_is_undetermined_not_an_empty_diff] [test: crates/propguard/src/gate.rs::unreadable_diff_gives_up_after_max_attempts_but_never_traps].
  This covers the partial case too: if one of those reads succeeds and another does not, the readable half is *not* handed onward as if it were the whole diff [test: crates/propguard/src/gate.rs::a_partially_read_diff_is_not_handed_on_as_complete] [test: crates/propguard/src/git.rs::diff_text_with_one_unreadable_subcommand_is_undetermined_not_a_partial_diff].
  An unread diff is undetermined, never "empty ⇒ nothing changed" [test: crates/propguard/src/gate.rs::a_diff_that_could_not_be_read_blocks_it_does_not_allow_as_empty] [test: crates/propguard/src/git.rs::diff_text_of_an_undecodable_diff_is_undetermined_not_empty].
- A genuine harness panic → fail-closed **block** on the first stop: the `check` Stop entry goes through `harness_core::gate::run::run_guarded`, and a panic under it emits a block decision [test: crates/harness-core/tests/prose_claims.rs::claim_prohib_run_guarded] [test: crates/harness-core/tests/prose_claims.rs::claim_prohib_run_guarded_panic_carries_a_block_decision].

The old shared `.propguard-skip` file in the project root no longer waves a stop through [test: crates/propguard/tests/session_scoped_skip.rs::an_unattributed_project_root_marker_no_longer_waves_a_propguard_stop_through].
Escape hatches: `propguard skip --reason "<why>"` — one-shot, reason required, scoped to the session that issued it [test: crates/propguard/tests/session_scoped_skip.rs::a_skip_issued_by_another_session_does_not_wave_my_propguard_gate_through], and recorded in the gate log at both issue and consumption [test: crates/propguard/tests/prose_claims.rs::claim_skip_contract].
Or `enabled = false` in the config [test: crates/propguard/tests/prose_claims.rs::claim_enabled_false], or `PROPGUARD_DISABLE=1` [test: crates/propguard/tests/integration.rs::disable_env_allows].

## Install the plugin

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install propguard@yukineko
```

## Install from source

```
cargo build --release -p propguard
./target/release/propguard install     # wires the Stop hook into ~/.claude/settings.json [test: crates/propguard/tests/prose_claims.rs::claim_install_wires_stop]
propguard init                         # writes a starter ./propguard.toml [test: crates/propguard/tests/prose_claims.rs::claim_init_starter]
```

`propguard uninstall` removes the Stop hook [test: crates/propguard/tests/prose_claims.rs::claim_uninstall_removes].
`propguard --protects` prints what the gate protects, from what, and why [test: crates/propguard/tests/protection_contract.rs::protects_prints_three_labelled_nonempty_lines_and_exits_zero].
Session state lives under `~/.propguard/state` (config key `state_dir`; the `PROPGUARD_STATE_DIR` env var overrides it) [test: crates/propguard/tests/prose_claims.rs::claim_state_dir].

See [`propguard.example.toml`](./propguard.example.toml) [test: crates/propguard/tests/prose_claims.rs::claim_example_toml].
Key knobs: `mode`, `min_properties`/`max_properties` (`max_properties` is capped at 5; `min_properties` = 1 is accepted, not raised to 3), `threshold`, `max_attempts`, `reset_after_secs`, `min_changed_files`, `max_diff_bytes`, `criteria_file`, `include`/`exclude`, `enabled`, and (subprocess mode) `checker_cmd` / `checker_timeout_secs` [test: crates/propguard/tests/prose_claims.rs::claim_key_knobs].
A project `propguard.toml` is honored only once the root is **trusted** [test: crates/propguard/src/trust_worktree_tests.rs::linked_worktree_of_untrusted_main_still_ignores_project_config] [test: crates/propguard/src/trust_worktree_tests.rs::trusted_plain_checkout_still_loads_project_config].
Trust a root with `propguard trust` [test: crates/propguard/tests/prose_claims.rs::claim_trust_cmd].

`propguard status` shows the resolved config and the properties derived for the current task [test: crates/propguard/tests/prose_claims.rs::claim_status_shows].

In `subprocess` mode, `checker_cmd` runs as a real OS subprocess and is bounded by a timeout [test: crates/propguard/src/gate.rs::checker_stdout_flood_without_draining_stdin_does_not_hang] [test: crates/propguard/src/gate.rs::shell_wrapped_timeout_kills_the_real_backgrounded_checker]:

- **Default: 300 seconds** [test: crates/propguard/src/config.rs::checker_timeout_secs_defaults_to_300].
- Configurable via the top-level `checker_timeout_secs` key in `propguard.toml` (or `~/.propguard/config.toml`) [test: crates/propguard/tests/prose_claims.rs::claim_timeout_home_config] [test: crates/propguard/src/config.rs::checker_timeout_secs_load_from_toml_text_overrides_default]:

  ```toml
  checker_cmd = "claude -p"
  checker_timeout_secs = 45   # override the 300s default [test: crates/propguard/src/config.rs::checker_timeout_secs_is_overridable_from_file_config]
  ```

- A value of `0` is sanitized back to the 300s default rather than disabling the timeout [test: crates/propguard/src/config.rs::claim_timeout_zero_sanitized].
  An invalid/negative value fails TOML parsing, which makes the whole config unreadable → **block** (`config-unreadable`), not a silent default [test: crates/propguard/tests/prose_claims.rs::claim_timeout_negative_blocks].
- **On timeout, propguard kills the checker's backgrounded descendants too**, not just the immediate child: a grandchild that a shell-wrapped `checker_cmd` backgrounds with `&` does not outlive the timeout [test: crates/propguard/tests/prose_claims.rs::claim_timeout_kills_tree].
  A descendant that keeps the stdout pipe open does not hang the stop [test: crates/propguard/src/gate.rs::lingering_stdout_holder_does_not_hang_the_read].
- A timeout is reported as a checker `Error` (never silently "all pass") and goes through the same fail-closed, bounded `max_attempts` give-up path as any other checker failure [test: crates/propguard/src/gate.rs::shell_wrapped_timeout_kills_the_real_backgrounded_checker] [test: crates/propguard/src/gate.rs::checker_error_blocks_it_does_not_allow] [test: crates/propguard/src/gate.rs::checker_error_gives_up_after_max_attempts_but_never_traps].
