Session-end auto-flow gate for Claude Code — a **Stop** hook that, once the `/record` prompt has been issued, keeps a session from ending while the in-flight condukt run still has pending tasks [test: crates/autoflow/tests/undetermined_stop.rs::pending_run_state_still_drives_condukt].
When a turn finishes, autoflow prompts `/record` once, then loops `/condukt` while the run's pending set keeps shrinking, escalating *visibly* if progress stalls [test: crates/autoflow/tests/prose_claims.rs::claim_record_then_condukt_sequence] [test: crates/autoflow/tests/prose_claims.rs::claim_config_defaults].
A **PreCompact** + **UserPromptSubmit** pair keeps that loop alive across a `/compact`: PreCompact drops a resume marker when this session is driving the project's backlog queue (per `backlog lock status`), and the next UserPromptSubmit consumes it to re-inject a "resume /flow" instruction exactly once [test: crates/autoflow/tests/precompact_lock.rs::precompact_resumes_flow_only_when_this_session_holds_its_own_project_lock].

<!-- record: 2026-08-20 rev: 5289312f -->
The backlog queue went out of scope on 2026-08-20. Until then an empty condukt pending set fell through to the queue and blocked the Stop with "/backlog を実行してください" every turn (behind a compass-freshness gate), and a **SessionStart** hook made the same request up front as "バックログに N 件…/flow で開始しますか？". All three — the proposal, the Stop arm, and its compass gate — were retired on the user's instruction.
<!-- /record -->

The backlog queue is out of scope: a pending backlog item does not make the Stop block [test: crates/autoflow/tests/undetermined_stop.rs::a_pending_backlog_queue_no_longer_nudges].

The Stop hook only ever emits a `block` decision with a reason — it never runs the work itself (its only side effects are its own per-session state file, marking the pending condukt tasks `running` on a routine continuation, and an overwatch violation record per block) [test: crates/autoflow/tests/prose_claims.rs::claim_stop_side_effects_only] [test: crates/autoflow/tests/prose_claims.rs::claim_stop_side_effects_other_blocks].
An unreadable condukt run-state **blocks** [test: crates/autoflow/tests/undetermined_stop.rs::undetermined_run_state_unparseable_does_not_latch_done] [test: crates/autoflow/tests/undetermined_stop.rs::undetermined_run_state_missing_a_task_status_does_not_latch_done].
The launcher likewise emits a `block` for `stop` when no binary exists for the host [test: crates/autoflow/tests/launcher_missing_binary.rs::stop_missing_binary_blocks_instead_of_silently_certifying].
The Stop also ends silently when the gate is disabled, when the condukt run file is missing (condukt never ran here — observed as "no pending task"), and on the stand-downs listed below [test: crates/autoflow/tests/prose_claims.rs::claim_silent_when_disabled_or_no_run_file] [test: crates/autoflow/tests/prose_claims.rs::claim_live_driver_stands_down] [test: crates/autoflow/tests/prose_claims.rs::claim_explicit_stop_allows_this_stop].

- **Idle** (enough turns + tool events this session): block → `/session-insights:record` [test: crates/autoflow/tests/prose_claims.rs::claim_idle_record_threshold].
- **RecordRequested / Continuing** (condukt tasks still pending): block → `/condukt`, continuing while the pending set keeps *shrinking*, with no call-count ceiling [test: crates/autoflow/tests/undetermined_stop.rs::pending_run_state_still_drives_condukt] [test: crates/autoflow/src/state.rs::decreasing_size_continues_forever_with_no_iteration_count_ceiling].
- **RecordRequested / Continuing** (pending set fails to shrink `stuck_threshold` Stops in a row): escalate *visibly* [test: crates/autoflow/src/state.rs::no_progress_reaching_threshold_escalates_and_resets_streak].
- `stuck_threshold` defaults to 3 [test: crates/autoflow/tests/prose_claims.rs::claim_config_defaults].
- **RecordRequested / Continuing** (condukt run-state could not be read): block with a "判定不能" (undetermined) reason; the phase is left untouched so the next Stop re-observes [test: crates/autoflow/tests/prose_claims.rs::claim_undetermined_reason_phase_untouched].
- **Done** (no pending task in the condukt run): allow the turn to end, and the queue is not consulted [test: crates/autoflow/tests/undetermined_stop.rs::genuinely_empty_world_still_latches_done_silently] [test: crates/autoflow/tests/undetermined_stop.rs::a_pending_backlog_queue_no_longer_nudges].

The escalation is never silent: a stall does not end the Stop silently [test: crates/autoflow/tests/prose_claims.rs::claim_config_defaults] [test: crates/autoflow/tests/prose_claims.rs::claim_escalation_autonomy_wording].
The escalation blocks with a message worded by the shared autonomy switch (`harness_core::autonomy` — autonomous: "continuing"; otherwise: "ask the user") [test: crates/autoflow/tests/prose_claims.rs::claim_escalation_autonomy_wording].
The escalating Stop is the one Stop that does *not* re-mark the pending tasks `running` [test: crates/autoflow/tests/mark_running_on_escalation.rs::escalate_stuck_does_not_re_mark_tasks_running].

- **Explicit user stop** (before the state machine): if the user's most recent transcript signal — a typed message, or their answer to an `AskUserQuestion` — is an explicit stop instruction ("止めて", "stop", …), *this* Stop is allowed through untouched [test: crates/autoflow/tests/prose_claims.rs::claim_explicit_stop_allows_this_stop].
- An explicit stop is not latched into `Done`; nudging resumes if the user keeps working [test: crates/autoflow/tests/prose_claims.rs::claim_explicit_stop_not_latched].
- An unreadable or absent signal never counts as a stop [test: crates/autoflow/src/stop_intent.rs::unreadable_transcript_path_is_false] [test: crates/autoflow/src/stop_intent.rs::empty_transcript_path_is_false] [test: crates/autoflow/tests/prose_claims.rs::claim_unreadable_transcript_not_stop].
- **Delegation-record advisory** (in RecordRequested / Continuing): if this session's transcript shows `/flow` drove a condukt run to completion without a `fugu-router record` for it, block once per session asking for `fugu-router record --class flow-delegation …` [test: crates/autoflow/tests/prose_claims.rs::claim_delegation_advisory_once].

autoflow stands down entirely (allows the Stop) while `backlog lock status` reports a live driver for this project, so a running `/flow` or `/backlog` driver is never double-driven [test: crates/autoflow/tests/prose_claims.rs::claim_live_driver_stands_down].
Backlog being *observably* not installed does not stand down [test: crates/autoflow/tests/prose_claims.rs::claim_backlog_absent_no_standdown].

At **PreCompact**, if this session is the exclusive lock holder or one of the registered drivers in `backlog lock status` (that is, a `/flow` loop is actually driving from this session) and the user hasn't opted out (`resume_flow_on_compact = false`), autoflow writes a resume marker [test: crates/autoflow/tests/precompact_lock.rs::precompact_resumes_flow_only_when_this_session_holds_its_own_project_lock] [test: crates/autoflow/src/lock.rs::holds_lock_from_status_matches_any_registered_driver] [test: crates/autoflow/tests/prose_claims.rs::claim_precompact_opt_out_with_lock].
PreCompact never blocks compaction [test: crates/autoflow/tests/prose_claims.rs::claim_precompact_never_blocks].
The following **UserPromptSubmit** consumes that marker (if any) and injects a "resume `/flow`" instruction exactly once [test: crates/autoflow/src/main.rs::prompt_submit_consumes_marker_once] [test: crates/autoflow/tests/prose_claims.rs::claim_prompt_submit_inject_text].
Every ordinary turn without a marker stays silent [test: crates/autoflow/src/main.rs::hooks_are_silent_without_marker_or_session].

Installed via the plugin marketplace, the bundled `hooks/hooks.json` wires the **Stop**, **PreCompact**, and **UserPromptSubmit** hooks to `${CLAUDE_PLUGIN_ROOT}/bin/autoflow` automatically [test: crates/autoflow/tests/prose_claims.rs::claim_hooks_json_wiring].
[record: 2026-08-20 rev: 5289312f] The **SessionStart** hook was removed.
Thresholds (min turns, min tool events, stuck threshold, resume-flow-on-compact) come from config defaults; the gate is on by default [test: crates/autoflow/tests/prose_claims.rs::claim_config_defaults].
To override, write `~/.autoflow/config.toml` [test: crates/autoflow/tests/prose_claims.rs::claim_config_toml_override]:

```toml
enabled = true                # default true [test: crates/autoflow/tests/prose_claims.rs::claim_config_defaults]
min_turns = 2                 # turns before the /record prompt [test: crates/autoflow/tests/prose_claims.rs::claim_idle_record_threshold]
min_tool_events = 3           # tool events before the /record prompt [test: crates/autoflow/tests/prose_claims.rs::claim_idle_record_threshold]
stuck_threshold = 3           # no-progress Stops before visible escalation [test: crates/autoflow/tests/prose_claims.rs::claim_config_toml_override]
resume_flow_on_compact = true
state_dir = "~/.autoflow/state"
```

An unparseable config file is ignored (defaults apply) [test: crates/autoflow/tests/prose_claims.rs::claim_unparseable_config_defaults].

```sh
cargo install --path .
autoflow stop            # Stop hook: run the record→condukt state machine [test: crates/autoflow/tests/prose_claims.rs::claim_record_then_condukt_sequence]
autoflow pre-compact     # PreCompact hook: drop a resume-flow marker if this session holds the lock [test: crates/autoflow/tests/precompact_lock.rs::precompact_resumes_flow_only_when_this_session_holds_its_own_project_lock]
autoflow prompt-submit   # UserPromptSubmit hook: consume the marker and inject a "resume the /flow loop" instruction once [test: crates/autoflow/tests/prose_claims.rs::claim_prompt_submit_inject_text]
```

`autoflow stop` reads the hook JSON on stdin and prints a `block` decision (or nothing) [test: crates/autoflow/tests/undetermined_stop.rs::pending_run_state_still_drives_condukt] [test: crates/autoflow/tests/integration.rs::valid_stop_payload_with_no_metrics_is_silent].
`autoflow pre-compact` and `autoflow prompt-submit` are silent unless the resume-marker gate is met [test: crates/autoflow/src/main.rs::hooks_are_silent_without_marker_or_session] [test: crates/autoflow/src/main.rs::precompact_writes_no_marker_without_a_held_lock] [test: crates/autoflow/tests/prose_claims.rs::claim_precompact_prompt_submit_cli_silent].
`AUTOFLOW_DISABLE=1` silences the gate [test: crates/autoflow/tests/prose_claims.rs::claim_disable_env].

```sh
cargo test
```

Only the `bin/autoflow` launcher is committed [test: crates/autoflow/tests/prose_claims.rs::claim_only_launcher_committed].
It execs the per-platform `bin/autoflow-<os>-<arch>` binary [test: crates/autoflow/tests/launcher_missing_binary.rs::control_launcher_still_execs_the_host_binary_when_it_is_present].
That binary is gitignored [test: crates/autoflow/tests/prose_claims.rs::claim_only_launcher_committed].
Run the rollout again when you change behavior the hook relies on.
