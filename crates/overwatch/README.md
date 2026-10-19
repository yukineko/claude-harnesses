Every mutating command (begin, run, end, heartbeat, reap, reassign, pause, resume) exits non-zero when the action did not happen: lease lock contended, lease root unresolvable, key not held (end, reassign), unknown `end --status`, or condukt failing (pause, resume). Exit 0 from `heartbeat`/`reap` still only means the call ran; read the printed `refreshed`/`reaped` count to see what changed.

Do NOT proceed when begin exits 1 with skip JSON (another session has this key).

- Any session can call `overwatch begin --key <k>` to atomically claim a key; if another session already holds a live lease, the request is denied. [test: crates/overwatch/tests/lease_concurrency.rs::concurrent_begin_same_key_exactly_one_wins]
- When a new session tries to begin work on an already-claimed key, overwatch reports it (exit 1 + skip JSON) so the caller skips duplicate execution. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_held_key_exit1_skip_json]
- On **SessionStart** it injects the project-wide progress view (via `overwatch status`) so the session knows which sessions hold live leases, what is pending (PDO hypotheses, backlog), which condukt runs exist, and the compass gap. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_sessionstart_injects_status]
- The helper binaries (`backlog` / `hypothesis` / `condukt` / `compass`) are resolved from the plugin cache first and `PATH` second. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_helper_binary_resolution_order]
- Liveness is heartbeat-TTL; a lease with no heartbeat for more than 1800s (30 min) is stale. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_ttl_1800_stale]
- Stale leases are dropped by **every** lease mutation (`begin` / `run` / `end` / `heartbeat` / `reassign` / `reap`), and ignored by `status` / `sessions` / `lease`, so a dead holder never blocks a `begin`. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_stale_dropped_by_every_mutation]
- `overwatch reap` just runs that cleanup on its own. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_reap_drops_stale_prints_count]

Two hooks (SessionStart + Stop), both running `overwatch status` with a 5s hook timeout [test: crates/overwatch/tests/prose_claims.rs::claim_rd_hooks_json_two_status_hooks]. A cached status is discarded once `aggregate::STATUS_CACHE_TTL_SECS` has passed, when `leases.json` was written at or after the second the cache was built, or when its mtime cannot be observed (`aggregate::cache_is_fresh`) [test: crates/overwatch/src/aggregate.rs::test_cache_is_fresh_within_ttl] [test: crates/overwatch/src/aggregate.rs::test_cache_is_fresh_expired_past_ttl] [test: crates/overwatch/src/aggregate.rs::test_cache_is_fresh_rule2_same_second_ledger_write_is_not_fresh] [test: crates/overwatch/src/aggregate.rs::test_cache_is_fresh_rule3_undetermined_ledger_mtime_is_not_fresh].

`(none)` is only printed when the source was actually observed [test: crates/overwatch/src/render.rs::undetermined_sessions_render_unknown_not_none] [test: crates/overwatch/src/render.rs::undetermined_backlog_renders_unknown_not_none] [test: crates/overwatch/src/render.rs::undetermined_hypotheses_render_unknown_not_none] [test: crates/overwatch/src/render.rs::undetermined_runs_render_unknown_not_none] [test: crates/overwatch/src/render.rs::undetermined_compass_gap_renders_unknown_not_none].

Every section of `overwatch status` prints `(none)` when it is empty [test: crates/overwatch/src/render.rs::undetermined_is_scoped_to_its_own_source] [test: crates/overwatch/src/render.rs::test_format_roster_empty] [test: crates/overwatch/src/render.rs::test_format_hypotheses_none].

A source that could **not** be read — an unreadable/corrupt lease ledger, an absent helper binary, undecodable JSON — renders `(unknown: <reason>)` instead [test: crates/overwatch/src/render.rs::undetermined_render_carries_the_reason] [test: crates/overwatch/tests/prose_claims.rs::claim_rd_status_warning_on_unreadable_source] [test: crates/overwatch/src/aggregate.rs::test_parse_backlog_invalid_json_is_error_not_zero] [test: crates/overwatch/src/aggregate.rs::test_parse_condukt_runs_unparseable_progress_is_error_not_zero].

A WARNING listing every such source goes to stderr. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_status_warning_on_unreadable_source]

`status --json` also carries a machine-readable `undetermined` array (`[{source, reason}]`), omitted entirely when every source was observed. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_status_json_undetermined_array]

```
/plugin install overwatch@yukineko
```

```sh
cargo install --path .
```

There is no `overwatch install` subcommand. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_no_install_subcommand]

```sh
overwatch begin --key <k> --title <t> [--session <sid>] [--scope <csv>] [--done-criteria <text>]   # try to acquire exclusive lease on key <k>; exit 1 + skip JSON if held by another live session or the lease lock is contended [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_held_key_exit1_skip_json]
overwatch lease --session <sid> [--json]    # print the live lease (PDO anchor) held by a session; exit 1 (silent) if none [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_cmd_prints_live_lease_exit1_silent]
overwatch run --key <k> [--note <text>]     # record a running heartbeat + event for a held lease [test: crates/overwatch/tests/prose_claims.rs::claim_rd_run_records_heartbeat_event]
overwatch status [--json]                   # project-wide progress view: Sessions / PDO hypotheses / Backlog / Condukt runs / Compass gap [test: crates/overwatch/tests/prose_claims.rs::claim_rd_sessionstart_injects_status]
overwatch sessions [--json]                 # only the Sessions roster: sessions holding a live (non-stale) lease [test: crates/overwatch/tests/prose_claims.rs::claim_rd_sessions_roster_live_only]
overwatch pause --run <id>                  # delegate to `condukt state pause --run <id>` and record a control event [test: crates/overwatch/tests/prose_claims.rs::claim_rd_pause_resume_delegate_condukt]
overwatch resume --run <id>                 # delegate to `condukt state resume --run <id>` and record a control event [test: crates/overwatch/tests/prose_claims.rs::claim_rd_pause_resume_delegate_condukt]
overwatch reassign --key <k> --to <sid>     # RELEASE the lease on <k> and record a "reassign to=<sid>" event (does not create a lease for <sid>) [test: crates/overwatch/tests/prose_claims.rs::claim_rd_reassign_releases_lease_no_new_lease]
overwatch end --key <k> --status <done|abandoned>   # release a lease, recording its terminal status (any other status is rejected, non-zero, lease stays held) [test: crates/overwatch/tests/prose_claims.rs::claim_rd_end_releases_lease_records_status]
overwatch reap                              # drop stale leases (no heartbeat within TTL); prints {"reaped": N} [test: crates/overwatch/tests/prose_claims.rs::claim_rd_reap_drops_stale_prints_count]
overwatch heartbeat --key <k>               # refresh heartbeat_at for a key; prints {"refreshed": 0|1} [test: crates/overwatch/tests/prose_claims.rs::claim_rd_heartbeat_refreshes_prints_count]
overwatch --protects                        # print what this gate protects, from what, and on what grounds [test: crates/overwatch/tests/protects_flag.rs::protects_prints_three_labelled_nonempty_lines_and_exits_zero]
```

Session id resolution for `begin`/`run`: `--session`, else `$CLAUDE_CODE_SESSION_ID`, else `pid-<pid>`. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_session_id_resolution]

The run id comes from `$OVERWATCH_RUN_ID`, else a timestamp-based `run-…` id. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_run_id_resolution]

`run`, `heartbeat`, `reap`, `reassign` and `end` exit non-zero when the lease lock is contended or the lease root cannot be resolved (nothing is written); `end` additionally leaves the lease NOT released. `reassign` of a key nobody holds, and `pause`/`resume` when `condukt` fails, also exit non-zero. [test: crates/overwatch/tests/repro_mutators_unresolvable_root_exit_zero.rs::heartbeat_on_unresolvable_root_must_be_nonzero] [test: crates/overwatch/tests/repro_run_lock_contended_exit_zero.rs::run_under_real_lock_contention_must_be_nonzero] [test: crates/overwatch/tests/repro_reassign_unheld_key_exit_zero.rs::reassign_of_unheld_key_must_be_nonzero] [test: crates/overwatch/tests/repro_pause_resume_condukt_failure_exit_zero.rs::pause_must_be_nonzero_when_condukt_fails] [test: crates/overwatch/tests/repro_end_unknown_status_accepted.rs::end_with_unknown_status_is_rejected]. [test: crates/overwatch/tests/prose_claims.rs::claim_sk_end_nonzero_when_not_released].

`end` exits non-zero ("nothing was released") when the key is not held. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_end_unheld_key_nonzero]

The binary itself does **not** ask for approval before `pause` / `resume` / `reassign` / `end` / `reap`. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_binary_no_approval_prompt]

Calling the binary directly performs the action immediately. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_skill_confirms_side_effects_with_askuserquestion]

Do NOT proceed when begin exits non-zero.

```bash
overwatch begin --key "hypothesis-v2.3"
```

- **exit 0**: Lease acquired (or refreshed, if the same session already held it — `claimed_at` is kept). [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_exit0_refresh_keeps_claimed_at]
- **exit 1** + JSON `{ "skipped": true, "holder": { "session_id", "run_id", "heartbeat_at" } }`: Lease is held by another live session [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_held_key_exit1_skip_json]. Do NOT proceed.
- **exit 1** + JSON `{ "skipped": true, "reason": "lock_contended" }`: the lease-registry lock could not be taken, so whether the key is free was not checked [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_held_key_exit1_skip_json]. Do NOT proceed (retry later).

Check the exit code and skip if it is non-zero.

A corrupt `leases.json` also exits **1**, with the error on stderr and no skip JSON on stdout. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_internal_error_exit1_no_json]

An unresolvable store root also exits **1**, with stderr saying the root is undetermined [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_internal_error_exit1_no_json].

A lease can carry two optional PDO-anchor fields, `scope` and `done_criteria`. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_scope_done_criteria_fields]

Both are `#[serde(default)]`, so existing `leases.json` files (written before these fields existed) still load unchanged [test: crates/overwatch/tests/prose_claims.rs::claim_rd_legacy_leases_json_loads]:

- `scope: Vec<String>` — files/globs this session owns. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_scope_done_criteria_fields]
- `done_criteria: Option<String>` — the session's definition of done. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_scope_done_criteria_fields]

`overwatch begin` takes two optional flags, `--scope` and `--done-criteria`, to set them [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_scope_done_criteria_fields]:

```bash
overwatch begin --key "hypothesis-v2.3" --title "cache the router table" \
  --scope "crates/fugu-router/src/**,crates/condukt/src/schedule.rs" \
  --done-criteria "all fugu-router tests green, no clippy warnings"
```

On a successful `begin` (exit 0) an advisory JSON summary is printed to stdout [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_prints_advisory_summary]:

```json
{ "scope_overlap": [ { "key": "...", "title": "...", "scope": ["..."] } ],
  "scope_undeclared": [ { "key": "...", "title": "..." } ],
  "scope_declared": true,
  "possible_duplicate": [ { "key": "...", "title": "...", "similarity": 0.72 } ] }
```

- `scope_overlap` lists other live leases (different key) whose scope overlaps this one. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_scope_overlap_lists_live_overlapping_peers]
- `scope_undeclared` lists other live leases whose overlap is **undetermined** because this lease or that peer declared no scope. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_scope_undeclared_lists_undetermined_peers]
- Undeclared is not "no overlap": a peer missing from both lists has a declared, non-overlapping scope [test: crates/overwatch/tests/prose_claims.rs::claim_rd_scope_undeclared_lists_undetermined_peers] [test: crates/overwatch/tests/prose_claims.rs::claim_rd_scope_overlap_lists_live_overlapping_peers].
- `scope_declared` is `false` when this lease itself declared no scope; then every peer is listed in `scope_undeclared` (backlog 56eb3f9a). [test: crates/overwatch/tests/prose_claims.rs::claim_rd_scope_declared_false_lists_all_peers_undeclared]
- `possible_duplicate` lists other live leases whose title is lexically similar (Jaccard similarity; default threshold 0.6, override via `OVERWATCH_DUP_THRESHOLD`) [test: crates/overwatch/tests/prose_claims.rs::claim_rd_possible_duplicate_threshold].
- The lists default to empty arrays and **never change the exit code**. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_lists_never_change_exit_code]

```bash
overwatch lease --session <sid> [--json]
```

Prints the live lease (PDO anchor) held by `<sid>` — the most-recently-claimed one if the session holds several — or exits 1 silently if none. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_cmd_prints_live_lease_exit1_silent]

Each lease carries a `heartbeat_at` timestamp (plus `claimed_at`) [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_has_heartbeat_and_claimed_at]. Staleness is judged against a **fixed** heartbeat TTL of **1800 seconds (30 minutes)** — `store::LEASE_TTL_SECS` [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_ttl_1800_stale]. The TTL is a compile-time constant, not configurable. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_ttl_not_configurable]

- Once `now - heartbeat_at > 1800s` the lease is stale: the next lease mutation (any of `begin` / `run` / `end` / `heartbeat` / `reassign` / `reap`) drops it from `leases.json`, and a `begin` on that key by another session succeeds [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_ttl_1800_stale] [test: crates/overwatch/tests/prose_claims.rs::claim_rd_stale_dropped_by_every_mutation].
- `overwatch reap` is simply that cleanup with nothing else attached. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_reap_drops_stale_prints_count]

State lives under `~/.overwatch/<project-key>/overwatch/` (`leases.json` + `events.jsonl`), keyed per repository on the MAIN worktree root [test: crates/overwatch/tests/prose_claims.rs::claim_rd_state_dir_layout] [test: crates/overwatch/tests/worktree_partition.rs::lease_begun_in_linked_worktree_is_visible_from_main_checkout].

The main checkout, every linked `git worktree`, and any subdirectory of a linked worktree share one store (so `overwatch status` run from the main checkout sees sessions working in linked worktrees) [test: crates/overwatch/tests/worktree_partition.rs::lease_begun_in_linked_worktree_is_visible_from_main_checkout] [test: crates/overwatch/tests/worktree_partition.rs::lease_begun_in_main_checkout_is_visible_from_linked_worktree] [test: crates/overwatch/tests/worktree_partition.rs::subdirectory_of_linked_worktree_resolves_to_same_store].

If the main worktree root cannot be resolved (an unparseable `.git` file), `begin`, `lease` and `end` fail rather than fall back to a per-worktree store. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_main_worktree_unresolvable_fails]

A cwd outside any git repo is keyed on the cwd itself. [test: crates/overwatch/tests/worktree_partition.rs::control_cwd_outside_any_git_repo_still_works]

- `<base>` is `~/.overwatch` (`$HOME/.overwatch`). [test: crates/overwatch/tests/prose_claims.rs::claim_sk_store_path]
- `<project-key>` is `<basename>-<fnv1a32 of the canonical path>` of the repository's MAIN worktree root (shared by all its linked worktrees) [test: crates/overwatch/tests/prose_claims.rs::claim_rd_state_dir_layout] [test: crates/overwatch/tests/worktree_partition.rs::lease_begun_in_linked_worktree_is_visible_from_main_checkout].
- A lease stays in `leases.json` until it is `end`ed, released by `reassign`, or dropped as stale by a later lease mutation; overwatch does not tie it to the Claude Code process, and no overwatch hook removes it when the session exits [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_persists_until_end_reassign_stale] [test: crates/overwatch/tests/prose_claims.rs::claim_rd_stale_dropped_by_every_mutation].

Alongside the lease registry, overwatch records gate-violation events with a **normalized signature** and escalates a signature as a **systemic issue** when it recurs N+ times within a time window across tasks/sessions [test: crates/overwatch/tests/prose_claims.rs::claim_rd_record_violation_and_systemic_escalation] [test: crates/overwatch/tests/prose_claims.rs::claim_rd_recurrence_default_policy_and_overrides].

```sh
overwatch record-violation --source <gate> --discriminator <d> --task <key> [--symbol <sym>] [--session <sid>] [--detail <text>]
overwatch violations  [--json] [--threshold N] [--window-secs S]   # recurrence stats for every signature [test: crates/overwatch/tests/prose_claims.rs::claim_rd_violations_cli_prints_all_signatures]
overwatch escalations [--json] [--threshold N] [--window-secs S]   # only the signatures judged systemic [test: crates/overwatch/tests/prose_claims.rs::claim_rd_escalations_cli_only_systemic]
```

- `--source` accepts `blastguard | propguard | specguard | mutategate | donegate | reviewgate | tdd | budgetguard | autoflow | ctxrot`; an unknown token does not parse as a source [test: crates/overwatch/src/violation.rs::violation_source_parse_round_trips_all_variants] [test: crates/overwatch/src/violation.rs::violation_source_parse_rejects_unknown_token].
- Recurrence detection (`violation::detect_recurrence`) takes the clock as an argument [test: crates/overwatch/tests/violation.rs::recurrence_window_excludes_stale_events_deterministically].
- Default policy: 3+ occurrences within a 24h window (`RecurrencePolicy::default`), overridable per call with `--threshold` / `--window-secs`. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_recurrence_default_policy_and_overrides]
- A signature is systemic only when it reaches the threshold **and** spans more than one distinct task or session; one task retrying the same gate is a local retry loop, not systemic [test: crates/overwatch/src/violation.rs::is_systemic_requires_threshold_and_multiple_tasks] [test: crates/overwatch/src/violation.rs::is_systemic_false_below_threshold_even_with_multiple_tasks] [test: crates/overwatch/src/violation.rs::is_systemic_true_when_multiple_sessions_same_task_key_reused].
- Stored in `violations.jsonl` (append-only, separate from `events.jsonl`) under the same `<base>/<project-key>/overwatch/` directory. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_violations_jsonl_separate_stream]

`overwatch --help` lists every subcommand with a full description. [test: crates/overwatch/tests/prose_claims.rs::claim_rd_help_lists_every_subcommand]

- canary rollout: `canary-plan` (pure stage split), `canary-gate` (PROCEED/ROLLBACK JSON; **exit 3** when rollback is advised), `canary-rollback-plan` (pure restore plan), `record-rollback` (records a rollback event in `rollbacks.jsonl`) [test: crates/overwatch/tests/prose_claims.rs::claim_rd_canary_commands_behaviour]
- review surface: `review-queue [--json] [--since] [--limit]` (merges five sources — systemic violations, canary rollbacks, AI review findings, condukt escalations and unresolved merge conflicts — into one severity-ordered list [test: crates/overwatch/src/review_queue.rs::stale_high_severity_outranks_fresh_low_severity] [test: crates/overwatch/tests/review_queue_five_sources.rs::review_queue_merges_exactly_five_sources]), `record-finding`, `record-disposition`, `review-metrics`, `compact-findings`, `resolve-merge-conflict`, `reconcile-fixed [--since-ref\|--range\|--last-n] [--dry-run]` (default last 50 commits), `auto-approved [--since] [--sample 5] [--seed 0]` [test: crates/overwatch/tests/prose_claims.rs::claim_rd_review_surface_defaults]
- Continuous-Audit: `audit-round record` / `audit-round close`, `audit-metrics [--window 3]` [test: crates/overwatch/tests/prose_claims.rs::claim_rd_continuous_audit_commands]
- telemetry: `undetermined-metrics [--window-days N]`, `gate-outcomes` (donegate / tdd / reviewgate firing outcomes from `~/.<gate>/state/log.jsonl`) [test: crates/overwatch/tests/prose_claims.rs::claim_rd_telemetry_commands]

Exit-code convention for the read side: when a ledger/journal **cannot be read** (as opposed to being absent or empty), `review-queue` [test: crates/overwatch/tests/undetermined_e2e.rs::undecodable_violation_ledger_is_not_rendered_as_no_systemic_violations], `review-metrics` [test: crates/overwatch/tests/undetermined_e2e.rs::review_metrics_with_an_unreadable_disposition_ledger_reports_null_not_zero], `reconcile-fixed` [test: crates/overwatch/tests/undetermined_e2e.rs::reconcile_fixed_with_an_unreadable_disposition_ledger_writes_nothing] and `gate-outcomes` [test: crates/overwatch/tests/gate_outcomes_cli.rs::non_utf8_log_is_undetermined_with_null_counts_and_exit_3] exit **3**, and `auto-approved` exits non-zero [test: crates/overwatch/tests/auto_approved_cli.rs::unreadable_journal_does_not_exit_clean]. The git history counts as such an input: when `git log` cannot be read (not a repository, git missing, an unknown ref), `reconcile-fixed` exits 3 with `commits_scanned: null` and `git log` in `undetermined_sources` [test: crates/overwatch/tests/reconcile_fixed.rs::reconcile_fixed_undetermined_when_not_a_git_repo], and `review-metrics` reports a `null` stale-undisposed count instead of 0 [test: crates/overwatch/tests/repro_review_metrics_stale_count_not_a_git_repo.rs::review_metrics_stale_count_outside_git_repo_is_not_zero].

MIT [test: crates/overwatch/tests/prose_claims.rs::claim_rd_license_mit]
