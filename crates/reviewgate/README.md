A **code-review gate** [test: crates/reviewgate/tests/integration.rs::help_describes_the_gate].

It registers one Stop hook, which runs the bundled `reviewgate review` binary [test: crates/reviewgate/tests/prose_claims.rs::claim_hooks_one_stop].

- `off` is the default `mode`: with no config file the stop is allowed with no review, no block and no injected rubric; reviewing is opt-in via `mode = "inject"` or `mode = "subprocess"` [test: crates/reviewgate/tests/prose_claims.rs::claim_default_mode_off] [test: crates/reviewgate/tests/default_mode_off.rs::no_config_default_is_off_and_does_not_review].
- In `inject` mode the first stop with a reviewable change in a session is blocked [test: crates/reviewgate/tests/prose_claims.rs::claim_inject_once_per_diff_state].
- The injected reason carries the review **rubric** verbatim, lists the files and asks for a review [test: crates/reviewgate/src/review.rs::inject_reason_still_lists_the_files_carries_the_rubric_and_asks_for_a_review].
- The injected reason describes the files as the uncommitted changes of the working tree, says the session attribution is inferred from the transcript, and states that their authorship was not strictly verified [test: crates/reviewgate/src/review.rs::inject_reason_describes_the_files_as_unattributed_uncommitted_changes].
- `subprocess` mode runs `reviewer_cmd` (default `claude -p`) as the reviewer [test: crates/reviewgate/src/config.rs::project_reviewer_cmd_is_gated_behind_trust] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::reviewer_that_prints_lgtm_is_allowed_and_clean].
- In `subprocess` mode a reviewer that answers LGTM lets the stop through [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::reviewer_that_prints_lgtm_is_allowed_and_clean], and reported findings are rendered verbatim in the block reason [test: crates/reviewgate/src/review.rs::subprocess_reason_still_lists_every_file_and_renders_the_findings_verbatim].

A stop whose diff matches the one it last forced a review of is allowed through (log tag `already-reviewed`) [test: crates/reviewgate/tests/prose_claims.rs::claim_inject_once_per_diff_state].
A diff that changed after an `already-reviewed` allow is blocked again, never waived as a repeat of the earlier block [test: crates/reviewgate/tests/repro_inject_changed_diff_waived.rs::changed_diff_after_already_reviewed_is_blocked_not_waived].
The number of blocking rounds is capped by `max_attempts`, so the agent is never trapped [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::inject_giveup_keeps_the_plain_giveup_tag]; the inject-mode give-up is announced on stderr and as a `systemMessage`, since it lets the latest diff through unreviewed [test: crates/reviewgate/tests/repro_inject_giveup_silent.rs::inject_giveup_is_visibly_signalled].
The default of `max_attempts` is 2 [test: crates/reviewgate/tests/prose_claims.rs::claim_default_max_attempts_2].

When there is nothing to review (not a git repo, no reviewable file changed) the stop is **allowed** [test: crates/reviewgate/tests/git_probe_wiring.rs::genuine_non_repo_with_real_git_still_allows] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::valid_include_that_excludes_the_change_allows_normally] [test: crates/reviewgate/tests/backlog_7d56f1e3_unknown_mode_blocks.rs::known_mode_on_clean_repo_allows].
When reviewgate *cannot determine* the change it does **not** allow: a git command that errors inside a real repo leaves the changed set undetermined and **blocks** (bounded by `max_attempts`) [test: crates/reviewgate/tests/git_probe_wiring.rs::unspawnable_git_in_a_repo_blocks_the_stop] [test: crates/reviewgate/src/review.rs::failed_git_scan_blocks_it_does_not_allow] [test: crates/reviewgate/src/review.rs::failed_git_scan_gives_up_after_max_attempts_but_never_traps].
Likewise, a reviewable change whose diff text comes back empty (such as under a `.gitattributes` diff driver that prints nothing) is undetermined, not an empty diff, and **blocks** (bounded by `max_attempts`) [test: crates/reviewgate/tests/repro_diff_driver_empty.rs::silent_diff_driver_change_is_not_allowed_as_empty].
A config file that cannot be read/parsed does not fall back to the `off` default — it prints a WARNING and blocks `config-invalid` [test: crates/reviewgate/tests/prose_claims.rs::claim_broken_config_still_gates] [test: crates/reviewgate/tests/default_mode_off.rs::unparseable_existing_config_blocks_config_invalid].
The `review` hook runs under `harness_core::gate::run::run_guarded` [test: crates/reviewgate/tests/prose_claims.rs::claim_panic_fails_closed]: a panic on the first stop blocks (fail **closed**), and only a panic on re-entry (`stop_hook_active`) falls through to allow, so a broken reviewgate cannot trap a turn forever [test: crates/harness-core/src/gate/run.rs::guard_maps_panic_to_action] [test: crates/harness-core/src/gate/run.rs::fail_closed_block_json_is_a_block_decision].
Lockfiles are excluded by default [test: crates/reviewgate/src/review.rs::lockfile_excluded_by_default].
`node_modules`, `target` and generated files are excluded by default [test: crates/reviewgate/tests/prose_claims.rs::claim_default_excludes].

**Whose changes.** Only the files **this session wrote** are reviewed (`harness_core::attribution::attribute_own_writes`, read from the hook's `transcript_path`, shared with tdd): a changed file is kept when this session's transcript or a subagent sidechain records an `Edit`/`Write`/`MultiEdit`/`NotebookEdit` of it, or a `Bash` command naming its path outside a read-only statement (merely reading it with `git diff`, `grep` or `cat` does not count) [test: crates/harness-core/tests/attribution_read_only_bash.rs::group1_read_only_mentions_do_not_attribute] [test: crates/harness-core/tests/attribution_read_only_bash.rs::group2_writing_mentions_attribute]; every other changed file is excluded as not attributed to this session [test: crates/reviewgate/tests/session_scope_own.rs::foreign_only_does_not_block] [test: crates/reviewgate/tests/session_scope_own.rs::own_edit_reviews_only_own_file_and_names_the_excluded_one] [test: crates/reviewgate/tests/session_scope_own.rs::own_bash_relative_path_is_reviewed] [test: crates/reviewgate/tests/session_attribution.rs::an_unattributed_file_is_excluded_from_the_review_set].
Known gap: a file written by a command that never names it (`cargo fmt`, a shell glob, a script) is excluded too.
The exclusion is appended to the block reason [test: crates/reviewgate/tests/session_attribution.rs::peer_written_file_is_excluded_from_the_review_set].
The exclusion is printed to stderr [test: crates/reviewgate/tests/prose_claims.rs::claim_attribution_exclusion_stderr].
When attribution is undetermined (no/unreadable transcript) every changed file is reviewed [test: crates/reviewgate/tests/session_attribution.rs::unreadable_transcript_does_not_narrow_the_review_set] [test: crates/reviewgate/tests/session_attribution.rs::empty_transcript_path_does_not_narrow_the_review_set].

A *reviewer* that itself fails is **not** the same as a clean review, so it does not silently allow — that would turn a broken reviewer into a bypass: [test: crates/reviewgate/src/review.rs::reviewer_error_blocks_it_does_not_allow]

- A reviewer subprocess that cannot be spawned / times out / emits unusable output (empty stdout, non-UTF-8 stdout) → **block** (bounded by `max_attempts`) [test: crates/reviewgate/src/review.rs::run_reviewer_reports_error_for_unspawnable_command] [test: crates/reviewgate/tests/prose_claims.rs::claim_key_fields_defaults] [test: crates/reviewgate/src/review.rs::reviewer_timeout_blocks_it_does_not_allow] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::non_utf8_reviewer_stdout_is_undetermined_not_clean] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::silent_reviewer_exit_zero_empty_stdout_is_undetermined_not_clean] [test: crates/reviewgate/src/review.rs::reviewer_error_gives_up_after_max_attempts_but_never_traps].
- A reviewer whose prompt could not be delivered → **block** [test: crates/reviewgate/tests/prose_claims.rs::claim_prompt_undelivered_blocks].
- A diff too large to review whole (truncated to `max_diff_bytes`) has an unreviewed tail → **block** (bounded by `max_attempts`) [test: crates/reviewgate/src/review.rs::truncated_diff_blocks_it_does_not_allow] [test: crates/reviewgate/src/review.rs::truncated_diff_gives_up_after_max_attempts_but_never_traps] [test: crates/reviewgate/src/git.rs::over_limit_is_flagged_and_marked].
- A diff whose content fetch partly failed (a `git diff` / `git ls-files` call errored, or an untracked file could not be read) is incomplete → **block** (bounded, same path as a failed change scan) [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::partial_untracked_fetch_failure_does_not_certify_the_diff_as_clean] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::partial_unstaged_diff_failure_does_not_certify_the_diff_as_clean] [test: crates/reviewgate/tests/backlog_922f0a96_diff_fetch_failure_wiring.rs::unreadable_untracked_file_blocks_via_fetch_failed].
- When the independent reviewer still reports findings after `max_attempts`, the stop is allowed with a stderr WARNING, the distinct log tag `review-giveup`, and an overwatch violation event [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::violation_giveup_is_loud_distinctly_tagged_and_recorded_as_a_violation].
- A malformed `include` glob does not narrow the review, and stderr names the pattern [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::partially_malformed_include_glob_does_not_silently_narrow_scope].
- A malformed `exclude` glob excludes nothing, with a stderr WARNING [test: crates/reviewgate/tests/prose_claims.rs::claim_malformed_exclude].
- A config file that cannot be read/parsed is reported (stderr WARNING, and `status` shows `FAILED to load`) [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::broken_home_config_warns_on_review_and_reports_failed_in_status] [test: crates/reviewgate/tests/prose_claims.rs::claim_broken_config_still_gates].

The truncation block reason names `reviewgate skip --reason "<why>"`, `REVIEWGATE_DISABLE=1` and raising `max_diff_bytes` [test: crates/reviewgate/src/review.rs::truncated_diff_blocks_it_does_not_allow]; the reviewer-findings block reason names `reviewgate skip --reason` and `REVIEWGATE_DISABLE=1` [test: crates/reviewgate/src/review.rs::subprocess_reason_remains_a_real_instruction_with_escape_hatches].

## Install

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install reviewgate@yukineko
```

```
cargo install --path .
reviewgate init          # write a starter ./reviewgate.toml [test: crates/reviewgate/tests/prose_claims.rs::claim_init_force]
reviewgate install       # wire the Stop hook into ~/.claude/settings.json [test: crates/reviewgate/tests/prose_claims.rs::claim_install_uninstall]
```

- `reviewgate review` — the Stop hook (reads the hook JSON, including `session_id`, on stdin) [test: crates/reviewgate/tests/integration.rs::review_hook_valid_payload_exits_zero] [test: crates/reviewgate/tests/session_scoped_skip.rs::a_skip_issued_by_another_session_does_not_wave_my_reviewgate_gate_through].
- `reviewgate install [--dry-run]` / `uninstall [--dry-run]` — manage the hook [test: crates/reviewgate/tests/prose_claims.rs::claim_install_uninstall].
- `reviewgate init [--force]` — write a starter `reviewgate.toml` [test: crates/reviewgate/tests/prose_claims.rs::claim_init_force].
- `reviewgate status` — show the resolved config and what would be reviewed now [test: crates/reviewgate/tests/prose_claims.rs::claim_status_shows_config_and_files].
- `reviewgate trust` — trust the current project so its `./reviewgate.toml` (including `reviewer_cmd`) is honored [test: crates/reviewgate/tests/prose_claims.rs::claim_trust_cli_honors_project_config].
- Until trusted, a repo-shipped config is ignored [test: crates/reviewgate/src/config.rs::project_reviewer_cmd_is_gated_behind_trust] [test: crates/reviewgate/src/trust_worktree_tests.rs::linked_worktree_of_untrusted_main_still_ignores_project_config].
- `reviewgate skip --reason "<why>"` — one-shot [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_consumed_once], session-scoped skip (see below) [test: crates/reviewgate/tests/session_scoped_skip.rs::a_skip_issued_by_another_session_does_not_wave_my_reviewgate_gate_through].

Run `reviewgate review` by hand (no stdin) for a dry check: it exits 1 on a block and prints no hook JSON [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::empty_stdin_keeps_manual_cli_mode_exit_1_on_block].

See [`reviewgate.example.toml`](reviewgate.example.toml) [test: crates/reviewgate/tests/prose_claims.rs::claim_example_toml].
Project `./reviewgate.toml` wins over `~/.reviewgate/config.toml` over built-in defaults — but only once the project root is **trusted** (`reviewgate trust`), since `reviewer_cmd` runs as a subprocess [test: crates/reviewgate/src/config.rs::project_reviewer_cmd_is_gated_behind_trust].

Key fields: `mode`, `max_attempts`, `reset_after_secs` (default 600; the per-session attempt counter resets after this idle gap), `min_changed_files`, `max_diff_bytes` (default 200000), `include`/`exclude` globs, `rubric`, `state_dir`, and (subprocess) `reviewer_cmd` / `reviewer_timeout_secs` [test: crates/reviewgate/tests/prose_claims.rs::claim_key_fields_defaults].
A value of `0` for `max_attempts` / `min_changed_files` is clamped to 1, and `0` for `max_diff_bytes` falls back to the default [test: crates/reviewgate/tests/prose_claims.rs::claim_zero_clamp].

`mode` accepts `"off"`, `"inject"` or `"subprocess"` (aliases `reviewer` / `independent`) [test: crates/reviewgate/tests/default_mode_off.rs::explicit_off_parses_and_does_not_review] [test: crates/reviewgate/src/config.rs::known_modes_parse_without_err] [test: crates/reviewgate/src/config.rs::known_modes_loaded_from_config_leave_config_invalid_none].
Any other value is **config-invalid**: it is not mapped to either mode, and the stop is blocked (`config-invalid`, naming the value) — even when nothing is reviewable [test: crates/reviewgate/tests/backlog_7d56f1e3_unknown_mode_blocks.rs::unknown_mode_blocks_config_invalid_on_clean_repo] [test: crates/reviewgate/tests/backlog_7d56f1e3_unknown_mode_blocks.rs::unknown_mode_blocks_config_invalid_with_reviewable_change] [test: crates/reviewgate/src/config.rs::unknown_mode_is_err_naming_the_value].
`reviewgate status` shows the state; `reviewgate skip --reason ...` or `REVIEWGATE_DISABLE=1` are the escape hatches [test: crates/reviewgate/tests/prose_claims.rs::claim_status_and_escapes_on_invalid_mode].

The plugin also ships `agents/reviewgate-typing.md`, a subagent definition whose tools include Read and no Edit or Write [test: crates/reviewgate/tests/prose_claims.rs::claim_typing_agent_readonly].
The `review` hook does not invoke it; call it explicitly [test: crates/reviewgate/tests/prose_claims.rs::claim_review_hook_does_not_invoke_agent].

- One-shot: `reviewgate skip --reason "<why>"` [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_reason_required] [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_consumed_once].
- A reason is required [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_reason_required].
- The skip applies only to the session that issued it [test: crates/reviewgate/tests/session_scoped_skip.rs::a_skip_issued_by_another_session_does_not_wave_my_reviewgate_gate_through].
- The skip is consumed once [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_consumed_once].
- The skip is recorded in the gate log at both issue and consumption [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_logged].
- The old shared `.reviewgate-skip` file in the project root was removed: it no longer waves a stop through [test: crates/reviewgate/tests/session_scoped_skip.rs::an_unattributed_project_root_marker_no_longer_waves_a_reviewgate_stop_through].
- Off entirely: `enabled = false` in config, or `REVIEWGATE_DISABLE=1` [test: crates/reviewgate/tests/prose_claims.rs::claim_disable_switches].

Each decision appends a JSONL line to `<state_dir>/log.jsonl` (`~/.reviewgate/state/log.jsonl` by default) [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::reviewer_that_prints_lgtm_is_allowed_and_clean] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::inject_giveup_keeps_the_plain_giveup_tag].

License: MIT [test: crates/reviewgate/tests/prose_claims.rs::claim_license_mit]
