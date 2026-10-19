変更系コマンド（begin / run / end / heartbeat / reap / reassign / pause / resume）は、操作が行われなかったとき非 0 で終了する（lease ロック競合・lease root 解決不能・キー未保持〈end, reassign〉・`end --status` の未知値・condukt 失敗〈pause, resume〉）。`heartbeat` / `reap` の exit 0 は呼び出しが走ったことしか意味しないので、出力の `refreshed` / `reaped` の件数で実際の変化を確認すること。

begin が exit 1 + skip JSON を返したら、決して先に進んではいけない。

- いずれかのセッションが `overwatch begin --key <k>` を呼べば、原子的にそのキーを claim する [test: crates/overwatch/tests/lease_concurrency.rs::concurrent_begin_same_key_exactly_one_wins]。別セッションが既に live lease を保持していれば、リクエストは reject される [test: crates/overwatch/tests/lease_concurrency.rs::concurrent_begin_same_key_exactly_one_wins]。
- 新しいセッションが、既に claim されているキーで begin を試みれば、overwatch はそれを report し（exit 1 + skip JSON）、呼び出し元は重複実行をスキップする [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_held_key_exit1_skip_json]。
- **SessionStart** フックで、プロジェクト全体の進捗ビュー（`overwatch status`）を注入し、新セッションは、開始時点で「どのセッションが live lease を保持しているか・何が pending か（PDO 仮説・backlog）・どの condukt run があるか・compass gap」を把握できる [test: crates/overwatch/tests/prose_claims.rs::claim_rd_sessionstart_injects_status]。
- 1800 秒（30 分）を超えてハートビート更新が無い lease は stale になる [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_ttl_1800_stale]。
- stale lease は**すべての** lease 変更操作（`begin` / `run` / `end` / `heartbeat` / `reassign` / `reap`）で削除され、`status` / `sessions` / `lease` では無視される [test: crates/overwatch/tests/prose_claims.rs::claim_rd_stale_dropped_by_every_mutation]。したがって dead な保持者が `begin` を阻むことはない [test: crates/overwatch/tests/prose_claims.rs::claim_rd_stale_dropped_by_every_mutation]。
- `overwatch reap` はその掃除だけを単独で行う [test: crates/overwatch/tests/prose_claims.rs::claim_rd_reap_drops_stale_prints_count]。

2 つのフック（SessionStart + Stop、どちらも `overwatch status` を hook timeout 5 秒で実行）がある [test: crates/overwatch/tests/prose_claims.rs::claim_rd_hooks_json_two_status_hooks]。キャッシュされた status は、`aggregate::STATUS_CACHE_TTL_SECS` を過ぎたとき、cache 構築と同じ秒以降に `leases.json` が書かれたとき、またはその mtime を観測できないときに破棄される（`aggregate::cache_is_fresh`） [test: crates/overwatch/src/aggregate.rs::test_cache_is_fresh_within_ttl] [test: crates/overwatch/src/aggregate.rs::test_cache_is_fresh_expired_past_ttl] [test: crates/overwatch/src/aggregate.rs::test_cache_is_fresh_rule2_same_second_ledger_write_is_not_fresh] [test: crates/overwatch/src/aggregate.rs::test_cache_is_fresh_rule3_undetermined_ledger_mtime_is_not_fresh]。

`overwatch status` の各セクションは、空のとき `(none)` を出す [test: crates/overwatch/src/render.rs::undetermined_is_scoped_to_its_own_source] [test: crates/overwatch/src/render.rs::test_format_roster_empty] [test: crates/overwatch/src/render.rs::test_format_hypotheses_none]。
`(none)` は**実際に観測できた source にしか出さない** [test: crates/overwatch/src/render.rs::undetermined_sessions_render_unknown_not_none] [test: crates/overwatch/src/render.rs::undetermined_backlog_renders_unknown_not_none] [test: crates/overwatch/src/render.rs::undetermined_hypotheses_render_unknown_not_none] [test: crates/overwatch/src/render.rs::undetermined_runs_render_unknown_not_none] [test: crates/overwatch/src/render.rs::undetermined_compass_gap_renders_unknown_not_none]。
読めなかった source — 壊れた/読めない lease 台帳、不在のヘルパーバイナリ、デコードできない JSON — は `(unknown: <理由>)` を出す [test: crates/overwatch/src/render.rs::undetermined_render_carries_the_reason] [test: crates/overwatch/tests/prose_claims.rs::claim_rd_status_warning_on_unreadable_source] [test: crates/overwatch/src/aggregate.rs::test_parse_backlog_invalid_json_is_error_not_zero] [test: crates/overwatch/src/aggregate.rs::test_parse_condukt_runs_unparseable_progress_is_error_not_zero]。
該当 source を列挙した WARNING を stderr に出す [test: crates/overwatch/tests/prose_claims.rs::claim_rd_status_warning_on_unreadable_source]。

`status --json` にも機械可読な `undetermined` 配列（`[{source, reason}]`）がある [test: crates/overwatch/tests/prose_claims.rs::claim_rd_status_json_undetermined_array]。全 source を観測できたときはキーごと省略される [test: crates/overwatch/tests/prose_claims.rs::claim_rd_status_json_undetermined_array]。

```
/plugin install overwatch@yukineko
```

```sh
cargo install --path .
```

`overwatch install` というサブコマンドは存在しない [test: crates/overwatch/tests/prose_claims.rs::claim_rd_no_install_subcommand]。

```sh
overwatch begin --key <k> --title <t> [--session <sid>] [--scope <csv>] [--done-criteria <text>]   # キー <k> を排他的に claim する、別セッションが live で保持中、または lease ロックが競合したら exit 1 + skip JSON [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_held_key_exit1_skip_json]
overwatch lease --session <sid> [--json]     # そのセッションが保持中の live lease（PDO anchor）を表示し、無ければ exit 1（無音） [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_cmd_prints_live_lease_exit1_silent]
overwatch run --key <k> [--note <text>]      # 保持中のリースにハートビート＋イベントを記録 [test: crates/overwatch/tests/prose_claims.rs::claim_rd_run_records_heartbeat_event]
overwatch status [--json]                    # プロジェクト全体の進捗ビュー: Sessions / PDO hypotheses / Backlog / Condukt runs / Compass gap [test: crates/overwatch/tests/prose_claims.rs::claim_rd_sessionstart_injects_status]
overwatch sessions [--json]                  # Sessions ロスターのみ: live（stale でない）lease を保持するセッション [test: crates/overwatch/tests/prose_claims.rs::claim_rd_sessions_roster_live_only]
overwatch pause --run <id>                   # `condukt state pause --run <id>` に委譲し、control イベントを記録 [test: crates/overwatch/tests/prose_claims.rs::claim_rd_pause_resume_delegate_condukt]
overwatch resume --run <id>                  # `condukt state resume --run <id>` に委譲し、control イベントを記録 [test: crates/overwatch/tests/prose_claims.rs::claim_rd_pause_resume_delegate_condukt]
overwatch reassign --key <k> --to <sid>      # <k> の lease を「解放」し reassign to=<sid> イベントを記録（<sid> 側に lease は作らない） [test: crates/overwatch/tests/prose_claims.rs::claim_rd_reassign_releases_lease_no_new_lease]
overwatch end --key <k> --status <done|abandoned>   # キーの lease を解放し、終端 status を記録（それ以外の status は非 0 で拒否され、lease は保持されたまま） [test: crates/overwatch/tests/prose_claims.rs::claim_rd_end_releases_lease_records_status]
overwatch reap                               # stale lease（TTL 内にハートビート無し）を削除し {"reaped": N} を出力 [test: crates/overwatch/tests/prose_claims.rs::claim_rd_reap_drops_stale_prints_count]
overwatch heartbeat --key <k>                # キーの heartbeat_at を更新し {"refreshed": 0|1} を出力 [test: crates/overwatch/tests/prose_claims.rs::claim_rd_heartbeat_refreshes_prints_count]
overwatch --protects                         # このゲートが何を・何から・どんな根拠で守るかを表示 [test: crates/overwatch/tests/protects_flag.rs::protects_prints_three_labelled_nonempty_lines_and_exits_zero]
```

`begin`/`run` のセッション ID は `--session`、無ければ `$CLAUDE_CODE_SESSION_ID`、それも無ければ `pid-<pid>` で決まる [test: crates/overwatch/tests/prose_claims.rs::claim_rd_session_id_resolution]。

run ID は `$OVERWATCH_RUN_ID`、無ければタイムスタンプ由来の `run-…` になる [test: crates/overwatch/tests/prose_claims.rs::claim_rd_run_id_resolution]。

`end` は lease ロックが競合したとき、非 0 で終了し、lease は解放されない [test: crates/overwatch/tests/prose_claims.rs::claim_sk_end_nonzero_when_not_released]。

`run` / `heartbeat` / `reap` / `reassign` / `end` は、lease ロックが競合したとき、または lease root を解決できないとき非 0 で終了する（何も書かない。`end` は加えて lease を解放しない）。未保持キーの `reassign`、condukt が失敗した `pause` / `resume` も非 0 で終了する [test: crates/overwatch/tests/repro_mutators_unresolvable_root_exit_zero.rs::heartbeat_on_unresolvable_root_must_be_nonzero] [test: crates/overwatch/tests/repro_run_lock_contended_exit_zero.rs::run_under_real_lock_contention_must_be_nonzero] [test: crates/overwatch/tests/repro_reassign_unheld_key_exit_zero.rs::reassign_of_unheld_key_must_be_nonzero] [test: crates/overwatch/tests/repro_pause_resume_condukt_failure_exit_zero.rs::pause_must_be_nonzero_when_condukt_fails] [test: crates/overwatch/tests/repro_end_unknown_status_accepted.rs::end_with_unknown_status_is_rejected]。

`end` はキーが未保持のとき、非 0（nothing was released）で終了する [test: crates/overwatch/tests/prose_claims.rs::claim_rd_end_unheld_key_nonzero]。

バイナリ自身は `pause` / `resume` / `reassign` / `end` / `reap` の前に承認を求め**ない** [test: crates/overwatch/tests/prose_claims.rs::claim_rd_binary_no_approval_prompt]。

バイナリを直接呼べば即座に実行される [test: crates/overwatch/tests/prose_claims.rs::claim_rd_skill_confirms_side_effects_with_askuserquestion]。

begin が非 0 を返したら、先に進んではいけない。

```bash
overwatch begin --key "hypothesis-v2.3"
```

- **exit 0**: Lease 取得成功（同じセッションが既に保持していた場合は refresh、`claimed_at` は維持） [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_exit0_refresh_keeps_claimed_at]。
- **exit 1** + JSON `{ "skipped": true, "holder": { "session_id", "run_id", "heartbeat_at" } }`: このキーについて、別の live セッションが lease を保持中 [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_held_key_exit1_skip_json]。**決して先に進んではいけない**。
- **exit 1** + JSON `{ "skipped": true, "reason": "lock_contended" }`: lease レジストリのロックが取れず、キーが空いているかを確認していない [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_held_key_exit1_skip_json]。後で再試行すること。**先に進んではいけない**。

exit code を確認し、非 0 なら処理をスキップすること。

`leases.json` が壊れているときも exit **1** になり、エラーは stderr に出て、stdout に skip JSON は出ない [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_internal_error_exit1_no_json]。

store root を解決できないときも exit **1** になり、stderr は root が undetermined であることを示す [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_internal_error_exit1_no_json]。

lease は 2 つの任意 PDO-anchor フィールド、`scope` と `done_criteria` を持てる [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_scope_done_criteria_fields]。

いずれも `#[serde(default)]` なので、これらが無かった頃に書かれた既存の `leases.json` もそのまま読める（後方互換） [test: crates/overwatch/tests/prose_claims.rs::claim_rd_legacy_leases_json_loads]：

- `scope: Vec<String>` — このセッションが担当するファイル/glob [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_scope_done_criteria_fields]。
- `done_criteria: Option<String>` — このセッションの「完了」の定義 [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_scope_done_criteria_fields]。

`overwatch begin` は、これらを設定する 2 つの任意フラグ `--scope` と `--done-criteria` を持つ [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_scope_done_criteria_fields]：

```bash
overwatch begin --key "hypothesis-v2.3" --title "cache the router table" \
  --scope "crates/fugu-router/src/**,crates/condukt/src/schedule.rs" \
  --done-criteria "fugu-router のテスト全 green・clippy 警告なし"
```

`begin` 成功時（exit 0）、advisory な JSON サマリを stdout に出力する [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_prints_advisory_summary]：

```json
{ "scope_overlap": [ { "key": "...", "title": "...", "scope": ["..."] } ],
  "scope_undeclared": [ { "key": "...", "title": "..." } ],
  "scope_declared": true,
  "possible_duplicate": [ { "key": "...", "title": "...", "similarity": 0.72 } ] }
```

- `scope_overlap` は、この lease と scope が重なる別 key の live lease を列挙する [test: crates/overwatch/tests/prose_claims.rs::claim_rd_scope_overlap_lists_live_overlapping_peers]。
- `scope_undeclared` は、この lease か相手のどちらかが scope を宣言していないため重なりを**判定できなかった** live lease を列挙する [test: crates/overwatch/tests/prose_claims.rs::claim_rd_scope_undeclared_lists_undetermined_peers]。
- 未宣言は「重なりなし」ではない — どちらのリストにも出ない peer は、scope を宣言したうえで重なっていないものだけである [test: crates/overwatch/tests/prose_claims.rs::claim_rd_scope_undeclared_lists_undetermined_peers] [test: crates/overwatch/tests/prose_claims.rs::claim_rd_scope_overlap_lists_live_overlapping_peers]。
- `scope_declared` は、この lease 自身が scope を宣言していないとき `false` になり、その場合はすべての peer が `scope_undeclared` に入る（backlog 56eb3f9a） [test: crates/overwatch/tests/prose_claims.rs::claim_rd_scope_declared_false_lists_all_peers_undeclared]。
- `possible_duplicate` は、title が語彙的に類似する（Jaccard 類似度、既定の閾値 0.6、環境変数 `OVERWATCH_DUP_THRESHOLD` で上書き可）別の live lease を列挙する [test: crates/overwatch/tests/prose_claims.rs::claim_rd_possible_duplicate_threshold]。
- いずれのリストも既定は空配列で、**exit code は決して変えない** [test: crates/overwatch/tests/prose_claims.rs::claim_rd_begin_lists_never_change_exit_code]。

```bash
overwatch lease --session <sid> [--json]
```

`<sid>` が保持中の live lease（PDO anchor）を——複数保持していれば最も直近に claim したものを——表示する [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_cmd_prints_live_lease_exit1_silent]。無ければ exit 1 で無音終了する [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_cmd_prints_live_lease_exit1_silent]。

各 lease は `heartbeat_at`（および `claimed_at`）タイムスタンプを持つ [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_has_heartbeat_and_claimed_at]。staleness の判定は**固定**のハートビート TTL **1800 秒（30 分）**（`store::LEASE_TTL_SECS`）に対して行う [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_ttl_1800_stale]。TTL はコンパイル時定数であり、設定変更はできない [test: crates/overwatch/tests/prose_claims.rs::claim_rd_ttl_not_configurable]。

- `now - heartbeat_at > 1800s` になると lease は stale になる [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_ttl_1800_stale]。次の lease 変更操作（`begin` / `run` / `end` / `heartbeat` / `reassign` / `reap` のいずれか）が `leases.json` から削除し、別セッションによるそのキーの `begin` は成功する [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_ttl_1800_stale] [test: crates/overwatch/tests/prose_claims.rs::claim_rd_stale_dropped_by_every_mutation]。
- `overwatch reap` はその掃除だけを行う [test: crates/overwatch/tests/prose_claims.rs::claim_rd_reap_drops_stale_prints_count]。

状態はリポジトリごとに `~/.overwatch/<project-key>/overwatch/`（`leases.json` + `events.jsonl`）に保存される [test: crates/overwatch/tests/prose_claims.rs::claim_rd_state_dir_layout]。キーは MAIN worktree root から導出する [test: crates/overwatch/tests/worktree_partition.rs::lease_begun_in_linked_worktree_is_visible_from_main_checkout]。

main checkout・全 linked `git worktree`・linked worktree の subdirectory は同一の store を共有する（main checkout から `overwatch status` を実行すると linked worktree 側のセッションも見える） [test: crates/overwatch/tests/worktree_partition.rs::lease_begun_in_linked_worktree_is_visible_from_main_checkout] [test: crates/overwatch/tests/worktree_partition.rs::lease_begun_in_main_checkout_is_visible_from_linked_worktree] [test: crates/overwatch/tests/worktree_partition.rs::subdirectory_of_linked_worktree_resolves_to_same_store]。

main worktree root を解決できない場合（`.git` ファイルが解釈できない）、`begin`・`lease`・`end` は worktree 単位の store へ fallback せずエラーになる [test: crates/overwatch/tests/prose_claims.rs::claim_rd_main_worktree_unresolvable_fails]。

git repo 外の cwd は cwd 自身がキーになる [test: crates/overwatch/tests/worktree_partition.rs::control_cwd_outside_any_git_repo_still_works]。

- `<base>`: `~/.overwatch`（`$HOME/.overwatch`） [test: crates/overwatch/tests/prose_claims.rs::claim_sk_store_path]。
- `<project-key>`: リポジトリの MAIN worktree root の `<basename>-<正規化パスの fnv1a32>`（linked worktree 間で共有） [test: crates/overwatch/tests/prose_claims.rs::claim_rd_state_dir_layout] [test: crates/overwatch/tests/worktree_partition.rs::lease_begun_in_linked_worktree_is_visible_from_main_checkout]。
- lease は `end` されるか、`reassign` で解放されるか、後続の lease 変更操作で stale として削除されるまで `leases.json` に残る [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_persists_until_end_reassign_stale] [test: crates/overwatch/tests/prose_claims.rs::claim_rd_stale_dropped_by_every_mutation]。overwatch は lease を Claude Code プロセスに紐づけておらず、セッション終了時に lease を消す overwatch フックも無い [test: crates/overwatch/tests/prose_claims.rs::claim_rd_lease_persists_until_end_reassign_stale]。

`overwatch` は project-wide の実行/lease レジストリに加えて、gate 違反イベントを **正規化した signature** 付きで記録し、同一 signature が複数タスク/セッションにまたがって時間窓内に N 件以上再発したら **systemic issue** としてエスカレーションする [test: crates/overwatch/tests/prose_claims.rs::claim_rd_record_violation_and_systemic_escalation] [test: crates/overwatch/tests/prose_claims.rs::claim_rd_recurrence_default_policy_and_overrides]。

```sh
overwatch record-violation --source <gate> --discriminator <d> --task <key> [--symbol <sym>] [--session <sid>] [--detail <text>]
overwatch violations [--json] [--threshold N] [--window-secs S]     # 全 signature の再発状況 [test: crates/overwatch/tests/prose_claims.rs::claim_rd_violations_cli_prints_all_signatures]
overwatch escalations [--json] [--threshold N] [--window-secs S]    # systemic と判定された signature のみ [test: crates/overwatch/tests/prose_claims.rs::claim_rd_escalations_cli_only_systemic]
```

- `--source` は `blastguard | propguard | specguard | mutategate | donegate | reviewgate | tdd | budgetguard | autoflow | ctxrot` を受け付け、未知の token は source として parse されない [test: crates/overwatch/src/violation.rs::violation_source_parse_round_trips_all_variants] [test: crates/overwatch/src/violation.rs::violation_source_parse_rejects_unknown_token]。
- **signature 正規化**（`violation::normalize_signature`）では、同種の失敗（例: blastguard の同一 rule、mutategate の同一 mutation operator、specguard の同一 drift kind + symbol）は、大文字小文字・空白の違いを問わず同じ signature になる [test: crates/overwatch/src/violation.rs::normalize_signature_is_case_and_whitespace_insensitive] [test: crates/overwatch/src/violation.rs::normalize_signature_mutategate_uses_operator] [test: crates/overwatch/src/violation.rs::normalize_signature_specguard_combines_kind_and_symbol]。
- **再発検知**（`violation::detect_recurrence`）は時刻を引数で受け取る [test: crates/overwatch/tests/violation.rs::recurrence_window_excludes_stale_events_deterministically]。
- 既定ポリシーは 24 時間窓・3 件以上（`RecurrencePolicy::default`） [test: crates/overwatch/tests/prose_claims.rs::claim_rd_recurrence_default_policy_and_overrides]。CLI では `--threshold` / `--window-secs` で呼び出しごとに上書きできる [test: crates/overwatch/tests/prose_claims.rs::claim_rd_recurrence_default_policy_and_overrides]。
- **systemic 判定**は「閾値以上」かつ「複数タスク or 複数セッションにまたがる」の両方を要求する [test: crates/overwatch/src/violation.rs::is_systemic_requires_threshold_and_multiple_tasks] [test: crates/overwatch/src/violation.rs::is_systemic_false_below_threshold_even_with_multiple_tasks] [test: crates/overwatch/src/violation.rs::is_systemic_true_when_multiple_sessions_same_task_key_reused]。単一タスクが同じゲートに何度もリトライして失敗するのは systemic ではなく local retry loop として区別する [test: crates/overwatch/src/violation.rs::is_systemic_requires_threshold_and_multiple_tasks] [test: crates/overwatch/src/violation.rs::is_systemic_false_below_threshold_even_with_multiple_tasks] [test: crates/overwatch/src/violation.rs::is_systemic_true_when_multiple_sessions_same_task_key_reused]。
- 保存先は既存の project-wide registry と同じ `<base>/<project-key>/overwatch/` 配下の `violations.jsonl`（events.jsonl とは別ストリーム、append-only） [test: crates/overwatch/tests/prose_claims.rs::claim_rd_violations_jsonl_separate_stream]。

`overwatch --help` が全サブコマンドを説明付きで列挙する [test: crates/overwatch/tests/prose_claims.rs::claim_rd_help_lists_every_subcommand]。

- canary rollout: `canary-plan`（純粋な stage 分割）、`canary-gate`（PROCEED/ROLLBACK の JSON、rollback 推奨時は **exit 3**）、`canary-rollback-plan`（純粋な復元計画）、`record-rollback`（`rollbacks.jsonl` に rollback イベントを記録） [test: crates/overwatch/tests/prose_claims.rs::claim_rd_canary_commands_behaviour]
- review 窓口: `review-queue [--json] [--since] [--limit]`（5 source — systemic 違反・canary rollback・AI review finding・condukt escalation・未解決の merge conflict — を severity 順の 1 リストに統合 [test: crates/overwatch/src/review_queue.rs::stale_high_severity_outranks_fresh_low_severity] [test: crates/overwatch/tests/review_queue_five_sources.rs::review_queue_merges_exactly_five_sources]）、`record-finding`、`record-disposition`、`review-metrics`、`compact-findings`、`resolve-merge-conflict`、`reconcile-fixed [--since-ref\|--range\|--last-n] [--dry-run]`（既定は直近 50 commit）、`auto-approved [--since] [--sample 5] [--seed 0]` [test: crates/overwatch/tests/prose_claims.rs::claim_rd_review_surface_defaults]
- Continuous-Audit: `audit-round record` / `audit-round close`、`audit-metrics [--window 3]` [test: crates/overwatch/tests/prose_claims.rs::claim_rd_continuous_audit_commands]
- テレメトリ: `undetermined-metrics [--window-days N]`、`gate-outcomes`（donegate / tdd / reviewgate の発火結果を `~/.<gate>/state/log.jsonl` から集計） [test: crates/overwatch/tests/prose_claims.rs::claim_rd_telemetry_commands]

読み取り側の exit code 規約: ledger/journal が（不在・空ではなく）**読めない**とき、`review-queue` [test: crates/overwatch/tests/undetermined_e2e.rs::undecodable_violation_ledger_is_not_rendered_as_no_systemic_violations]・`review-metrics` [test: crates/overwatch/tests/undetermined_e2e.rs::review_metrics_with_an_unreadable_disposition_ledger_reports_null_not_zero]・`reconcile-fixed` [test: crates/overwatch/tests/undetermined_e2e.rs::reconcile_fixed_with_an_unreadable_disposition_ledger_writes_nothing]・`gate-outcomes` [test: crates/overwatch/tests/gate_outcomes_cli.rs::non_utf8_log_is_undetermined_with_null_counts_and_exit_3] は **exit 3** し、`auto-approved` は非 0 で終わる [test: crates/overwatch/tests/auto_approved_cli.rs::unreadable_journal_does_not_exit_clean]。git 履歴も同じ扱いの入力で、`git log` が読めない（repository でない・git が無い・未知の ref）とき `reconcile-fixed` は exit 3・`commits_scanned: null`・`undetermined_sources` に `git log` を出し [test: crates/overwatch/tests/reconcile_fixed.rs::reconcile_fixed_undetermined_when_not_a_git_repo]、`review-metrics` は stale-undisposed 件数を 0 ではなく `null` と報告する [test: crates/overwatch/tests/repro_review_metrics_stale_count_not_a_git_repo.rs::review_metrics_stale_count_outside_git_repo_is_not_zero]。

MIT [test: crates/overwatch/tests/prose_claims.rs::claim_rd_license_mit]
