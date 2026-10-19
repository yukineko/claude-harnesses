---
name: backlog
description: "既定で repo ごとの backlog（`<repo root>/.backlog/tasks.toml`）の queue+state を操作する backlog コマンド群（list / next / done / fail / driver / lock）の使い方 [test: crates/backlog/src/config.rs::project_in_a_repo_resolves_to_the_repo_local_store] [test: crates/backlog/tests/project_scope.rs::an_explicit_project_naming_a_different_repo_is_refused]。[record: 2026-06-27 rev: 6a33f004] ループ driver は /flow に統合された。"
argument-hint: [サブコマンド: list / next / done / fail / driver / lock]
allowed-tools: Bash(backlog:*), Bash(git:*), Read
---

> バックログのアイテムを順に実装したい場合は **`/flow`** を使ってください [test: crates/backlog/tests/prose_claims.rs::claim_s_flow]。

```bash
backlog list --status pending [--project <path>]   # ピックには使用禁止。キュー一覧（純粋な read） [test: crates/backlog/tests/prose_claims.rs::claim_s_read]
backlog next [--project <path>]                    # 次のアイテムを覗く（予約しない） [test: crates/backlog/tests/prose_claims.rs::claim_s_prohibition_form_not_flagged_b]
backlog next --claim [--project <path>]            # 次のアイテムを予約して取る（driver はこちら） [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]
backlog done <id> --test "<cmd>" --red-rev <rev>   # 完了（committed test が REV で RED・HEAD で GREEN を実測） [test: crates/backlog/tests/closure_evidence.rs::allowlisted_bash_script_with_red_rev_closes_and_records_closure] [test: crates/backlog/tests/closure_evidence.rs::red_rev_where_test_already_passes_is_refused] [test: crates/backlog/tests/closure_evidence.rs::failing_test_at_head_is_refused]
backlog done <id> --doc-only <commit>              # 完了（doc だけを触る祖先 commit が証拠） [test: crates/backlog/tests/doc_only_closure.rs::plain_docs_paths_are_accepted] [test: crates/backlog/tests/doc_only_closure.rs::mixed_doc_and_code_commit_is_refused]
backlog done <id> --duplicate-of <id>              # 完了（重複 [test: crates/backlog/tests/closure_evidence.rs::duplicate_of_pending_target_is_accepted]。相手は pending / claimed / done [test: crates/backlog/tests/prose_claims.rs::claim_s_pending_claimed_done]）
backlog add --repro-test "<cmd>" ...               # 再現した時だけ pending で起票し、それ以外は非 0 で拒否して store に何も書かない [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome] [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_without_repro_writes_nothing_anywhere] [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere]
backlog audit-open [--json]                        # open 行（pending/failed）に記録済み再現テストがあるかを、テストを実行せずに分類する [test: crates/backlog/tests/audit_open_once.rs::first_run_classifies_open_rows_and_excludes_terminal] [test: crates/backlog/tests/audit_open_once.rs::no_test_is_executed]
backlog audit-closures [--json]                    # 既存 close の証拠クラスを集計（read-only） [test: crates/backlog/tests/audit_closures.rs::classifies_each_done_row] [test: crates/backlog/tests/audit_closures.rs::audit_is_read_only_store_byte_identical_and_never_reopens]
backlog fail <id> --reason "<概要>"                # アイテムを失敗マーク（2 日後に再キューされる） [test: crates/backlog/tests/prose_claims.rs::claim_s_skill_20] [test: crates/backlog/tests/prose_claims.rs::claim_n_failed_requeued_after_defer]
backlog cancel <id> --reason "<理由>"              # やらないと決めて捨てる（終端 [test: crates/backlog/tests/backlog_d8d25af9.rs::fail_on_cancelled_task_is_refused]。`discard` closure として理由を記録 [test: crates/backlog/tests/cancel_evidence.rs::cancel_pending_task_records_a_discard_closure_without_tty]。証明できない推測はこれで捨てること。done と違い完了を主張しない [test: crates/backlog/tests/cancel_evidence.rs::cancel_pending_task_records_a_discard_closure_without_tty]）

# driver の存在通知（非排他 [test: crates/backlog/tests/concurrent_drivers.rs::lock_status_answers_liveness_for_zero_one_and_many_drivers]。何セッションでも同時に登録できる [test: crates/backlog/tests/concurrent_drivers.rs::lock_status_answers_liveness_for_zero_one_and_many_drivers]）
backlog driver register   --session-id <id> --project <path>
backlog driver heartbeat  --session-id <id> --project <path>
backlog driver unregister --session-id <id> --project <path>
backlog driver status [--project <path>]           # 登録中の driver 一覧（active / count / drivers） [test: crates/backlog/tests/prose_claims.rs::claim_s_driver_active_count_drivers]

# 排他ロックは「全セッションを締め出す」ときだけ取ること [test: crates/backlog/tests/prose_claims.rs::claim_n_force_takes_live_lock] [test: crates/backlog/src/liveness.rs::an_active_exclusive_lock_still_wins_and_keeps_its_shape]。キューを回すのに取る必要は無い [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]
backlog lock status [--project <path>]             # 排他ロック + driver 登録の合算ステータス（project 省略時は全 project 横断） [test: crates/backlog/tests/concurrent_drivers.rs::another_projects_driver_does_not_make_my_project_look_busy] [test: crates/backlog/src/liveness.rs::an_active_exclusive_lock_still_wins_and_keeps_its_shape]
backlog lock acquire --session-id <id> --project <path>
backlog lock release --project <path>
```

- `lock acquire` は**人が意図的に全セッションを締め出すときだけ**使うこと [test: crates/backlog/tests/prose_claims.rs::claim_n_force_takes_live_lock] [test: crates/backlog/src/liveness.rs::an_active_exclusive_lock_still_wins_and_keeps_its_shape]。
- キュー操作は必ず repo の中から実行すること [test: crates/backlog/tests/project_scope.rs::list_outside_any_repo_refuses_instead_of_reading_the_shared_store] [test: crates/backlog/tests/prose_claims.rs::claim_s_add_outside_repo_names_the_missing_repo] [test: crates/backlog/tests/prose_claims.rs::claim_s_repo_root_cwd_next]。
- `store_dir` を pin していなければ、repo root が上に無い cwd では `add` / `list` / `next` は拒否し、`~/.backlog` へフォールバックしない [test: crates/backlog/tests/prose_claims.rs::claim_s_add_outside_repo_names_the_missing_repo] [test: crates/backlog/tests/project_scope.rs::list_outside_any_repo_refuses_instead_of_reading_the_shared_store] [test: crates/backlog/tests/prose_claims.rs::claim_r_next_refuses_the_same_way] [test: crates/backlog/tests/project_scope.rs::a_pinned_store_dir_still_works_outside_any_repo]。
- **キューを回すのに排他ロックは要らない** [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。
- `next --claim` が選択と予約を同一クリティカルセクションで行うので、複数セッションが同じ project のキューを同時に消化しても同じ task を二重に掴むことはない [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。
- driver 登録も排他ロックも **project ごとにスコープ**される [test: crates/backlog/tests/prose_claims.rs::claim_s_driver_project]。
- `--project` は `status` 以外必須 [test: crates/backlog/tests/prose_claims.rs::claim_s_project_status]。
- `status` 省略時は全 project 横断スキャンになる [test: crates/backlog/tests/concurrent_drivers.rs::another_projects_driver_does_not_make_my_project_look_busy]。
- store は repo ごと（`<repo root>/.backlog/tasks.toml`）なので、`list` / `next` の `--project` は絞り込みではなく「どの store のことを言っているか」の assertion であり、別 repo を指すとエラーになる（絞り込まれた一覧は返らない） [test: crates/backlog/tests/project_scope.rs::an_explicit_project_naming_a_different_repo_is_refused]。
- この repo のタスクは全部スコープ内なので、通常は `--project` を **付けなくてよい** [test: crates/backlog/tests/project_scope.rs::a_task_recorded_under_another_checkouts_label_is_listed]。

## 起票の基準 — テストが無ければ起票しない

- 見つけたら**まず再現テスト（RED）を書いてコミット**し、`backlog add --repro-test "<cmd>"` で起票する [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]。
  `<cmd>` が HEAD で振る舞いとして失敗（＝再現）した時だけ task が `pending` で書かれる [test: crates/backlog/tests/repro_evidence.rs::reproduced_lands_pending_and_records_outcome]。
- それ以外は非 0 終了で拒否し、store に何も書かない: `--repro-test` 無し [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_without_repro_writes_nothing_anywhere]、再現せず [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere]、判定不能 [test: crates/backlog/tests/repro_evidence.rs::undetermined_repro_files_no_row_and_writes_nothing]。
- `next` / `next --claim` は reproduced な repro の無い行を渡さない [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_skips_pending_row_without_repro] [test: crates/backlog/tests/no_confirm_no_unproven.rs::next_claim_skips_pending_row_without_repro]。
- status が `unconfirmed` / `needs-ruling` の行を含む store は `list` / `next` が拒否し、その行を名指しする [test: crates/backlog/tests/no_unconfirmed.rs::list_fails_closed_on_an_unconfirmed_row_in_tasks_toml] [test: crates/backlog/tests/no_unconfirmed.rs::next_fails_closed_on_an_unconfirmed_row_in_tasks_toml] [test: crates/backlog/tests/no_needs_ruling.rs::list_fails_closed_on_a_needs_ruling_row_in_tasks_toml] [test: crates/backlog/tests/no_needs_ruling.rs::next_fails_closed_on_a_needs_ruling_row_in_tasks_toml] [test: crates/backlog/tests/status_type.rs::store_with_unconfirmed_row_fails_naming_id_and_status] [test: crates/backlog/tests/status_type.rs::store_with_needs_ruling_row_fails_naming_id_and_status]。
- `backlog audit-open [--json]` は pending / failed の各行が **記録済みの** 再現テストを持つかを
  `has-test` / `no-test` / `undetermined` で返す [test: crates/backlog/tests/audit_open_once.rs::first_run_classifies_open_rows_and_excludes_terminal]。
- `audit-open` はテストを実行しない [test: crates/backlog/tests/audit_open_once.rs::no_test_is_executed]。
- 判定済みは `~/.backlog/audit-open/<project-slug>.json` に記録され、以後読み飛ばす [test: crates/backlog/tests/audit_open_once.rs::first_run_ledger_records_has_and_no_test_but_not_undetermined] [test: crates/backlog/tests/audit_open_once.rs::row_checked_as_no_test_stays_skipped_after_repro_added]。
- undetermined の行は次回また見る [test: crates/backlog/tests/audit_open_once.rs::second_run_skips_checked_and_reexamines_undetermined]。
- `cancel` は空でない `--reason` が要る [test: crates/backlog/tests/cancel_evidence.rs::cancel_with_empty_or_whitespace_reason_is_refused]。
- `edit --status done|cancelled` は拒否される [test: crates/backlog/tests/closure_evidence.rs::edit_status_done_is_refused] [test: crates/backlog/tests/closure_evidence.rs::edit_status_cancelled_is_refused]。

## コマンドを用途で使い分けること

- キューを確認したいときは `backlog list --status pending` を使うこと [test: crates/backlog/tests/project_scope.rs::a_task_recorded_under_another_checkouts_label_is_listed]。
- 次のアイテムだけ確認したいときは `backlog next` を使うこと [test: crates/backlog/tests/project_scope.rs::next_hands_out_a_task_recorded_under_another_checkouts_label]。着手するなら `next --claim` を使うこと [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。
- 手動で完了したいときは `backlog done <id> --test "<cmd>" --red-rev <rev>` を使うこと [test: crates/backlog/tests/closure_evidence.rs::allowlisted_bash_script_with_red_rev_closes_and_records_closure]。素の `done` は拒否される [test: crates/backlog/tests/closure_evidence.rs::bare_done_is_refused_and_status_unchanged]。失敗マークしたいときは `backlog fail <id>` を使うこと [test: crates/backlog/tests/prose_claims.rs::claim_s_skill_20]。
- やらないと決めて閉じたいときは `backlog cancel <id> --reason "<理由>"` を使うこと（テスト無しで `discard` として記録される [test: crates/backlog/tests/cancel_evidence.rs::cancel_pending_task_records_a_discard_closure_without_tty]）。
- キューを自動で全件消化したいときは **`/flow` を使う**こと [test: crates/backlog/tests/prose_claims.rs::claim_s_flow_1]。

## 失敗モードの対処に従うこと

- **`lock status` が `kind: undetermined` を返しても「driver 不在」と読んではならない**（安全側に倒して見送ること） [test: crates/backlog/tests/concurrent_drivers.rs::an_unreadable_registry_does_not_report_none]。
- `backlog` が起動できないときは README の plugin 導入手順を案内すること [test: crates/backlog/tests/launcher_missing_binary.rs::every_subcommand_fails_loudly_rather_than_returning_an_empty_result]。
- **他セッションが driver 登録中** → 競合ではない [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks] [test: crates/backlog/tests/prose_claims.rs::claim_n_registered_driver_does_not_block_claim]。並走してよい（task 単位の排他は `next --claim` が保証する） [test: crates/backlog/tests/concurrent_drivers.rs::two_concurrent_driver_processes_get_disjoint_tasks]。
- **排他ロック競合**（`lock status` の `kind` が `exclusive-lock`） [test: crates/backlog/src/liveness.rs::an_active_exclusive_lock_still_wins_and_keeps_its_shape] → 保有セッションを確認し、必要なら `--force` で奪取すること [test: crates/backlog/tests/lock_heartbeat_ttl.rs::ttl_exceeded_lock_with_unconfirmable_progress_is_not_reaped_but_force_wins] [test: crates/backlog/tests/prose_claims.rs::claim_n_force_takes_live_lock]。
- `kind: undetermined` は backlog が registry を読めなかった状態である [test: crates/backlog/tests/prose_claims.rs::claim_s_kind_undetermined_backlog_regi]。
