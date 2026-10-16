**SessionStart** フックは、セッションが開いた瞬間に pending なタスクを context として注入する [test: crates/backlog/tests/prose_claims.rs::claim_r_hook_injects_pending_work]。

キューを自動で全件消化したいときは `/flow` を使うこと [test: crates/backlog/tests/prose_claims.rs::claim_s_flow_1]。

## サブコマンドを使うこと

- `add` — `--repro-test` のコミット済みテストが HEAD で落ちた時だけタスクを追加する [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome] [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere]
- `list` — store のタスク一覧: `--tag` / `--status`（`pending` / `done` / `failed` / `cancelled` / `claimed`、`claimed` は導出値、`open` は status ではない）/ `--json` [test: crates/backlog/src/store.rs::list_with_tag_filter] [test: crates/backlog/src/store.rs::list_with_status_filter] [test: crates/backlog/tests/integration.rs::list_json_emits_machine_readable_array] [test: crates/backlog/tests/backlog_0dafa254.rs::list_status_cancelled_is_a_recognised_status] [test: crates/backlog/tests/backlog_s03_audit.rs::list_status_claimed_is_a_recognised_status] [test: crates/backlog/src/task.rs::status_warning_flags_unknown_and_suggests_valid] [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]。repo store では `--project` は絞り込みではなく **assertion**（下の「スコープ」参照） [test: crates/backlog/tests/project_scope.rs::an_explicit_project_naming_this_repo_changes_nothing] [test: crates/backlog/tests/project_scope.rs::an_explicit_project_naming_a_different_repo_is_refused]
- `next` — reproduced な repro を持つ pending タスクのうち最高優先度のものを JSON で出力する（`--claim` で予約） [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_hands_out_the_reproduced_row_over_a_repro_less_one] [test: crates/backlog/src/store.rs::next_returns_highest_priority]
- `done <id>` — タスクを**証拠付きで**完了マークする: `--test CMD --red-rev REV`（`--reason fixed|already-fixed|obsolete`）・`--doc-only COMMIT`・`--duplicate-of ID` [test: crates/backlog/tests/closure_evidence.rs::allowlisted_bash_script_with_red_rev_closes_and_records_closure] [test: crates/backlog/tests/closure_evidence.rs::already_fixed_with_red_rev_is_accepted_and_recorded] [test: crates/backlog/tests/closure_evidence.rs::obsolete_with_committed_test_and_red_rev_is_accepted] [test: crates/backlog/tests/doc_only_closure.rs::plain_docs_paths_are_accepted] [test: crates/backlog/tests/closure_evidence.rs::duplicate_of_pending_target_is_accepted]。この 3 形式のちょうど 1 つが必要 [test: crates/backlog/tests/prose_claims.rs::claim_r_exactly_one_of_those_forms]。素の `done <id>` は拒否される [test: crates/backlog/tests/closure_evidence.rs::bare_done_is_refused_and_status_unchanged]
- `fail <id>` — タスクを失敗マークする (`--reason`) [test: crates/backlog/src/store.rs::mark_failed_appends_reason]。再実行を 2 日先送りする [test: crates/backlog/tests/prose_claims.rs::claim_r_defers_the_re_run_by_2_days]
- `cancel <id> --reason R` — 「やらないと決めた」としてテスト無しで閉じ、`discard` closure として記録する [test: crates/backlog/tests/cancel_evidence.rs::cancel_pending_task_records_a_discard_closure_without_tty]
- `audit-closures` — 全終端行の closure 証拠を read-only で分類する（`--json`） [test: crates/backlog/tests/audit_closures.rs::classifies_each_done_row]。store は書かない [test: crates/backlog/tests/audit_closures.rs::audit_is_read_only_store_byte_identical_and_never_reopens]
- `overlap` — キュー中のタスクを宣言された `touched_files` scope でグループ化する（advisory、未宣言は別に列挙） [test: crates/backlog/tests/prose_claims.rs::claim_r_overlap]
- `edit <id>` — タスクの title / tags / notes / status を更新する [test: crates/backlog/tests/prose_claims.rs::claim_r_edit_id]
- `sync` — store を GitHub issue と突き合わせる（一方向・ローカル優先） [test: crates/backlog/tests/prose_claims.rs::claim_r_reconcile_the_store_against]。`--apply` なしでは報告のみ [test: crates/backlog/tests/close_records_content.rs::dry_run_sync_performs_no_gh_invocation] [test: crates/backlog/tests/mirror_gaps_after_0323.rs::only_body_dry_run_lists_the_stale_task_and_invokes_nothing]。`--only both|create|close|body` で実行する arm を選ぶ [test: crates/backlog/tests/sync_only_scope.rs::only_close_closes_everything_and_creates_nothing] [test: crates/backlog/tests/sync_only_scope.rs::only_create_files_everything_and_closes_nothing] [test: crates/backlog/tests/sync_only_scope.rs::default_still_reconciles_both_arms] [test: crates/backlog/tests/mirror_gaps_after_0323.rs::only_body_apply_pushes_issue_edit_with_the_new_body]。`--limit` は plan をそのタスク数に切り詰める [test: crates/backlog/tests/prose_claims.rs::claim_r_limit]
- `session-start` — SessionStart フック: pending タスクを context として注入する [test: crates/backlog/tests/prose_claims.rs::claim_r_hook_injects_pending_work]
- `install` / `uninstall` — `~/.claude/settings.json` の SessionStart フックを配線/除去する（standalone 用） [test: crates/backlog/tests/prose_claims.rs::claim_r_wire_remove_the_sessionstart_hook]
- `merge-driver` — task store 用の git merge driver（下記参照） [test: crates/backlog/tests/merge_driver.rs::direct_clean_merge_writes_union_into_ours]
- `lock {acquire,release,status,heartbeat,probe}` — project ごとの排他ロック `~/.backlog/locks/<project-slug>.lock` [test: crates/backlog/tests/prose_claims.rs::claim_r_project_slug_is_the_same_fnv_1a] を管理する [test: crates/backlog/src/lock.rs::acquire_status_release_cycle]。`probe` は保有者の進捗（git HEAD + transcript の伸び）を判定する [test: crates/backlog/tests/prose_claims.rs::claim_r_probe_judges]。`stalled` の保有者だけが `acquire` で reap されうる [test: crates/backlog/tests/lock_heartbeat_ttl.rs::frozen_signals_across_the_window_are_confirmed_stalled_and_reaped] [test: crates/backlog/src/lock.rs::stale_heartbeat_but_progressing_holder_is_not_reaped] [test: crates/backlog/src/lock.rs::stale_heartbeat_with_undetermined_progress_is_not_reaped]
- `driver {register,heartbeat,unregister,status}` — project のキュー driver を排他せずに通知・観測する [test: crates/backlog/src/driver.rs::second_driver_registration_is_never_refused] [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。`heartbeat` は自セッションの `next --claim` lease も更新する [test: crates/backlog/tests/backlog_486e83ca.rs::live_holder_heartbeat_keeps_lease_past_stale_window]


### store 書き込みは linked worktree で [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_primary_checkout_is_refused_and_writes_nothing]

`add` / `done` / `fail` / `edit` / `cancel` / `sync --apply` は、store が git リポジトリの **primary（main）作業ツリー**内にある場合
**拒否**される（非 0 終了・何も書かない） [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_primary_checkout_is_refused_and_writes_nothing] [test: crates/backlog/tests/store_worktree_boundary.rs::done_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary_env.rs::fail_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary.rs::edit_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary_env.rs::cancel_from_primary_checkout_is_refused_and_file_unchanged] [test: crates/backlog/tests/store_worktree_boundary.rs::sync_apply_from_primary_checkout_is_refused_and_creates_no_issue]。
linked worktree から実行すること。
git が checkout の種別を答えられない場合も `add` を拒否する [test: crates/backlog/tests/store_worktree_boundary_env.rs::add_where_dot_git_points_at_a_missing_gitdir_is_refused]。
`list` は primary でも動く [test: crates/backlog/tests/store_worktree_boundary.rs::list_from_primary_checkout_still_works]。
`next --claim` も primary で許可され、`git status` は clean のまま [test: crates/backlog/tests/store_worktree_boundary.rs::next_claim_from_primary_checkout_is_allowed_and_never_dirties_main]。
SessionStart フックは primary では requeue をスキップし、その旨を表示する [test: crates/backlog/tests/store_boundary_hooks.rs::session_start_in_primary_skips_the_requeue_visibly_and_does_not_write]。

プラグインの `hooks/hooks.json` が SessionStart フック（`backlog session-start`）を登録する [test: crates/backlog/tests/session_start_hook_wired.rs::session_start_hook_is_declared_in_the_plugin] [test: crates/backlog/tests/session_start_hook_wired.rs::session_start_command_resolves_through_plugin_root]。登録されたフックの timeout は 10 秒 [test: crates/backlog/tests/prose_claims.rs::claim_r_timeout_is_10s]。`backlog install` は standalone（cargo）利用向けで、`SessionStart` グループを `~/.claude/settings.json` にマージする（冪等・所有権マーク付き） [test: crates/backlog/tests/prose_claims.rs::claim_r_wire_remove_the_sessionstart_hook]。`install` / `uninstall` は `--dry-run` で書き込まず結果だけ表示できる [test: crates/backlog/tests/prose_claims.rs::claim_r_accept_dry_run]。

### 最小例を standalone（cargo）で実行すること

```sh
cargo install --path .
backlog add --title "Fix X" --project "$PWD" --priority p1 \
  --repro-test "bash tests/repro_fix_x.sh"                  # コミット済みテストが HEAD で落ちた時だけ起票 [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]
backlog list --status pending
backlog next                                                 # 次の項目をピック [test: crates/backlog/src/store.rs::next_returns_highest_priority]
backlog done <id> --test "cargo test -p x" --red-rev <rev>  # 解決する（REV で RED・HEAD で GREEN を実測） [test: crates/backlog/tests/closure_evidence.rs::allowlisted_bash_script_with_red_rev_closes_and_records_closure] [test: crates/backlog/tests/closure_evidence.rs::red_rev_where_test_already_passes_is_refused]
backlog done <id> --doc-only <commit>                        # または doc だけの祖先 commit [test: crates/backlog/tests/doc_only_closure.rs::plain_docs_paths_are_accepted]
backlog done <id> --duplicate-of <id>                        # または重複 [test: crates/backlog/tests/closure_evidence.rs::duplicate_of_pending_target_is_accepted]
backlog fail <id> --reason "blocked"                         # 2 日先送りする（2 日後に pending へ戻る） [test: crates/backlog/tests/prose_claims.rs::claim_r_defers_the_re_run_by_2_days] [test: crates/backlog/tests/prose_claims.rs::claim_n_failed_requeued_after_defer]
backlog cancel <id> --reason "やらないと決めた"               # やらないと決めて捨てる（終端、再キューしない、`discard` closure として記録、テスト不要） [test: crates/backlog/tests/cancel_evidence.rs::cancel_pending_task_records_a_discard_closure_without_tty] [test: crates/backlog/tests/prose_claims.rs::claim_n_cancel_never_requeued]
backlog lock status                                          # project の排他ロックの保有者を確認 [test: crates/backlog/src/lock.rs::acquire_status_release_cycle]
backlog install                                              # SessionStart フックを settings.json にマージ [test: crates/backlog/tests/prose_claims.rs::claim_r_wire_remove_the_sessionstart_hook]
backlog uninstall                                            # 再び除去する [test: crates/backlog/tests/prose_claims.rs::claim_r_wire_remove_the_sessionstart_hook]
```

> 補足: `backlog list` の status 語彙は `pending` であり `open` ではない [test: crates/backlog/src/task.rs::status_warning_flags_unknown_and_suggests_valid]。

### テストが無ければ起票しない

- `add` は `--repro-test CMD` が HEAD で振る舞いとして失敗した時**だけ** task を `pending` で書く（再現記録つき） [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]。
- それ以外は非 0 終了で拒否し、store に何も書かない: `--repro-test` 無し [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_without_repro_writes_nothing_anywhere]、再現せず [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere]、判定不能 [test: crates/backlog/tests/repro_evidence.rs::undetermined_repro_files_no_row_and_writes_nothing]。
- `next` / `next --claim` は reproduced な repro の無い行を渡さない [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_skips_pending_row_without_repro] [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_claim_skips_pending_row_without_repro]。
- status が `unconfirmed` / `needs-ruling` の行を含む store は `list` / `next` が拒否し、その行を名指しする [test: crates/backlog/tests/no_unconfirmed.rs::list_fails_closed_on_an_unconfirmed_row_in_tasks_toml] [test: crates/backlog/tests/no_unconfirmed.rs::next_fails_closed_on_an_unconfirmed_row_in_tasks_toml] [test: crates/backlog/tests/no_needs_ruling.rs::list_fails_closed_on_a_needs_ruling_row_in_tasks_toml] [test: crates/backlog/tests/no_needs_ruling.rs::next_fails_closed_on_a_needs_ruling_row_in_tasks_toml] [test: crates/backlog/tests/status_type.rs::store_with_unconfirmed_row_fails_naming_id_and_status] [test: crates/backlog/tests/status_type.rs::store_with_needs_ruling_row_fails_naming_id_and_status]。
- `backlog audit-open [--json]` は open 行（pending / failed）を記録済み repro から `has-test` / `no-test` / `undetermined` に分類する [test: crates/backlog/tests/audit_open_once.rs::first_run_classifies_open_rows_and_excludes_terminal]。
- `audit-open` はテストを実行しない [test: crates/backlog/tests/audit_open_once.rs::no_test_is_executed]。
- 判定済みは `~/.backlog/audit-open/<project-slug>.json` に記録される [test: crates/backlog/tests/audit_open_once.rs::first_run_ledger_records_has_and_no_test_but_not_undetermined]。
- 判定済みの行は以後読み飛ばし、後から repro が付いても再確認しない [test: crates/backlog/tests/audit_open_once.rs::row_checked_as_no_test_stays_skipped_after_repro_added]。
- undetermined の行は次回また見る [test: crates/backlog/tests/audit_open_once.rs::second_run_skips_checked_and_reexamines_undetermined]。
- 記録が読めなければ失敗する [test: crates/backlog/tests/audit_open_once.rs::garbage_ledger_is_undetermined_human] [test: crates/backlog/tests/audit_open_once.rs::garbage_ledger_is_undetermined_json]。

`add` はタイトルと project から求めた **content hashkey** (`title` を trim → Unicode NFKC → 小文字化 →
連続空白の1個への圧縮 → 前後の記号除去 したものと project を FNV-1a 64bit で畳み込んだ 16 桁 hex)
で内容の重複を検出する [test: crates/backlog/src/task.rs::hashkey_is_16_hex_chars] [test: crates/backlog/src/task.rs::hashkey_normalizes_trivial_phrasing_differences] [test: crates/backlog/src/task.rs::hashkey_differs_across_projects] [test: crates/backlog/tests/prose_claims.rs::claim_n_hashkey_normalisation]。次のいずれかに該当する場合、`add` はエラーで拒否される [test: crates/backlog/src/store.rs::add_rejects_duplicate_pending_hashkey]。`done` の重複はブロックしない [test: crates/backlog/src/store.rs::add_does_not_block_on_done_duplicate]:

- 同じ hashkey を持つ既存タスクが `pending` または `failed` である [test: crates/backlog/src/store.rs::add_rejects_duplicate_pending_hashkey] [test: crates/backlog/src/store.rs::add_rejects_duplicate_failed_hashkey]。
- `condukt` が（plugin cache を先に、次に PATH を探して）見つかり [test: crates/backlog/tests/prose_claims.rs::claim_n_condukt_resolution_order]、`condukt state is-claimed --hashkey <h>` が exit 0 (= 他セッションの
  live なクレームが握っている) を返す [test: crates/backlog/tests/claim_check_contract.rs::exit0_claimed_true_is_refused_as_claimed]。exit 1 は stdout に `"claimed": false` が付いている場合に限り
  「クレームなし」と読む [test: crates/backlog/tests/claim_check_contract.rs::control_exit1_with_claimed_false_json_adds]。それ以外 — `condukt` が見つからない・起動失敗 [test: crates/backlog/tests/prose_claims.rs::claim_n_condukt_resolution_order] [test: crates/backlog/tests/claim_check_contract.rs::non_executable_condukt_is_refused]、0/1 以外の exit code (例: 3 = claim registry
  を読めない) [test: crates/backlog/src/store.rs::is_claimed_elsewhere_maps_exit_codes_three_valued] [test: crates/backlog/tests/prose_claims.rs::claim_n_claim_check_undetermined_refuses_add]、`claimed` フィールドを伴わない exit 0/1 [test: crates/backlog/tests/claim_check_contract.rs::exit1_garbage_stdout_is_refused] [test: crates/backlog/tests/claim_check_contract.rs::exit1_empty_stdout_registry_error_is_refused] [test: crates/backlog/tests/claim_check_contract.rs::exit0_with_claimed_false_contradiction_is_refused]、exit 1 に `"claimed": true` (exit と
  claimed の不一致) [test: crates/backlog/tests/claim_check_contract.rs::exit1_with_claimed_true_contradiction_is_refused]、シグナル終了 [test: crates/backlog/tests/claim_check_contract.rs::signal_killed_condukt_is_refused]、時間内に答えが返らない [test: crates/backlog/tests/claim_check_slow_helper.rs::never_answering_helper_is_refused_within_bounded_time] — は
  クレーム確認ができなかったことを意味し、`add` は理由を添えて**拒否**される (live な claim 済みタスクの
  重複を排除できないため)。「判定不能」を「クレームなし」とは読まない [test: crates/backlog/tests/backlog_s03_audit.rs::add_does_not_treat_an_undetermined_condukt_claim_check_as_not_claimed] [test: crates/backlog/tests/prose_claims.rs::claim_n_claim_check_undetermined_refuses_add]。

いずれの拒否も `backlog add --force` で意図的にバイパスできる [test: crates/backlog/src/store.rs::add_force_bypasses_duplicate_guard] [test: crates/backlog/tests/claim_check_contract.rs::force_overrides_an_undetermined_claim_check] [test: crates/backlog/tests/prose_claims.rs::claim_n_add_force_bypasses_claimed]。

`backlog list --json` の各要素には `hashkey` フィールドが含まれる (title + project から計算、保存はされない) [test: crates/backlog/tests/prose_claims.rs::claim_r_carries_a_hashkey_field]。

store は repo ごとに解決される (`<repo root>/.backlog/tasks.toml`) [test: crates/backlog/src/config.rs::project_in_a_repo_resolves_to_the_repo_local_store]。

- **read は project フィルタを掛けない:** どの checkout が書いたものであっても、ファイル内の
  全タスクがスコープ内 [test: crates/backlog/tests/project_scope.rs::a_task_recorded_under_another_checkouts_label_is_listed] [test: crates/backlog/tests/project_scope.rs::next_hands_out_a_task_recorded_under_another_checkouts_label]。

  - [record: 2026-08-20 rev: 89feaddb] 本 repo の store で pending/failed をラベル別に数えると macOS 265、WSL 70、`C:/…` 5。

  再測定コマンドを実行すること。

  ```sh
  python3 -c "import collections,tomllib; d=tomllib.load(open('.backlog/tasks.toml','rb')); \
    print(collections.Counter(t['project'] for t in d['task'] if t['status'] in ('pending','failed')).most_common())"
  ```

  よって `--project` は「どの store のことを言っているか」の **assertion** になる — この repo を
  指すなら何も変わらず [test: crates/backlog/tests/project_scope.rs::an_explicit_project_naming_this_repo_changes_nothing]、別の repo を指すのは（絞り込み結果ではなく）エラー [test: crates/backlog/tests/project_scope.rs::an_explicit_project_naming_a_different_repo_is_refused]。`--all` は受理される [test: crates/backlog/tests/integration.rs::list_all_flag_returns_every_project_in_the_resolved_store]。
- **repo root が上に無い cwd には store が無い:** `add` と `list` は理由を述べて拒否し、
  cross-project な `~/.backlog` へフォールバックしない [test: crates/backlog/tests/project_scope.rs::add_outside_any_repo_refuses_instead_of_writing_the_shared_store] [test: crates/backlog/tests/project_scope.rs::list_outside_any_repo_refuses_instead_of_reading_the_shared_store]。`next` も同様に拒否する [test: crates/backlog/tests/prose_claims.rs::claim_r_next_refuses_the_same_way]。共有 store を明示的に使いたい場合は
  `~/.backlog/config.toml` で `store_dir` を pin する（pinned store は複数 project を持ちうるので、
  そこでは `--project` は従来どおりのフィルタ） [test: crates/backlog/tests/project_scope.rs::a_pinned_store_dir_still_scopes_by_project] [test: crates/backlog/tests/project_scope.rs::a_pinned_store_dir_still_works_outside_any_repo]。

`project` フィールド自体は残る（誰が起票したかを示し、そのラベルが推測だった場合は `list` が
`[project unresolved: …]` と表示する） [test: crates/backlog/tests/project_identity.rs::add_under_an_undetermined_scope_marks_the_stored_task] [test: crates/backlog/tests/prose_claims.rs::claim_n_list_marks_project_unresolved] が、**何が見えるかを決めるものではなくなった** [test: crates/backlog/tests/project_scope.rs::a_task_recorded_under_another_checkouts_label_is_listed]。

store は兄弟関係の 2 ファイルからなる: `tasks.toml`（live キュー）と `tasks.done.toml`
（終端状態 — `done` / `cancelled` — の行すべて） [test: crates/backlog/tests/done_split.rs::done_moves_row_into_done_file_and_out_of_tasks_file] [test: crates/backlog/tests/done_split.rs::cancelled_row_is_migrated_to_done_file]。`.backlog/tasks.toml` の done ファイルは
`.backlog/tasks.done.toml` [test: crates/backlog/tests/prose_claims.rs::claim_r_dir_stem_done_toml]。

- **読み取りは和集合を見る:** reader は両ファイルの行を返す [test: crates/backlog/tests/done_split.rs::readers_see_rows_that_live_only_in_the_done_file]。
  `list --status done`・`done <id>`・`edit <id>` は、done ファイルにしか無い行も見る [test: crates/backlog/tests/done_split.rs::readers_see_rows_that_live_only_in_the_done_file]。
  `sync` も同様に見る [test: crates/backlog/tests/prose_claims.rs::claim_r_sync_the_duplicate_guard_and_the_near_du]。near-duplicate 走査は終端行を peer として出さない [test: crates/backlog/tests/prose_claims.rs::claim_r_sync_the_duplicate_guard_and_the_near_du] [test: crates/backlog/tests/prose_claims.rs::claim_n_near_dup_terminal_not_peer]。重複ガードの判定は従来どおり（`done` 行はどちらのファイルに
  あっても再 add をブロックしない） [test: crates/backlog/tests/done_split.rs::duplicate_guard_decision_is_unchanged_for_a_row_in_the_done_file]。
- **終端が勝ち、終端は最終:** 同じ id が両ファイルにあるとき（例: git merge が完了済みタスクを
  `tasks.toml` に `pending` として戻した）、返るのは終端側の行 1 件で、`next` はそれを渡さない [test: crates/backlog/tests/done_split.rs::done_in_done_file_beats_pending_in_tasks_file] [test: crates/backlog/tests/prose_claims.rs::claim_n_cancelled_terminal_wins_and_edit_refused]。
  `done`/`cancelled` のタスクへの `edit --status pending|failed` は非0終了で拒否され [test: crates/backlog/tests/done_split.rs::edit_cannot_revert_a_done_task_to_pending] [test: crates/backlog/tests/done_split_verifier.rs::edit_status_failed_cannot_reopen_a_done_task] [test: crates/backlog/tests/prose_claims.rs::claim_n_cancelled_terminal_wins_and_edit_refused]、`fail` も
  拒否される [test: crates/backlog/tests/done_split_verifier.rs::fail_cannot_reopen_a_done_task]。タスクは終端のまま [test: crates/backlog/tests/done_split_verifier.rs::fail_cannot_reopen_a_done_task]。既に done のタスクへの `done` は従来どおり冪等な成功 [test: crates/backlog/src/store.rs::mark_done_is_idempotent_on_repeat_call]。
- **壊れた done ファイルはエラーであり、空ではない:** `tasks.done.toml` が無いのは、まだ何も
  完了していないというだけ [test: crates/backlog/tests/done_split.rs::missing_done_file_is_fine]。存在するのに読めない/パースできない場合、`list` と `next` はそのファイル名を含むエラーで非0終了し、done 行を黙って欠いた一覧は出さない [test: crates/backlog/tests/done_split.rs::unparseable_done_file_makes_list_and_next_fail_closed]。`add`・`done`・`edit`・`fail`・`sync` も同様に非0終了し、done 行を上書きする書き込みは起こさない [test: crates/backlog/tests/prose_claims.rs::claim_r_add_done_edit_fail_and_sync_exit_non_zer]。SessionStart フックは空キューでは
  なく「store UNREADABLE」通知を注入する [test: crates/backlog/tests/prose_claims.rs::claim_r_store_unreadable]。
- **書き込みは振り分ける:** 保存のたびに終端行は done ファイルへ、それ以外は `tasks.toml` へ書く [test: crates/backlog/tests/done_split.rs::done_moves_row_into_done_file_and_out_of_tasks_file]。新たに終端になった行は末尾に追記されるので、完了の git diff は純粋な追記に
  なる [test: crates/backlog/tests/done_split.rs::done_file_is_appended_in_completion_order]。done ファイルに既にある行は位置を保ち、内容が変わったとき（`sync` の記録や notes 編集）だけ
  その場で書き換える [test: crates/backlog/tests/prose_claims.rs::claim_r_rows_already_in_the_done_file_keep_their]。done ファイルを先に書き、中身が変わらなければ書き直さない [test: crates/backlog/tests/prose_claims.rs::claim_r_done_file_is_written_first] [test: crates/backlog/tests/prose_claims.rs::claim_r_rows_already_in_the_done_file_keep_their]。`tasks.toml` はアトミックに置換
  される（fsync した一時ファイル + rename） [test: crates/backlog/src/store.rs::save_fsyncs_temp_before_rename_and_dir_after]。2 回の書き込みの間でクラッシュすると行が両ファイルに
  残るが、終端優先の和集合で解決される [test: crates/backlog/tests/prose_claims.rs::claim_r_a_crash_between_the_two_writes]。
- **古いバイナリの行は次の書き込みで移行される:** `tasks.toml` に残った
  `done`/`cancelled` 行は通常どおり一覧に出て、いずれかのコマンドが store を保存した
  次の機会に done ファイルへ移る [test: crates/backlog/tests/done_split.rs::legacy_done_row_in_tasks_file_is_listed_and_migrated_on_next_write]。

`.gitattributes` は `.backlog/tasks.toml` と `.backlog/tasks.done.toml` の両方に `merge=backlog` を指定する [test: crates/backlog/tests/prose_claims.rs::claim_r_gitattributes_marks_both]。
`backlog merge-driver <base> <ours> <theirs>`（git の
`%O %A %B`）が行単位ではなくタスク id 単位でマージする [test: crates/backlog/tests/merge_driver.rs::both_add_different_tasks_merges_clean_with_union] [test: crates/backlog/tests/merge_driver.rs::direct_clean_merge_writes_union_into_ours]:

- id の和集合をとる [test: crates/backlog/tests/merge_driver.rs::both_add_different_tasks_merges_clean_with_union]。片側で（base に対して）削除され他方で未変更のタスクは削除のまま [test: crates/backlog/tests/merge_driver.rs::deletion_on_one_side_stays_deleted]。片側だけで
  変更されたタスクはその側を採る [test: crates/backlog/tests/merge_driver.rs::edit_and_add_both_survive]。
- 同じ id が両側で異なって編集された場合は
  **conflict**: どちらも採らず非 0 終了する [test: crates/backlog/tests/merge_driver.rs::same_task_edited_differently_conflicts]。edit/delete、同じ新 id が異なる内容で追加された場合も同様 [test: crates/backlog/tests/prose_claims.rs::claim_r_the_same_holds_for_edit_delete]。`<ours>` は手を付けず、git がファイルを unmerged として
  人間に返す [test: crates/backlog/tests/prose_claims.rs::claim_r_ours_is_left_untouched]。
- パースできない入力は非 0 終了で `<ours>` は不変 [test: crates/backlog/tests/merge_driver.rs::unparseable_theirs_fails_and_leaves_ours_untouched]。
- 文字列 `id` を持たない行、1 入力内の id 重複、未知の
  top-level キーの場合も非 0 終了で `<ours>` は不変 [test: crates/backlog/tests/prose_claims.rs::claim_r_a_row_without_a_string_id]。
  clean な結果はアトミックに書かれる [test: crates/backlog/tests/prose_claims.rs::claim_r_a_clean_result_is_written_atomically]。

repo ごとに driver を設定するには次を実行すること。

```sh
backlog merge-driver --install   # repo 内で実行すること
```

これは repo の `merge.backlog.driver` を `backlog merge-driver %O %A %B` を実行するよう設定する [test: crates/backlog/tests/backlog_57a98a8f_68b8092f_regression.rs::control_install_configures_the_driver]。

store は意図的に checkout に追従する (`<repo root>/.backlog/tasks.toml`、linked worktree は
それ自体が root) [test: crates/backlog/src/config.rs::a_worktree_whose_dot_git_is_a_file_is_still_a_root] [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_linked_worktree_succeeds_and_lands_in_that_worktree]。

`next --claim` は、より **広い** ロックを先に取り、claim を store の場所ではなく project の
**identity** で鍵付けした machine-global な ledger に記録する [test: crates/backlog/tests/cross_checkout_claim.rs::two_checkouts_of_the_same_project_never_claim_the_same_task] [test: crates/backlog/tests/prose_claims.rs::claim_r_lock_order_never_inverted]:

- ロック順序 (逆順にしないこと): `~/.backlog/claims/<slug>.lock` (project 全体) → `<store>.lock` (この checkout) [test: crates/backlog/tests/prose_claims.rs::claim_r_lock_order_never_inverted]
- ledger: `~/.backlog/claims/<project-slug>.json`、linked worktree は main working tree に正規化されるので、同一
  プロジェクトの全 checkout が 1 つの ledger を共有する [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]。`<project-slug>` は `backlog lock` と同じ
  FNV-1a の project ハッシュ [test: crates/backlog/tests/prose_claims.rs::claim_r_project_slug_is_the_same_fnv_1a]。
- entry (**lease**) は holder の生存が 1h (`CLAIM_STALE_SECS`) 観測されないと除外をやめるので、
  死んだ claimant が全 checkout でタスクを永久にロックすることはない [test: crates/backlog/tests/claim_lease_untracked.rs::stale_lease_releases_the_task] [test: crates/backlog/tests/backlog_486e83ca.rs::holder_whose_last_heartbeat_aged_out_is_reclaimable] [test: crates/backlog/tests/prose_claims.rs::claim_n_lease_stale_threshold_1h]。記録自体は 7 日保持する [test: crates/backlog/tests/prose_claims.rs::claim_r_kept_for_7_days]。
- 「生存の観測」= claim 時刻と holder の最後の lease heartbeat のうち新しい方 [test: crates/backlog/tests/backlog_486e83ca.rs::live_holder_heartbeat_keeps_lease_past_stale_window] [test: crates/backlog/tests/backlog_486e83ca.rs::holder_whose_last_heartbeat_aged_out_is_reclaimable]。`next --claim` は
  claim したセッション (`CLAUDE_CODE_SESSION_ID`) を記録し、`backlog driver heartbeat --session-id S
  --project P` は P の ledger 上の S の **まだ生きている** lease も更新する [test: crates/backlog/tests/backlog_486e83ca.rs::live_holder_heartbeat_keeps_lease_past_stale_window]。したがって 1h 以内ごとに
  heartbeat する driver は、タスクに何時間かかっても claim を保つ (backlog 486e83ca) [test: crates/backlog/tests/backlog_486e83ca.rs::live_holder_heartbeat_keeps_lease_past_stale_window] [test: crates/backlog/tests/backlog_486e83ca.rs::another_sessions_heartbeat_does_not_refresh_the_lease]。期限切れの
  lease は蘇生しない [test: crates/backlog/tests/backlog_486e83ca.rs::heartbeat_does_not_revive_an_expired_lease]。セッションの無い lease (旧バイナリの entry やセッション外の claim) は従来どおり
  claim 時刻だけで期限切れになる [test: crates/backlog/tests/backlog_486e83ca.rs::sessionless_claim_ages_by_claimed_at] [test: crates/backlog/tests/backlog_486e83ca.rs::legacy_entry_without_liveness_fields_still_parses_and_ages]。lease を更新できない (例: ledger が読めない) 場合、heartbeat はその旨を出して非0終了する [test: crates/backlog/tests/backlog_486e83ca.rs::heartbeat_reports_failure_when_ledger_unreadable]。heartbeat フィールドが壊れていれば
  ledger 全体がパース不能となり、holder を死んだとはみなさず claim を拒否する [test: crates/backlog/tests/backlog_486e83ca.rs::unparseable_heartbeat_field_refuses_rather_than_reclaims] [test: crates/backlog/tests/verifier_486e83ca.rs::verifier_malformed_heartbeat_refused_by_every_reader]。

このパス上の判定不能はすべて **claim を拒否** し、理由を stderr に出して非0終了する [test: crates/backlog/tests/cross_checkout_claim.rs::an_unreadable_ledger_refuses_the_claim_instead_of_answering_empty]。exit 0 +
`no pending tasks` (= driver は「仕事がない」と読む) には決して倒さない [test: crates/backlog/tests/cross_checkout_claim.rs::an_unreadable_ledger_refuses_the_claim_instead_of_answering_empty] [test: crates/backlog/tests/project_identity.rs::next_claim_fails_closed_when_the_project_identity_cannot_be_determined]。対象は: ledger ロックを取れない [test: crates/backlog/src/claim_ledger.rs::a_held_ledger_lock_refuses_rather_than_claiming_unprotected] / ledger が読めない・パースできない [test: crates/backlog/src/claim_ledger.rs::unparseable_ledger_refuses_the_claim] / tasks-file ロックを
保持できない [test: crates/backlog/src/store.rs::next_claim_refuses_when_lock_not_held_instead_of_racing_unprotected] / project identity を解決できない [test: crates/backlog/tests/project_identity.rs::next_claim_fails_closed_when_the_project_identity_cannot_be_determined]。拒否は空のキューではない [test: crates/backlog/tests/claim_lease_untracked.rs::unreadable_ledger_refuses_list_and_next] [test: crates/backlog/tests/cross_checkout_claim.rs::an_unreadable_ledger_refuses_the_claim_instead_of_answering_empty]。ledger ディレクトリを作れない場合と ledger が書けない場合も同様に拒否される [test: crates/backlog/tests/prose_claims.rs::claim_r_the_ledger_directory_not_being_creatable]。

#### lease はトラックされた store に書かない — `claimed` は導出値 [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]

claim が書くのは **ledger だけ** である [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]。`next --claim` はトラックされた `.backlog/tasks.toml` を
変更しない [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]。SessionStart の requeue も claim を
理由に行を書き換えない [test: crates/backlog/tests/claim_lease_untracked.rs::session_start_does_not_dirty_the_store_for_claims]。したがって claim しても git worktree は汚れない [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees] [test: crates/backlog/tests/claim_lease_untracked.rs::session_start_does_not_dirty_the_store_for_claims]。`claimed` は **導出**
ステータスで、行ごとに保存されたステータスと生きた lease (あれば) から決まる [test: crates/backlog/tests/claim_lease_release.rs::claimant_fail_is_displayed_failed_but_still_excludes] [test: crates/backlog/tests/claim_lease_release.rs::reclaimed_old_failed_row_is_displayed_claimed]:

- 生きた lease を持つ `pending` 行は `claimed` と表示する [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]。
- 生きた lease を持つ `failed` 行は、lease がその行の `updated_at` より **厳密に後** に取られた
  場合 (古い failed タスクの再 claim) だけ `claimed` と表示する [test: crates/backlog/tests/claim_lease_release.rs::reclaimed_old_failed_row_is_displayed_claimed] [test: crates/backlog/tests/claim_lease_release.rs::failed_row_claimed_boundary_is_strict]。claim と同時かそれ以降に更新された
  行 (claimant が `fail` した) は `claimed` と表示せず、`--status failed` がそれを選ぶ [test: crates/backlog/tests/claim_lease_release.rs::claimant_fail_is_displayed_failed_but_still_excludes] [test: crates/backlog/tests/claim_lease_release.rs::failed_row_claimed_boundary_is_strict]。
- `done`/`cancelled` 行は決して `claimed` と表示しない [test: crates/backlog/tests/prose_claims.rs::claim_r_is_never_shown_claimed]。

この規則は表示だけのものである [test: crates/backlog/tests/claim_lease_release.rs::claimant_fail_is_displayed_failed_but_still_excludes]。`next` / `next --claim` からの除外は生きた lease だけで決まるので、
claimant が直前に fail したタスクは `claimed` と表示されないが、lease が期限切れになるまで配られない [test: crates/backlog/tests/claim_lease_release.rs::claimant_fail_is_displayed_failed_but_still_excludes]。

- `next --claim` は従来どおり `"status": "claimed"` でタスクを出力する [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]。
- 素の `next` は lease 中のタスクを返さない [test: crates/backlog/tests/claim_lease_untracked.rs::plain_next_skips_a_leased_task_in_every_checkout]。`list` (テキストと `--json`) は上記の導出ステータスを表示する [test: crates/backlog/tests/claim_lease_untracked.rs::claim_does_not_dirty_the_tracked_store_and_is_visible_across_worktrees]。
  status フィルタは導出後に適用される: `--status claimed` は
  `claimed` と表示される行を選ぶ [test: crates/backlog/tests/claim_lease_release.rs::reclaimed_old_failed_row_is_displayed_claimed]。`--status pending` は lease 中を含まない [test: crates/backlog/tests/prose_claims.rs::claim_r_status_pending_omits_leased]。
- 旧バイナリが `status = "claimed"` で保存した行は `pending` として読む [test: crates/backlog/tests/claim_lease_untracked.rs::legacy_tracked_claimed_row_without_lease_is_pending]。除外するのは生きた lease
  だけ [test: crates/backlog/tests/claim_lease_untracked.rs::legacy_tracked_claimed_row_without_lease_is_pending]。その行は、無関係なコマンドが次に store を保存したときに `pending` として書き直される [test: crates/backlog/tests/prose_claims.rs::claim_r_it_is_rewritten_as_pending]。
- `done`/`fail` は lease を解放しない [test: crates/backlog/tests/claim_lease_release.rs::claimant_fail_is_displayed_failed_but_still_excludes] [test: crates/backlog/tests/claim_lease_release.rs::claimant_done_is_not_redispatched_from_another_checkout]。`edit --status pending` も lease を解放しない [test: crates/backlog/tests/prose_claims.rs::claim_r_edit_status_pending_does_not_release]。終端でないタスクの lease は、holder の
  最後の生存観測から 1h で期限切れになるまで除外を続ける [test: crates/backlog/tests/claim_lease_untracked.rs::stale_lease_releases_the_task] [test: crates/backlog/tests/prose_claims.rs::claim_n_lease_stale_threshold_1h]。
- `list`・素の `next`・`next --claim` は同じ方法 (上記の project identity) で ledger を特定する [test: crates/backlog/tests/prose_claims.rs::claim_r_locate_the_ledger_the_same_way]。
  repo 外ではその identity は正規化した cwd [test: crates/backlog/tests/prose_claims.rs::claim_r_locate_the_ledger_the_same_way]。identity を解決できない (例: worktree の `.git` リンクが
  切れている) か、ledger が存在するのに読めない・パースできない場合、`list` と素の `next` は
  **拒否** する (非0終了、理由は stderr、stdout は空) [test: crates/backlog/tests/project_identity.rs::a_dangling_git_link_no_longer_hides_an_existing_task] [test: crates/backlog/tests/claim_lease_untracked.rs::unreadable_ledger_refuses_list_and_next]。claim 済みかもしれないタスクを pending と
  表示することはない [test: crates/backlog/tests/project_identity.rs::a_dangling_git_link_no_longer_hides_an_existing_task] [test: crates/backlog/tests/claim_lease_untracked.rs::unreadable_ledger_refuses_list_and_next] [test: crates/backlog/tests/prose_claims.rs::claim_n_plain_next_refuses_unresolved_identity]。
- store 乖離チェックは lease 中のタスクを「キューに残る作業」に数えない [test: crates/backlog/tests/prose_claims.rs::claim_r_store_divergence_check_does_not_count]。

git で追跡しているのは `bin/backlog` ランチャーだけである [test: crates/backlog/tests/prose_claims.rs::claim_r_only_the_bin_backlog_launcher_is_tracked]。ランチャーは配布先プラグインで隣に置かれたプラットフォーム別の `backlog-<os>-<arch>` バイナリを exec し [test: crates/backlog/tests/launcher_missing_binary.rs::control_launcher_still_execs_the_host_binary_when_it_is_present]、ホスト用のビルドが無ければ（空出力で exit 0 ではなく）非 0 で終了する [test: crates/backlog/tests/launcher_missing_binary.rs::every_subcommand_fails_loudly_rather_than_returning_an_empty_result]。
