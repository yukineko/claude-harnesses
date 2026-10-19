The **SessionStart** hook injects pending work as context when a session opens [test: crates/backlog/tests/prose_claims.rs::claim_r_hook_injects_pending_work].

To drain the whole queue automatically, use `/flow` instead [test: crates/backlog/tests/prose_claims.rs::claim_s_flow_1].

## Use the subcommands

- `add` — Append a task (`--repro-test`); see **File only with a reproducing test** [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]
- `list` — List the store's tasks, filterable by `--tag` / `--status` (`pending` / `done` / `failed` / `cancelled` / `claimed`; `open` is not a status; `claimed` is derived) and `--json` [test: crates/backlog/src/store.rs::list_with_tag_filter] [test: crates/backlog/src/store.rs::list_with_status_filter] [test: crates/backlog/tests/integration.rs::list_json_emits_machine_readable_array] [test: crates/backlog/tests/backlog_0dafa254.rs::list_status_cancelled_is_a_recognised_status] [test: crates/backlog/tests/backlog_s03_audit.rs::list_status_claimed_is_a_recognised_status] [test: crates/backlog/src/task.rs::status_warning_flags_unknown_and_suggests_valid] [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]. For a repo store `--project` is an assertion, not a filter — see **Scope** [test: crates/backlog/tests/project_scope.rs::an_explicit_project_naming_this_repo_changes_nothing] [test: crates/backlog/tests/project_scope.rs::an_explicit_project_naming_a_different_repo_is_refused]
- `next` — Print the next highest-priority pending task that carries a reproduced repro, as JSON (`--claim` reserves it — see below) [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_hands_out_the_reproduced_row_over_a_repro_less_one] [test: crates/backlog/src/store.rs::next_returns_highest_priority]
- `done <id>` — Mark a task done **with evidence**: `--test CMD --red-rev REV` (`--reason fixed|already-fixed|obsolete`), `--doc-only COMMIT`, or `--duplicate-of ID` [test: crates/backlog/tests/closure_evidence.rs::allowlisted_bash_script_with_red_rev_closes_and_records_closure] [test: crates/backlog/tests/closure_evidence.rs::already_fixed_with_red_rev_is_accepted_and_recorded] [test: crates/backlog/tests/closure_evidence.rs::obsolete_with_committed_test_and_red_rev_is_accepted] [test: crates/backlog/tests/doc_only_closure.rs::plain_docs_paths_are_accepted] [test: crates/backlog/tests/closure_evidence.rs::duplicate_of_pending_target_is_accepted]. Exactly one of those forms is required [test: crates/backlog/tests/prose_claims.rs::claim_r_exactly_one_of_those_forms]. A bare `done <id>` is refused [test: crates/backlog/tests/closure_evidence.rs::bare_done_is_refused_and_status_unchanged]
  - The RED run is `--red-rev` in a detached temp worktree with HEAD's test files overlaid (every test file for `cargo test`, the named files for pytest, the script for `bash`/`sh`), so a test added with the fix exists there. A refusal because the test passes at `--red-rev` says so. When the fix itself lives in a test file, that overlay copies the fix in. `--red-overlay PATH` (repeatable) narrows the overlay to the named repro side [test: crates/backlog/tests/f2p_red_overlay.rs::fix_in_a_test_file_closes_with_a_repro_only_overlay] [test: crates/backlog/tests/f2p_red_overlay.rs::default_overlay_refusal_names_the_head_test_overlay]. It only narrows: each path must already be in the default overlay [test: crates/backlog/tests/f2p_red_overlay.rs::red_overlay_paths_must_be_head_test_files_in_the_default_overlay] [test: crates/backlog/tests/f2p_red_overlay.rs::red_overlay_cannot_widen_a_narrowed_pytest_overlay]. A test that passes at `--red-rev` is still refused [test: crates/backlog/tests/f2p_red_overlay.rs::repro_only_overlay_still_refuses_a_test_green_at_red_rev]. The narrowed set is recorded as `closure.red.overlay`.
- `fail <id>` — Mark a task failed (`--reason`) [test: crates/backlog/src/store.rs::mark_failed_appends_reason]. It defers the re-run by 2 days [test: crates/backlog/tests/prose_claims.rs::claim_r_defers_the_re_run_by_2_days]
- `cancel <id> --reason R` — Close as "decided not to do it": terminal, never requeued, recorded as a `discard` closure, no test needed [test: crates/backlog/tests/cancel_evidence.rs::cancel_pending_task_records_a_discard_closure_without_tty] [test: crates/backlog/tests/prose_claims.rs::claim_n_cancel_never_requeued]
- `audit-closures` — Read-only classification of every terminal row's closure evidence (`--json`); never writes the store [test: crates/backlog/tests/audit_closures.rs::classifies_each_done_row] [test: crates/backlog/tests/audit_closures.rs::audit_is_read_only_store_byte_identical_and_never_reopens]
- `overlap` — Group queued tasks by declared `touched_files` scope (advisory; undeclared scopes listed separately) [test: crates/backlog/tests/prose_claims.rs::claim_r_overlap]
- `edit <id>` — Update a task's title / tags / notes / status [test: crates/backlog/tests/prose_claims.rs::claim_r_edit_id]
- `sync` — Reconcile the store against its GitHub issues (one-way, local wins) [test: crates/backlog/tests/prose_claims.rs::claim_r_reconcile_the_store_against]. It is report-only unless `--apply` [test: crates/backlog/tests/close_records_content.rs::dry_run_sync_performs_no_gh_invocation] [test: crates/backlog/tests/mirror_gaps_after_0323.rs::only_body_dry_run_lists_the_stale_task_and_invokes_nothing]. `--only both|create|close|body` selects which arms run [test: crates/backlog/tests/sync_only_scope.rs::only_close_closes_everything_and_creates_nothing] [test: crates/backlog/tests/sync_only_scope.rs::only_create_files_everything_and_closes_nothing] [test: crates/backlog/tests/sync_only_scope.rs::default_still_reconciles_both_arms] [test: crates/backlog/tests/mirror_gaps_after_0323.rs::only_body_apply_pushes_issue_edit_with_the_new_body]. `--limit` truncates the plan to that many tasks [test: crates/backlog/tests/prose_claims.rs::claim_r_limit]
- `session-start` — SessionStart hook: inject pending tasks as context [test: crates/backlog/tests/prose_claims.rs::claim_r_hook_injects_pending_work]
- `install` / `uninstall` — Wire/remove the SessionStart hook in `~/.claude/settings.json` (standalone use) [test: crates/backlog/tests/prose_claims.rs::claim_r_wire_remove_the_sessionstart_hook]
- `merge-driver` — git merge driver for the task store — see below [test: crates/backlog/tests/merge_driver.rs::direct_clean_merge_writes_union_into_ours]
- `lock {acquire,release,status,heartbeat,probe}` — Manage the per-project exclusive lock `~/.backlog/locks/<project-slug>.lock` [test: crates/backlog/tests/prose_claims.rs::claim_r_project_slug_is_the_same_fnv_1a] [test: crates/backlog/src/lock.rs::acquire_status_release_cycle]. `probe` judges the holder's progress (git HEAD + transcript growth) [test: crates/backlog/tests/prose_claims.rs::claim_r_probe_judges]. Only a `stalled` holder may be reaped by `acquire` [test: crates/backlog/tests/lock_heartbeat_ttl.rs::frozen_signals_across_the_window_are_confirmed_stalled_and_reaped] [test: crates/backlog/src/lock.rs::stale_heartbeat_but_progressing_holder_is_not_reaped] [test: crates/backlog/src/lock.rs::stale_heartbeat_with_undetermined_progress_is_not_reaped]. While the lock is held (active or heartbeat-stale), `next --claim` from any session other than the holder is refused with a non-zero exit [test: crates/backlog/tests/repro_exclusive_lock_ignored_by_claim.rs::held_exclusive_lock_stops_other_sessions_claiming] [test: crates/backlog/tests/repro_exclusive_lock_ignored_by_claim.rs::lock_holder_claims_and_release_restores_others]. A claimer without `CLAUDE_CODE_SESSION_ID` is refused too, and so is any claim while the lock file is unreadable [test: crates/backlog/src/lock.rs::claim_permitted_truth_table]
- `driver {register,heartbeat,unregister,status}` — Announce/observe a project's queue drivers WITHOUT excluding them [test: crates/backlog/src/driver.rs::second_driver_registration_is_never_refused] [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]. `heartbeat` also refreshes this session's `next --claim` leases [test: crates/backlog/tests/backlog_486e83ca.rs::live_holder_heartbeat_keeps_lease_past_stale_window]


## Install as a plugin

The plugin's `hooks/hooks.json` registers the SessionStart hook (`backlog session-start`) [test: crates/backlog/tests/session_start_hook_wired.rs::session_start_hook_is_declared_in_the_plugin] [test: crates/backlog/tests/session_start_hook_wired.rs::session_start_command_resolves_through_plugin_root].
The registered hook's timeout is 10s [test: crates/backlog/tests/prose_claims.rs::claim_r_timeout_is_10s].
`backlog install` is for standalone (cargo) use: it merges a `SessionStart` group into `~/.claude/settings.json` (idempotent, ownership-marked) [test: crates/backlog/tests/prose_claims.rs::claim_r_wire_remove_the_sessionstart_hook].

## Store writes need a linked worktree [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_primary_checkout_is_refused_and_writes_nothing]

`add` / `done` / `fail` / `edit` / `cancel` / `sync --apply` are **refused** (non-zero exit,
nothing written) when the store lies inside the *primary* working tree of a git repo, so run them
from a linked worktree (`git worktree add …`) [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_primary_checkout_is_refused_and_writes_nothing] [test: crates/backlog/tests/store_worktree_boundary.rs::done_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary_env.rs::fail_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary.rs::edit_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary_env.rs::cancel_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary.rs::sync_apply_from_primary_checkout_is_refused_and_creates_no_issue].
If git cannot say which kind of checkout this is, `add` is refused too [test: crates/backlog/tests/store_worktree_boundary_env.rs::add_where_dot_git_points_at_a_missing_gitdir_is_refused].
`list` works in the primary tree [test: crates/backlog/tests/store_worktree_boundary.rs::list_from_primary_checkout_still_works].
`next --claim` is allowed there too and leaves `git status` clean [test: crates/backlog/tests/store_worktree_boundary.rs::next_claim_from_primary_checkout_is_allowed_and_never_dirties_main].
The SessionStart hook skips its requeue in the primary tree and says so [test: crates/backlog/tests/store_boundary_hooks.rs::session_start_in_primary_skips_the_requeue_visibly_and_does_not_write].

## Run standalone from cargo

```sh
cargo install --path .
backlog add --title "Fix X" --project "$PWD" --priority p1 \
  --repro-test "bash tests/repro_fix_x.sh"                  # file an item ONLY if the committed test fails at HEAD
backlog list --status pending                                # see the queue
backlog next                                                 # pick the next item [test: crates/backlog/src/store.rs::next_returns_highest_priority]
backlog done <id> --test "cargo test -p x" --red-rev <rev>  # resolve it (RED at rev, GREEN at HEAD) [test: crates/backlog/tests/closure_evidence.rs::allowlisted_bash_script_with_red_rev_closes_and_records_closure] [test: crates/backlog/tests/closure_evidence.rs::red_rev_where_test_already_passes_is_refused]
backlog done <id> --doc-only <commit>   # or: a doc-only ancestor commit [test: crates/backlog/tests/doc_only_closure.rs::plain_docs_paths_are_accepted]
backlog done <id> --duplicate-of <id>   # or: a duplicate [test: crates/backlog/tests/closure_evidence.rs::duplicate_of_pending_target_is_accepted]
backlog fail <id> --reason "blocked"   # defer it 2 days (it comes back as pending) [test: crates/backlog/tests/prose_claims.rs::claim_r_defers_the_re_run_by_2_days] [test: crates/backlog/tests/prose_claims.rs::claim_n_failed_requeued_after_defer]
backlog cancel <id> --reason "won't do"   # discard it (terminal, never requeued; recorded as a `discard` closure, no test needed [test: crates/backlog/tests/prose_claims.rs::claim_n_cancel_never_requeued]) [test: crates/backlog/tests/cancel_evidence.rs::cancel_pending_task_records_a_discard_closure_without_tty]
backlog lock status         # who holds the project's lock [test: crates/backlog/src/lock.rs::acquire_status_release_cycle]
backlog install             # merge the SessionStart hook into settings.json [test: crates/backlog/tests/prose_claims.rs::claim_r_wire_remove_the_sessionstart_hook]
backlog uninstall           # remove it again [test: crates/backlog/tests/prose_claims.rs::claim_r_wire_remove_the_sessionstart_hook]
```

`install`/`uninstall` accept `--dry-run` to print the resulting settings without
writing [test: crates/backlog/tests/prose_claims.rs::claim_r_accept_dry_run].

## File only with a reproducing test

- `add` writes a task **only** when `--repro-test CMD` fails behaviourally at HEAD; the task lands
  `pending` with the reproduced repro recorded [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome].
- Any other outcome is refused with a non-zero exit and nothing written to the store: no `--repro-test` [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_without_repro_writes_nothing_anywhere],
  not reproduced [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere], undetermined [test: crates/backlog/tests/repro_evidence.rs::undetermined_repro_files_no_row_and_writes_nothing].
- `next` / `next --claim` never hand out a row without a reproduced repro [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_skips_pending_row_without_repro] [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_claim_skips_pending_row_without_repro].
- A store holding a row with status `unconfirmed` or `needs-ruling` is refused by `list` and `next`,
  naming the row [test: crates/backlog/tests/no_unconfirmed.rs::list_fails_closed_on_an_unconfirmed_row_in_tasks_toml] [test: crates/backlog/tests/no_unconfirmed.rs::next_fails_closed_on_an_unconfirmed_row_in_tasks_toml] [test: crates/backlog/tests/no_needs_ruling.rs::list_fails_closed_on_a_needs_ruling_row_in_tasks_toml] [test: crates/backlog/tests/no_needs_ruling.rs::next_fails_closed_on_a_needs_ruling_row_in_tasks_toml] [test: crates/backlog/tests/status_type.rs::store_with_unconfirmed_row_fails_naming_id_and_status] [test: crates/backlog/tests/status_type.rs::store_with_needs_ruling_row_fails_naming_id_and_status].
- `backlog audit-open [--json]` audits OPEN rows (pending / failed) as `has-test` / `no-test` /
  `undetermined`, read from the recorded repro [test: crates/backlog/tests/audit_open_once.rs::first_run_classifies_open_rows_and_excludes_terminal].
- `audit-open` runs no test [test: crates/backlog/tests/audit_open_once.rs::no_test_is_executed].
- Determined results go to the ledger `~/.backlog/audit-open/<project-slug>.json` [test: crates/backlog/tests/audit_open_once.rs::first_run_ledger_records_has_and_no_test_but_not_undetermined].
- A determined row is skipped on later runs, even if a repro is added later [test: crates/backlog/tests/audit_open_once.rs::row_checked_as_no_test_stays_skipped_after_repro_added].
- An undetermined row is re-examined on the next run [test: crates/backlog/tests/audit_open_once.rs::second_run_skips_checked_and_reexamines_undetermined].
- An unreadable ledger fails the command [test: crates/backlog/tests/audit_open_once.rs::garbage_ledger_is_undetermined_human] [test: crates/backlog/tests/audit_open_once.rs::garbage_ledger_is_undetermined_json].

`add` derives a **content hashkey** from the title and project (title trimmed →
Unicode NFKC → lowercased → runs of whitespace collapsed to one space →
leading/trailing punctuation stripped, folded with `project` via 64-bit FNV-1a
into a 16-hex-digit key) [test: crates/backlog/src/task.rs::hashkey_is_16_hex_chars] [test: crates/backlog/src/task.rs::hashkey_normalizes_trivial_phrasing_differences] [test: crates/backlog/src/task.rs::hashkey_differs_across_projects] [test: crates/backlog/tests/prose_claims.rs::claim_n_hashkey_normalisation], and rejects the add when either of the following is true. `done` never
blocks a re-add of the same title [test: crates/backlog/src/store.rs::add_does_not_block_on_done_duplicate]:

- an existing `pending` or `failed` task already has that hashkey [test: crates/backlog/src/store.rs::add_rejects_duplicate_pending_hashkey] [test: crates/backlog/src/store.rs::add_rejects_duplicate_failed_hashkey], or
- `condukt` (looked up in the plugin cache first, then on `PATH`) is found [test: crates/backlog/tests/prose_claims.rs::claim_n_condukt_resolution_order] and `condukt state is-claimed --hashkey <h>` exits 0
  (another live session holds a claim on it) [test: crates/backlog/tests/claim_check_contract.rs::exit0_claimed_true_is_refused_as_claimed]. Exit 1 counts as "not claimed"
  only when stdout also carries `"claimed": false` [test: crates/backlog/tests/claim_check_contract.rs::control_exit1_with_claimed_false_json_adds]. Any other outcome — `condukt`
  not found or failing to spawn [test: crates/backlog/tests/prose_claims.rs::claim_n_condukt_resolution_order] [test: crates/backlog/tests/claim_check_contract.rs::non_executable_condukt_is_refused],
  an exit code other than 0/1 (for example 3 = claim registry unreadable) [test: crates/backlog/src/store.rs::is_claimed_elsewhere_maps_exit_codes_three_valued] [test: crates/backlog/tests/prose_claims.rs::claim_n_claim_check_undetermined_refuses_add],
  exit 0/1 without the matching `claimed` field [test: crates/backlog/tests/claim_check_contract.rs::exit1_garbage_stdout_is_refused] [test: crates/backlog/tests/claim_check_contract.rs::exit1_empty_stdout_registry_error_is_refused] [test: crates/backlog/tests/claim_check_contract.rs::exit0_with_claimed_false_contradiction_is_refused],
  exit 1 with `"claimed": true` (exit code and field disagree) [test: crates/backlog/tests/claim_check_contract.rs::exit1_with_claimed_true_contradiction_is_refused], termination by signal [test: crates/backlog/tests/claim_check_contract.rs::signal_killed_condukt_is_refused], or no
  answer in time [test: crates/backlog/tests/claim_check_slow_helper.rs::never_answering_helper_is_refused_within_bounded_time] — means the claim check could not be made, and the `add` is
  **refused** with the reason (a possible duplicate of a live claimed task
  cannot be ruled out). "Cannot determine" is never read as "not claimed" [test: crates/backlog/tests/backlog_s03_audit.rs::add_does_not_treat_an_undetermined_condukt_claim_check_as_not_claimed] [test: crates/backlog/tests/prose_claims.rs::claim_n_claim_check_undetermined_refuses_add].

Each rejection can be bypassed intentionally with `backlog add --force` [test: crates/backlog/src/store.rs::add_force_bypasses_duplicate_guard] [test: crates/backlog/tests/claim_check_contract.rs::force_overrides_an_undetermined_claim_check] [test: crates/backlog/tests/prose_claims.rs::claim_n_add_force_bypasses_claimed].

Each element of `backlog list --json` carries a `hashkey` field (computed from
title + project, not stored) [test: crates/backlog/tests/prose_claims.rs::claim_r_carries_a_hashkey_field].

The store resolves per repo — `<repo root>/.backlog/tasks.toml` [test: crates/backlog/src/config.rs::project_in_a_repo_resolves_to_the_repo_local_store].

- **Reads apply no project filter:** every task in the file is in scope,
  whichever checkout wrote it [test: crates/backlog/tests/project_scope.rs::a_task_recorded_under_another_checkouts_label_is_listed] [test: crates/backlog/tests/project_scope.rs::next_hands_out_a_task_recorded_under_another_checkouts_label].

  - [record: 2026-08-20 rev: 89feaddb] In this repo's own store, pending-or-failed tasks by label: macOS 265, WSL 70, `C:/…` 5.

  Run this to re-measure:

  ```sh
  python3 -c "import collections,tomllib; d=tomllib.load(open('.backlog/tasks.toml','rb')); \
    print(collections.Counter(t['project'] for t in d['task'] if t['status'] in ('pending','failed')).most_common())"
  ```

  `--project` therefore becomes an **assertion** about which store you meant —
  naming this repo changes nothing [test: crates/backlog/tests/project_scope.rs::an_explicit_project_naming_this_repo_changes_nothing], naming a different one is an error, not a
  filtered listing [test: crates/backlog/tests/project_scope.rs::an_explicit_project_naming_a_different_repo_is_refused]. `--all` is accepted [test: crates/backlog/tests/integration.rs::list_all_flag_returns_every_project_in_the_resolved_store].
- **A cwd with no repo above it has no store:** `add` and `list` refuse with
  the reason instead of falling back to the cross-project `~/.backlog` [test: crates/backlog/tests/prose_claims.rs::claim_s_add_outside_repo_names_the_missing_repo] [test: crates/backlog/tests/project_scope.rs::list_outside_any_repo_refuses_instead_of_reading_the_shared_store]. `next` refuses the same way [test: crates/backlog/tests/prose_claims.rs::claim_r_next_refuses_the_same_way]. To opt into a shared store anyway, pin `store_dir` in
  `~/.backlog/config.toml`; a pinned store may hold several projects, so there
  `--project` stays an ordinary filter [test: crates/backlog/tests/project_scope.rs::a_pinned_store_dir_still_scopes_by_project] [test: crates/backlog/tests/project_scope.rs::a_pinned_store_dir_still_works_outside_any_repo].

The `project` field is still recorded — it says which checkout filed the task,
and `list` marks it `[project unresolved: …]` when that label was a guess [test: crates/backlog/tests/project_identity.rs::add_under_an_undetermined_scope_marks_the_stored_task] [test: crates/backlog/tests/prose_claims.rs::claim_n_list_marks_project_unresolved] — but
it no longer decides what you can see [test: crates/backlog/tests/project_scope.rs::a_task_recorded_under_another_checkouts_label_is_listed].

A store is two sibling files: `tasks.toml` (the live queue) and
`tasks.done.toml` (every terminal row — `done` or `cancelled`) [test: crates/backlog/tests/done_split.rs::done_moves_row_into_done_file_and_out_of_tasks_file] [test: crates/backlog/tests/done_split.rs::cancelled_row_is_migrated_to_done_file].
The done file of `.backlog/tasks.toml` is `.backlog/tasks.done.toml` [test: crates/backlog/tests/prose_claims.rs::claim_r_dir_stem_done_toml].

- **Reads see the union:** readers return both files' rows [test: crates/backlog/tests/done_split.rs::readers_see_rows_that_live_only_in_the_done_file].
  `list --status done`, `done <id>` and `edit <id>` see rows that live only in the done file [test: crates/backlog/tests/done_split.rs::readers_see_rows_that_live_only_in_the_done_file].
  `sync` sees them too, and the near-duplicate scan does not offer a terminal row as a peer [test: crates/backlog/tests/prose_claims.rs::claim_r_sync_the_duplicate_guard_and_the_near_du] [test: crates/backlog/tests/prose_claims.rs::claim_n_near_dup_terminal_not_peer].
  The duplicate guard decides exactly as before: a `done` row does
  not block a re-add, wherever it is stored [test: crates/backlog/tests/done_split.rs::duplicate_guard_decision_is_unchanged_for_a_row_in_the_done_file].
- **Terminal wins, and terminal is final:** when an id appears in both files
  (for example a git merge re-introduced a completed task as `pending` in
  `tasks.toml`), the terminal row is the one returned, once; `next` never hands
  it out [test: crates/backlog/tests/done_split.rs::done_in_done_file_beats_pending_in_tasks_file] [test: crates/backlog/tests/prose_claims.rs::claim_n_cancelled_terminal_wins_and_edit_refused]. `edit --status pending|failed` on a `done`/`cancelled` task is refused
  with a non-zero exit [test: crates/backlog/tests/done_split.rs::edit_cannot_revert_a_done_task_to_pending] [test: crates/backlog/tests/done_split_verifier.rs::edit_status_failed_cannot_reopen_a_done_task] [test: crates/backlog/tests/prose_claims.rs::claim_n_cancelled_terminal_wins_and_edit_refused], and `fail` on one is refused too [test: crates/backlog/tests/done_split_verifier.rs::fail_cannot_reopen_a_done_task]; the task stays
  terminal. `done` on an already-done task is still an idempotent success [test: crates/backlog/src/store.rs::mark_done_is_idempotent_on_repeat_call].
- **A bad done file is an error, not an empty one:** a missing
  `tasks.done.toml` just means nothing was completed yet [test: crates/backlog/tests/done_split.rs::missing_done_file_is_fine]. One that exists but
  cannot be read or parsed makes `list` and `next` exit non-zero with an error naming that file — never a listing that silently lacks the done rows [test: crates/backlog/tests/done_split.rs::unparseable_done_file_makes_list_and_next_fail_closed]. `add`, `done`, `edit`, `fail` and `sync` exit non-zero the same way, and never write a store that would overwrite the done rows [test: crates/backlog/tests/prose_claims.rs::claim_r_add_done_edit_fail_and_sync_exit_non_zer]. The
  SessionStart hook injects a "store UNREADABLE" notice instead of an empty
  queue [test: crates/backlog/tests/prose_claims.rs::claim_r_store_unreadable].
- **Writes partition:** every save writes the terminal rows to the done file and
  the rest to `tasks.toml` [test: crates/backlog/tests/done_split.rs::done_moves_row_into_done_file_and_out_of_tasks_file]. Newly finished rows are appended, so a completion's git diff is
  a pure append [test: crates/backlog/tests/done_split.rs::done_file_is_appended_in_completion_order]. Rows already in the done file keep their position, and are rewritten in place only if their content changed (for example a `sync` stamp or a notes edit) [test: crates/backlog/tests/prose_claims.rs::claim_r_rows_already_in_the_done_file_keep_their]. The done file is written first and is not rewritten when nothing
  in it changed [test: crates/backlog/tests/prose_claims.rs::claim_r_done_file_is_written_first] [test: crates/backlog/tests/prose_claims.rs::claim_r_rows_already_in_the_done_file_keep_their]. `tasks.toml` is replaced atomically (fsync'd temp + rename) [test: crates/backlog/src/store.rs::save_fsyncs_temp_before_rename_and_dir_after]. A
  crash between the two writes leaves the row in both files, which
  terminal-wins resolves [test: crates/backlog/tests/prose_claims.rs::claim_r_a_crash_between_the_two_writes].
- **Old binaries migrate on the next write:** a `done`/`cancelled` row still in `tasks.toml` is listed normally and
  moves to the done file the next time any command saves the store [test: crates/backlog/tests/done_split.rs::legacy_done_row_in_tasks_file_is_listed_and_migrated_on_next_write].

`.gitattributes` marks both `.backlog/tasks.toml` and `.backlog/tasks.done.toml` `merge=backlog` [test: crates/backlog/tests/prose_claims.rs::claim_r_gitattributes_marks_both].
`backlog merge-driver <base> <ours> <theirs>` (git's `%O %A %B`) merges them per
task id instead of per line [test: crates/backlog/tests/merge_driver.rs::both_add_different_tasks_merges_clean_with_union] [test: crates/backlog/tests/merge_driver.rs::direct_clean_merge_writes_union_into_ours]:

- the union of ids; a task deleted on one side (relative to base) and left
  unchanged on the other stays deleted [test: crates/backlog/tests/merge_driver.rs::deletion_on_one_side_stays_deleted]; a task changed on only one side takes
  that side [test: crates/backlog/tests/merge_driver.rs::edit_and_add_both_survive];
- the same id edited differently on both sides is a **conflict**: no side is
  picked, the driver exits non-zero [test: crates/backlog/tests/merge_driver.rs::same_task_edited_differently_conflicts]. The same holds for edit/delete and for the same new id added with different content [test: crates/backlog/tests/prose_claims.rs::claim_r_the_same_holds_for_edit_delete]. `<ours>` is left untouched and git marks
  the file unmerged for a human [test: crates/backlog/tests/prose_claims.rs::claim_r_ours_is_left_untouched];
- an input that does not parse is a non-zero exit with `<ours>` untouched [test: crates/backlog/tests/merge_driver.rs::unparseable_theirs_fails_and_leaves_ours_untouched];
- a row without a string `id`, a duplicate id in one input, or an unknown top-level key is also a non-zero exit
  with `<ours>` untouched [test: crates/backlog/tests/prose_claims.rs::claim_r_a_row_without_a_string_id]. A clean result is written atomically [test: crates/backlog/tests/prose_claims.rs::claim_r_a_clean_result_is_written_atomically].

Output is rendered with the same writer `backlog` uses when every row
round-trips through the task schema, so merging a store with itself is
byte-identical; rows carrying fields this binary does not know are kept (the
generic TOML rendering is used then) [test: crates/backlog/tests/prose_claims.rs::claim_r_output_is_rendered_with_the_same_writer].

Run this to configure the driver per repository:

```sh
backlog merge-driver --install   # run inside the repo
```

This sets the repo's `merge.backlog.driver` to run `backlog merge-driver %O %A %B` [test: crates/backlog/tests/backlog_57a98a8f_68b8092f_regression.rs::control_install_configures_the_driver].

The store follows the checkout on purpose: `<repo root>/.backlog/tasks.toml`,
where a linked worktree counts as its own root [test: crates/backlog/src/config.rs::a_worktree_whose_dot_git_is_a_file_is_still_a_root] [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_linked_worktree_succeeds_and_lands_in_that_worktree].

`next --claim` takes a second, WIDER lock first, and records the claim in a
machine-global ledger keyed by project IDENTITY, not by store location [test: crates/backlog/tests/cross_checkout_claim.rs::two_checkouts_of_the_same_project_never_claim_the_same_task] [test: crates/backlog/tests/prose_claims.rs::claim_r_lock_order_never_inverted]:

- lock order, never inverted: `~/.backlog/claims/<slug>.lock` (project-wide),
  then `<store>.tasks.toml.lock` (this checkout) [test: crates/backlog/tests/prose_claims.rs::claim_r_lock_order_never_inverted]
- ledger: `~/.backlog/claims/<project-slug>.json`; a linked worktree normalizes to its main working tree, so every checkout of one project shares one ledger [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]. `<project-slug>` is the same FNV-1a project hash `backlog lock` uses [test: crates/backlog/tests/prose_claims.rs::claim_r_project_slug_is_the_same_fnv_1a]
- an entry (a **lease**) stops excluding once its holder has not been seen
  alive for 1h (`CLAIM_STALE_SECS`), so a dead claimant cannot strand a task
  everywhere [test: crates/backlog/tests/claim_lease_untracked.rs::stale_lease_releases_the_task] [test: crates/backlog/tests/backlog_486e83ca.rs::holder_whose_last_heartbeat_aged_out_is_reclaimable] [test: crates/backlog/tests/prose_claims.rs::claim_n_lease_stale_threshold_1h]. Entries are kept for 7 days for a human reading the file [test: crates/backlog/tests/prose_claims.rs::claim_r_kept_for_7_days]
- "seen alive" = the newer of the claim time and the holder's last lease
  heartbeat [test: crates/backlog/tests/backlog_486e83ca.rs::live_holder_heartbeat_keeps_lease_past_stale_window] [test: crates/backlog/tests/backlog_486e83ca.rs::holder_whose_last_heartbeat_aged_out_is_reclaimable]. `next --claim` records the claiming session
  (`CLAUDE_CODE_SESSION_ID`), and `backlog driver heartbeat --session-id S
  --project P` also refreshes S's still-live leases in P's ledger, so a driver
  heartbeating at least hourly keeps its claims however long the task takes
  (backlog 486e83ca) [test: crates/backlog/tests/backlog_486e83ca.rs::live_holder_heartbeat_keeps_lease_past_stale_window] [test: crates/backlog/tests/backlog_486e83ca.rs::another_sessions_heartbeat_does_not_refresh_the_lease]. An already-expired lease is not revived [test: crates/backlog/tests/backlog_486e83ca.rs::heartbeat_does_not_revive_an_expired_lease]. A lease with no
  session (an older binary's entry, or a claim outside a session) ages by claim
  time alone [test: crates/backlog/tests/backlog_486e83ca.rs::sessionless_claim_ages_by_claimed_at] [test: crates/backlog/tests/backlog_486e83ca.rs::legacy_entry_without_liveness_fields_still_parses_and_ages]. If the leases cannot be refreshed (for example the ledger is unreadable) the heartbeat exits non-zero saying so [test: crates/backlog/tests/backlog_486e83ca.rs::heartbeat_reports_failure_when_ledger_unreadable]; a
  malformed heartbeat field makes the ledger unparseable, which refuses claims
  rather than treating the holder as dead [test: crates/backlog/tests/backlog_486e83ca.rs::unparseable_heartbeat_field_refuses_rather_than_reclaims] [test: crates/backlog/tests/verifier_486e83ca.rs::verifier_malformed_heartbeat_refused_by_every_reader]

Every undetermined condition on this path **refuses the claim** and exits
non-zero with the reason on stderr — never `no pending tasks` on exit 0, which
a driver reads as "there is no work" [test: crates/backlog/tests/cross_checkout_claim.rs::an_unreadable_ledger_refuses_the_claim_instead_of_answering_empty] [test: crates/backlog/tests/project_identity.rs::next_claim_fails_closed_when_the_project_identity_cannot_be_determined]. That covers: the ledger lock not being acquired [test: crates/backlog/src/claim_ledger.rs::a_held_ledger_lock_refuses_rather_than_claiming_unprotected], the ledger being
unreadable/unparseable [test: crates/backlog/src/claim_ledger.rs::unparseable_ledger_refuses_the_claim], the tasks-file lock not being held [test: crates/backlog/src/store.rs::next_claim_refuses_when_lock_not_held_instead_of_racing_unprotected], and a
project identity that cannot be resolved [test: crates/backlog/tests/project_identity.rs::next_claim_fails_closed_when_the_project_identity_cannot_be_determined]. A refusal is not an empty queue [test: crates/backlog/tests/claim_lease_untracked.rs::unreadable_ledger_refuses_list_and_next] [test: crates/backlog/tests/cross_checkout_claim.rs::an_unreadable_ledger_refuses_the_claim_instead_of_answering_empty]. The ledger directory not being creatable and the ledger being unwritable are refused the same way [test: crates/backlog/tests/prose_claims.rs::claim_r_the_ledger_directory_not_being_creatable].

### The lease never touches the tracked store; `claimed` is derived [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]

A claim writes **only** the ledger [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]. The tracked `.backlog/tasks.toml` is not
modified by `next --claim`, and SessionStart's requeue never rewrites rows on account of
claims — so claiming leaves the git worktree clean [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees] [test: crates/backlog/tests/claim_lease_untracked.rs::session_start_does_not_dirty_the_store_for_claims]. `claimed` is a DERIVED status, decided per row from its stored status and its live lease (if any) [test: crates/backlog/tests/claim_lease_release.rs::claimant_fail_is_displayed_failed_but_still_excludes] [test: crates/backlog/tests/claim_lease_release.rs::reclaimed_old_failed_row_is_displayed_claimed]:

- a `pending` row with a live lease is shown `claimed` [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees];
- a `failed` row with a live lease is shown `claimed` only if the lease was
  taken strictly AFTER the row's `updated_at` (a re-claim of an older failed
  task) [test: crates/backlog/tests/claim_lease_release.rs::reclaimed_old_failed_row_is_displayed_claimed] [test: crates/backlog/tests/claim_lease_release.rs::failed_row_claimed_boundary_is_strict]. A row updated at or after the claim — the claimant ran `fail` — is
  not shown `claimed`, and `--status failed` selects it [test: crates/backlog/tests/claim_lease_release.rs::claimant_fail_is_displayed_failed_but_still_excludes] [test: crates/backlog/tests/claim_lease_release.rs::failed_row_claimed_boundary_is_strict];
- a `done`/`cancelled` row is never shown `claimed` [test: crates/backlog/tests/prose_claims.rs::claim_r_is_never_shown_claimed].

This rule is display-only [test: crates/backlog/tests/claim_lease_release.rs::claimant_fail_is_displayed_failed_but_still_excludes]. Exclusion from `next` / `next --claim` is decided
by the live lease alone, so a task its claimant just failed is not listed `claimed`
yet is still not handed out until the lease ages out [test: crates/backlog/tests/claim_lease_release.rs::claimant_fail_is_displayed_failed_but_still_excludes].

- `next --claim` prints the task with `"status": "claimed"`, as before [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees].
- plain `next` skips leased tasks [test: crates/backlog/tests/claim_lease_untracked.rs::plain_next_skips_a_leased_task_in_every_checkout]; `list` (text and `--json`) shows the
  derived status above [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]. The status filter applies to the derived view: `--status claimed` selects the
  rows shown `claimed` [test: crates/backlog/tests/claim_lease_release.rs::reclaimed_old_failed_row_is_displayed_claimed]. `--status pending` omits leased tasks [test: crates/backlog/tests/prose_claims.rs::claim_r_status_pending_omits_leased].
- a row that an older binary persisted as `status = "claimed"` is read as
  `pending`; only a live lease excludes it [test: crates/backlog/tests/claim_lease_untracked.rs::legacy_tracked_claimed_row_without_lease_is_pending]. It is rewritten as `pending` the
  next time an unrelated command saves the store [test: crates/backlog/tests/prose_claims.rs::claim_r_it_is_rewritten_as_pending].
- `done` and `fail` do not release a lease [test: crates/backlog/tests/claim_lease_release.rs::claimant_fail_is_displayed_failed_but_still_excludes] [test: crates/backlog/tests/claim_lease_release.rs::claimant_done_is_not_redispatched_from_another_checkout]. `edit --status pending` does not release a lease either [test: crates/backlog/tests/prose_claims.rs::claim_r_edit_status_pending_does_not_release]. A leased
  non-terminal task stays excluded until the lease ages out (1h after the
  holder was last seen alive) [test: crates/backlog/tests/claim_lease_untracked.rs::stale_lease_releases_the_task] [test: crates/backlog/tests/prose_claims.rs::claim_n_lease_stale_threshold_1h].
- `list`, plain `next` and `next --claim` locate the ledger the same way (the
  project identity, see above); outside any repo that identity is the
  canonical cwd [test: crates/backlog/tests/prose_claims.rs::claim_r_locate_the_ledger_the_same_way]. If the identity cannot be resolved (for example a dangling worktree
  `.git` link) or the ledger exists but cannot be read or parsed, `list` and
  plain `next` **refuse** (non-zero exit, reason on stderr, nothing on
  stdout) rather than show a possibly-claimed task as pending [test: crates/backlog/tests/project_identity.rs::a_dangling_git_link_no_longer_hides_an_existing_task] [test: crates/backlog/tests/claim_lease_untracked.rs::unreadable_ledger_refuses_list_and_next] [test: crates/backlog/tests/prose_claims.rs::claim_n_plain_next_refuses_unresolved_identity].
- the store-divergence check does not count leased tasks as queued work [test: crates/backlog/tests/prose_claims.rs::claim_r_store_divergence_check_does_not_count].

## Build and test

```sh
cargo test
```

Only the `bin/backlog` launcher is tracked in git [test: crates/backlog/tests/prose_claims.rs::claim_r_only_the_bin_backlog_launcher_is_tracked].
It execs the per-platform `backlog-<os>-<arch>` binary placed beside it in the deployed plugin [test: crates/backlog/tests/launcher_missing_binary.rs::control_launcher_still_execs_the_host_binary_when_it_is_present], and exits
non-zero (not 0 with empty output) when no build exists for the host [test: crates/backlog/tests/launcher_missing_binary.rs::every_subcommand_fails_loudly_rather_than_returning_an_empty_result].
