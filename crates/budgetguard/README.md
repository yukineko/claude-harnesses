`budgetguard gate` is wired to the Claude Code **Stop** hook; on each stop it [test: crates/budgetguard/tests/prose_claims.rs::claim_hooks_json_stop_gate]:

1. takes this session's per-model token usage — from gauge's persisted
   session record when it is at least as new as the transcript, otherwise by
   re-parsing the transcript (JSONL), including sub-agent spend [test: crates/budgetguard/src/gate.rs::fresh_record_matches_transcript_parse_cost] [test: crates/budgetguard/src/gate.rs::stale_record_falls_back_to_transcript_parse] [test: crates/budgetguard/tests/prose_claims.rs::claim_cost_source_freshness] [test: crates/budgetguard/src/gate.rs::unreadable_subagents_dir_is_undetermined_and_blocks_when_a_limit_is_armed];
2. estimates the USD cost using the built-in pricing table [test: crates/budgetguard/src/gate.rs::fresh_record_matches_transcript_parse_cost];
3. updates a local **daily ledger** (`~/.budgetguard/state/ledger.json`) with
   this session's latest cost (not a sum over stops) [test: crates/budgetguard/tests/prose_claims.rs::claim_ledger_latest_cost], under a ledger lock: while
   another session holds the lock the ledger is not written and the day total
   is unmeasured [test: crates/budgetguard/tests/prose_claims.rs::claim_ledger_lock_serializes]; the day key is the **local** date [test: crates/budgetguard/tests/prose_claims.rs::claim_day_key_local_midnight];
4. checks the session total and the day's cumulative total against configured
   limits [test: crates/budgetguard/src/gate.rs::daily_block_triggers_independently_of_session] [test: crates/budgetguard/tests/prose_claims.rs::claim_day_cumulative] [test: crates/budgetguard/src/gate.rs::block_is_inclusive_at_threshold_and_beats_warn];
5. **within limits** → exits 0, the stop proceeds;
   **warn threshold** → emits `{"additionalContext":"…"}` (advisory only, no block);
   **block threshold** → emits `{"decision":"block","reason":"…"}` asking the
   agent to save and commit before the turn ends [test: crates/budgetguard/tests/prose_claims.rs::claim_verdict_outputs].
   Thresholds are inclusive (spend at or above the limit); block beats warn [test: crates/budgetguard/src/gate.rs::warn_is_inclusive_at_threshold] [test: crates/budgetguard/src/gate.rs::block_is_inclusive_at_threshold_and_beats_warn].

It also reports **cache-hit health** from gauge's session record (to stderr): once `input + cache_read` reaches `[cache].min_tokens` (default
200000), a hit rate below `[cache].min_rate` (default 0.5; out-of-range values
fall back to the default) is promoted to an advisory `additionalContext`
warning when nothing louder is being said [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_promoted_warning] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_below_floor_not_promoted] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_out_of_range_rate_uses_default] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_min_tokens_default_boundary] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_min_rate_default_and_inrange] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_never_blocks_louder_wins]. It never blocks [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_promoted_warning].

A blocking stop also records an overwatch violation event [test: crates/budgetguard/tests/prose_claims.rs::claim_block_records_violation].

### Limits when none is configured

A limit of `0` turns off that one axis [test: crates/budgetguard/src/gate.rs::zero_threshold_means_disabled] [test: crates/budgetguard/src/config.rs::one_set_limit_keeps_the_others_off]. A gate with **no** limit at all
(no config file, or every `[session]` / `[daily]` warn/block limit `0`) is not
"no limit". budgetguard sets the limits itself:

- **derived**: if the ledger holds at least 7 past days (today is excluded),
  the limits come from that history, with Tukey outlier removal (Q1/Q3 by
  linear interpolation, fence = Q3 + 1.5 × IQR). Daily block = the fence over
  per-day totals. Daily warn = p90 of the days at or below the fence [test: crates/budgetguard/src/config.rs::derive_limits_uses_the_tukey_fence_and_ignores_today] [test: crates/budgetguard/src/config.rs::an_absent_config_with_history_derives_its_limits] [test: crates/budgetguard/tests/unset_limits_are_derived.rs::absent_config_with_history_gate_blocks_over_the_derived_daily_limit] [test: crates/budgetguard/tests/unset_limits_are_derived.rs::all_zero_config_with_history_gate_blocks_over_the_derived_daily_limit];
- **default**: with a shorter history, the built-in limits apply: daily warn
  $1336 / block $1871. These were computed on 2026-10-19 from the
  maintainer's ledger with the same method [test: crates/budgetguard/src/config.rs::derive_limits_needs_seven_past_days] [test: crates/budgetguard/src/config.rs::an_absent_config_with_no_history_gets_the_default_limits] [test: crates/budgetguard/tests/unset_limits_are_derived.rs::absent_config_no_history_check_reports_default_limits];
- the **session axis** gets no derived warn and no derived block. The session
  fence (the Tukey fence over every past per-session entry; the default is
  $306) is reported as `session_fence_usd` by `check` / `status --json`, for
  information only [test: crates/budgetguard/tests/unset_limits_are_derived.rs::absent_config_derives_no_session_warning_but_reports_the_fence]. A session
  warn/block applies only when the config sets one;
- a ledger that exists but cannot be read or parsed, or that holds a
  non-finite or negative cost, is **undetermined**: the gate blocks and
  `check`/`status` exit 2. It is never silently replaced by the defaults [test: crates/budgetguard/src/config.rs::an_absent_config_with_a_corrupt_ledger_is_undetermined] [test: crates/budgetguard/src/config.rs::derive_limits_rejects_a_non_finite_cost] [test: crates/budgetguard/tests/unset_limits_are_derived.rs::absent_config_corrupt_ledger_is_undetermined_everywhere].

`check` and `status --json` report where the limits came from as
`limits_source`: `config` | `derived` | `default` [test: crates/budgetguard/tests/unset_limits_are_derived.rs::absent_config_with_history_check_reports_derived_limits] [test: crates/budgetguard/tests/unset_limits_are_derived.rs::control_one_configured_axis_reports_config_and_keeps_others_off].

Allowed (exit 0, no decision) [test: crates/budgetguard/tests/prose_claims.rs::claim_reentry_allowed] [test: crates/budgetguard/tests/prose_claims.rs::claim_disable_env]:

- A re-entry with `stop_hook_active` (a previous Stop hook already blocked) —
  an over-budget session is blocked once, then allowed to stop [test: crates/budgetguard/tests/prose_claims.rs::claim_reentry_allowed].
- `BUDGETGUARD_DISABLE=1` → hook is a no-op, even when the config file cannot be parsed [test: crates/budgetguard/tests/prose_claims.rs::claim_disable_env].
- `enabled = false` in the config → hook is a no-op for a parseable Stop
  payload (unparseable stdin still blocks) [test: crates/budgetguard/tests/prose_claims.rs::claim_example_toml].

*Cannot determine* is **not** treated as within budget [test: crates/budgetguard/src/gate.rs::an_undetermined_config_blocks_despite_zero_default_limits] [test: crates/budgetguard/src/gate.rs::a_contended_ledger_lock_makes_the_day_undetermined_and_blocks]:

- A config file that exists but cannot be read or parsed → **block**
  (`config-undetermined`), whatever the limits were meant to be [test: crates/budgetguard/src/gate.rs::an_undetermined_config_blocks_despite_zero_default_limits] [test: crates/budgetguard/src/config.rs::an_unparseable_config_is_undetermined_not_an_unconfigured_gate] [test: crates/budgetguard/src/config.rs::an_unreadable_config_is_undetermined_not_an_unconfigured_gate] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::control_unparseable_config_blocks_naming_the_file].
- Session spend that cannot be measured (for example a sub-agent transcript that
  could not be read, or a model with no known rate) → **block** if any block limit is armed, else **warn** if
  any warn limit is armed [test: crates/budgetguard/src/gate.rs::unreadable_subagents_dir_is_undetermined_and_blocks_when_a_limit_is_armed] [test: crates/budgetguard/tests/prose_claims.rs::claim_session_undetermined_warn_only] [test: crates/budgetguard/tests/prose_claims.rs::claim_session_undetermined_daily_arms].
  This includes a `transcript_path` that does not exist or is a directory, and
  a Stop payload whose `session_id` or `transcript_path` is empty [test: crates/budgetguard/tests/repro_unmeasurable_transcript_allows.rs::a_nonexistent_transcript_with_limits_armed_is_not_silently_allowed] [test: crates/budgetguard/tests/repro_unmeasurable_transcript_allows.rs::b_directory_transcript_with_limits_armed_is_not_silently_allowed] [test: crates/budgetguard/tests/repro_unmeasurable_transcript_allows.rs::d_empty_session_or_transcript_with_limits_armed_is_not_silently_allowed].
- stdin that is not a hook payload at all → **block** (`{"decision":"block"}` on
  stdout, exit 0) in every case, with or without a limit armed: the gate cannot
  process its input, and an exit 1 would be a non-blocking error that lets the
  stop through. This holds even with `enabled = false`. The block is bounded:
  at most 3 consecutive unparseable stops block, and the 4th is allowed with
  a loud `systemMessage` saying the gate gave up. The streak is kept per
  `CLAUDE_CODE_SESSION_ID` (or under one global key) in
  `<state_dir>/unparseable-streak/`, and a parsed stop resets it. A marker that
  cannot be read or written allows the stop loudly [test: crates/budgetguard/tests/unparseable_retry_bound.rs::blocks_three_times_then_allows_the_fourth_with_a_session] [test: crates/budgetguard/tests/unparseable_retry_bound.rs::blocks_three_times_then_allows_the_fourth_without_a_session] [test: crates/budgetguard/tests/unparseable_retry_bound.rs::a_parsed_stop_resets_the_streak] [test: crates/budgetguard/tests/unparseable_retry_bound.rs::an_unusable_marker_allows_loudly]. `BUDGETGUARD_DISABLE=1` bypasses it entirely [test: crates/budgetguard/tests/repro_unmeasurable_transcript_allows.rs::c2_non_json_stdin_with_block_limit_blocks_the_stop] [test: crates/budgetguard/tests/repro_unmeasurable_transcript_allows.rs::c3_non_json_stdin_with_warn_only_limit_blocks_the_stop] [test: crates/budgetguard/tests/repro_unmeasurable_transcript_allows.rs::c4_non_json_stdin_with_unreadable_config_blocks_the_stop] [test: crates/budgetguard/tests/repro_unmeasurable_transcript_allows.rs::c5_non_json_stdin_with_no_limit_blocks_the_stop].
- A day total that cannot be measured (ledger lock held by another session, or
  a corrupt `ledger.json`, which is preserved untouched) → block/warn per the
  armed *daily* limits; a session already over its own block limit still gets
  that precise reason [test: crates/budgetguard/src/gate.rs::a_contended_ledger_lock_makes_the_day_undetermined_and_blocks] [test: crates/budgetguard/src/gate.rs::a_corrupt_ledger_makes_the_day_undetermined_and_blocks] [test: crates/budgetguard/tests/prose_claims.rs::claim_day_undetermined_warn_only] [test: crates/budgetguard/src/gate.rs::a_contended_lock_still_reports_a_session_block_precisely].
- Unmeasured totals print as `unknown` on stderr, never as `$0` [test: crates/budgetguard/tests/prose_claims.rs::claim_unknown_not_zero] [test: crates/budgetguard/tests/prose_claims.rs::claim_session_undetermined_warn_only].

## Install (plugin)

The marketplace catalog at the root of this repo lists budgetguard [test: crates/budgetguard/tests/prose_claims.rs::claim_marketplace_entry]:

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install budgetguard@yukineko
```

```sh
cargo install --path .
budgetguard init          # write a starter ./budgetguard.toml [test: crates/budgetguard/tests/prose_claims.rs::claim_init_writes_starter]
budgetguard install       # merge the Stop hook into ~/.claude/settings.json [test: crates/budgetguard/tests/prose_claims.rs::claim_install_merges_stop_hook]
```

Drop a `budgetguard.toml` in the project root, or `~/.budgetguard/config.toml`
for a global default [test: crates/budgetguard/tests/prose_claims.rs::claim_config_lookup_home_and_project_not_merged]. `budgetguard.example.toml` is a working starter
config with `enabled`, a commented `state_dir`, `[session]`, `[daily]` and a
commented `[[price]]` [test: crates/budgetguard/tests/prose_claims.rs::claim_example_toml] [test: crates/budgetguard/tests/prose_claims.rs::claim_example_commented].

```toml
[session]
warn_usd  = 0.50
block_usd = 2.00

[daily]
warn_usd  = 5.00
block_usd = 20.00
```

The project file, if present, is used *instead of* the home file (they are not
merged) [test: crates/budgetguard/tests/prose_claims.rs::claim_config_lookup_home_and_project_not_merged]. There is no trust gate: a repo-shipped `budgetguard.toml` is honored
as-is [test: crates/budgetguard/tests/prose_claims.rs::claim_no_trust_gate]. Other keys: `enabled`, `state_dir` (default `~/.budgetguard/state`),
`[[price]]`, and `[cache]` (`min_rate`, `min_tokens`) [test: crates/budgetguard/tests/prose_claims.rs::claim_example_toml] [test: crates/budgetguard/tests/prose_claims.rs::claim_ledger_latest_cost] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_below_floor_not_promoted] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_min_rate_default_and_inrange] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::control_valid_finite_override_allows] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_promoted_warning].

```sh
budgetguard gate           # Stop hook (reads stdin JSON, emits decision) [test: crates/budgetguard/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_known_model_over_limit_blocks]
budgetguard status         # resolved config + today's spend [test: crates/budgetguard/tests/integration.rs::status_prints_spend_summary]
budgetguard status --json  # machine-readable budget pressure [test: crates/budgetguard/tests/prose_claims.rs::claim_status_json_fields]:
                           #   {day_usd, daily_warn_usd, daily_block_usd, pressure};
                           #   an unreadable config prints {error, undetermined:true}
                           #   and exits 2 [test: crates/budgetguard/tests/prose_claims.rs::claim_status_json_fields] [test: crates/budgetguard/tests/prose_claims.rs::claim_status_json_unreadable];
                           #   so does a corrupt ledger.json [test: crates/budgetguard/tests/repro_status_json_corrupt_ledger.rs::status_json_corrupt_ledger_is_not_zero_spend]
budgetguard init [--force]          # write a starter budgetguard.toml; without --force an existing file is left unchanged [test: crates/budgetguard/tests/prose_claims.rs::claim_init_writes_starter]
budgetguard install [--dry-run]     # merge the hook into ~/.claude/settings.json [test: crates/budgetguard/tests/prose_claims.rs::claim_install_merges_stop_hook]
budgetguard uninstall [--dry-run]   # remove it [test: crates/budgetguard/tests/prose_claims.rs::claim_install_merges_stop_hook]
```

Uses `harness-core`'s built-in rate table [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table]:

| Family | Input $/1M | Output $/1M [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table] [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_split] |
|---|---|---|
| Fable / Mythos | 10 | 50 [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table] [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_split] |
| Opus | 5 | 25 [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table] [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_split] [test: crates/harness-core/src/pricing.rs::opus_input_output] |
| Sonnet | 3 | 15 [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table] [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_split] |
| Haiku | 1 | 5 [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table] [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_split] |

Override any model with a `[[price]]` stanza in `budgetguard.toml` (`pattern`
is a case-insensitive substring of the model id) [test: crates/budgetguard/tests/price_pattern_case_ff98fed0.rs::f4_mixed_case_price_pattern_matches_and_allows] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::control_valid_finite_override_allows]. A model with usage that is
neither in the table nor matched by an override has an **unknown** cost, not
$0: the gate treats the session as unmeasured (`cost-undetermined`) [test: crates/budgetguard/tests/backlog_ff98fed0.rs::backlog_ff98fed0_unknown_model_with_usage_blocks_not_priced_zero] [test: crates/budgetguard/src/gate.rs::fresh_gauge_record_with_unpriced_model_is_undetermined_and_blocks]. Declare a
genuinely free model with `input = 0.0` / `output = 0.0` [test: crates/budgetguard/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_explicit_zero_override_allows]. A `[[price]]` row
with an empty `pattern` (it would match every model) or a rate that is not a
finite non-negative number makes the config unusable, exactly like a parse
error [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::r3_empty_pattern_is_config_undetermined_not_a_wildcard] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::r3_nan_rate_is_config_undetermined] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::r3_inf_rate_is_config_undetermined] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::r3_negative_rate_is_config_undetermined].

License: MIT [test: crates/budgetguard/tests/prose_claims.rs::claim_license_mit].
