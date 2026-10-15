On each `Stop`, gauge aggregates the session's JSONL transcript into one record
per session (`<store>/sessions/<session_id>.json`) [test: crates/gauge/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_opus_session_costs_five_dollars].
The record is rewritten each turn so the latest write holds the whole session to date [test: crates/gauge/tests/prose_claims.rs::claim_record_cumulative_single_file]:

- input / output / **cache-write (5m & 1h)** / **cache-read** tokens, per model [test: crates/gauge/tests/prose_claims.rs::claim_record_token_kinds_per_model]
- number of model requests (turns) and per-tool call counts (Bash, Edit, …) [test: crates/gauge/tests/prose_claims.rs::claim_record_turns_and_tool_counts]
- first/last timestamp → session duration [test: crates/gauge/tests/prose_claims.rs::claim_record_first_last_duration]

The **estimated cost** is priced when a record is read, from a built-in price table (overridable per model) [test: crates/gauge/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_opus_session_costs_five_dollars] [test: crates/gauge/tests/config_residual_00880b3d.rs::control_valid_finite_override_applies].

`gauge report` then rolls these up by project, model, and day [test: crates/gauge/tests/prose_claims.rs::claim_report_rollup_project_model_day].
`gauge subagents` reads each sub-agent (`Task`) transcript **live**
(`<session>/subagents/agent-<id>.jsonl`) and attributes cost per `Task`
invocation [test: crates/gauge/tests/prose_claims.rs::claim_subagents_live_per_task].

`GAUGE_DISABLE=1` turns recording off entirely [test: crates/gauge/tests/prose_claims.rs::claim_gauge_disable_env].

As a Claude Code plugin, the bundled `bin/gauge` is invoked by `hooks/hooks.json`
(`${CLAUDE_PLUGIN_ROOT}/bin/gauge record`) [test: crates/gauge/tests/prose_claims.rs::claim_hooks_json_stop_record]. Install standalone:

```sh
cargo install --path .
gauge install        # merges the Stop hook into ~/.claude/settings.json [test: crates/gauge/tests/prose_claims.rs::claim_install_merges_stop_hook]
gauge uninstall      # removes it again [test: crates/gauge/tests/prose_claims.rs::claim_uninstall_removes_hook]
```

```sh
gauge report                       # totals + breakdown by project / model / day [test: crates/gauge/tests/prose_claims.rs::claim_report_rollup_project_model_day]
gauge report --project myrepo      # filter by project [test: crates/gauge/tests/prose_claims.rs::claim_report_project_filter]
gauge report --since 2026-06-01    # filter by day [test: crates/gauge/tests/prose_claims.rs::claim_report_since_filter]
gauge session                      # details for the most recent session [test: crates/gauge/tests/prose_claims.rs::claim_session_most_recent]
gauge session --session <ID> --json  # a specific session as {session_id, cost_usd, models, agents} [test: crates/gauge/tests/prose_claims.rs::claim_session_json_shape]
gauge subagents                    # per-sub-agent (Task) cost, read live from transcripts [test: crates/gauge/tests/prose_claims.rs::claim_subagents_live_per_task]
gauge subagents --session <ID> --json  # JSON array; time fields are null when unknown [test: crates/gauge/tests/prose_claims.rs::claim_subagents_json_array_null_times]
gauge status                       # resolved config, store path, session count [test: crates/gauge/tests/prose_claims.rs::claim_status_fields]
gauge init [--force]               # write a starter ./gauge.toml (--force overwrites) [test: crates/gauge/tests/prose_claims.rs::claim_init_force]
```

`gauge install` / `gauge uninstall` accept `--dry-run` [test: crates/gauge/tests/prose_claims.rs::claim_install_dry_run].

```sh
gauge config show                  # print the currently registered window, if any [test: crates/gauge/tests/backlog_49d9bcae.rs::control_window_show_reads_valid_and_missing]
```

Built-in rates (USD per 1M tokens, input/output): **Opus** 5/25 · **Sonnet**
3/15 · **Haiku** 1/5 · **Fable/Mythos** 10/50 [test: crates/gauge/tests/prose_claims.rs::claim_builtin_rates]. Cache writes bill at 1.25× input
(5-minute TTL) or 2× (1-hour TTL); cache reads at 0.1× input [test: crates/gauge/tests/prose_claims.rs::claim_cache_multipliers]. An unrecognized
model with usage has an **unknown** cost, not $0: gauge prints `unknown` [test: crates/gauge/tests/consumer_erasure_ff98fed0.rs::ff98fed0_consumer_session_text_cost_is_unknown] [test: crates/gauge/tests/consumer_erasure_ff98fed0.rs::ff98fed0_consumer_report_total_is_unknown_not_partial_sum] [test: crates/gauge/tests/consumer_erasure_ff98fed0.rs::ff98fed0_consumer_status_total_is_unknown_not_partial_sum].
In JSON the cost is `cost_usd: null` plus `cost_undetermined` [test: crates/gauge/tests/prose_claims.rs::claim_unpriced_json_null_undetermined].
budgetguard treats the session as unmeasured [test: crates/budgetguard/src/gate.rs::fresh_gauge_record_with_unpriced_model_is_undetermined_and_blocks].
Declare a genuinely free model with an explicit `input = 0.0` /
`output = 0.0` override [test: crates/gauge/tests/config_residual_00880b3d.rs::control_explicit_zero_rate_is_a_known_zero] [test: crates/gauge/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_explicit_zero_override_is_known_zero].
Override or add a model in `gauge.toml`: [test: crates/gauge/tests/config_residual_00880b3d.rs::control_valid_finite_override_applies] [test: crates/gauge/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_explicit_zero_override_is_known_zero]

```toml
[[pricing]]
pattern = "opus"   # substring match against the model id; first match wins [test: crates/gauge/tests/prose_claims.rs::claim_pricing_pattern_first_match]
input = 5.0
output = 25.0
```

Cost is recomputed on every report, so editing the
table re-prices history [test: crates/gauge/tests/prose_claims.rs::claim_report_reprices_history].

Project `./gauge.toml` over `~/.gauge/config.toml` over built-in defaults (first
file that exists wins) [test: crates/gauge/tests/prose_claims.rs::claim_config_lookup_order]. See `gauge.example.toml` [test: crates/gauge/tests/config_residual_00880b3d.rs::control_example_toml_verbatim_loads].
Store defaults to
`~/.gauge/store`. [test: crates/gauge/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_opus_session_costs_five_dollars]

A project `./gauge.toml` that exists but cannot be read or parsed (including any unknown
key, such as a misspelled `[[pricng]]` or `enabeld`), or a `[[pricing]]` row
missing `pattern`/`input`/`output` or with a rate that is not a finite
non-negative number, is not silently replaced by defaults: the read commands (`report`, `session`,
`subagents`, `status`) print a message naming the file and exit 1, and the
`record` hook records nothing and says why on stderr [test: crates/gauge/tests/prose_claims.rs::claim_bad_config_refuses] [test: crates/gauge/tests/prose_claims.rs::claim_unreadable_config_refuses].

```sh
cargo build --release
cargo test
```
