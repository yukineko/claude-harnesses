メトリクス収集が失敗してもセッションを妨げてはならない。

backlog / code / commit から読み直せるものは書かないこと。

`<!-- si:numeric:* -->` / `<!-- si:cost:* -->` で囲まれたブロックは機械所有のため編集しない [test: crates/session-insights/tests/prose_claims.rs::claim_machine_block_edits_overwritten]。

`merge` は機械所有マーカーの無い既存ノートを書き換えない（既存ノートへ遡及追加しない） [test: crates/session-insights/tests/prose_claims.rs::claim_merge_leaves_markerless_note]。

[record: 2026-06-29 rev: ccc8fc2d] Devin の Session Insights に着想を得て、ローカル・API キー不要のフックとして組み直したもの。

- **PostToolUse フック**（`record`）が各ツール呼び出しを記録する（ツールごとの回数、触れた個別ファイル） [test: crates/session-insights/tests/prose_claims.rs::claim_posttooluse_records_tool_and_files]。
- **Stop フック**（`stop`）がターンを 1 つ数える [test: crates/session-insights/tests/prose_claims.rs::claim_stop_counts_turn]。`obsidian_log = true` なら Obsidian の日付つきセッションノート（`sessions/`）も毎ターン書き直し、`record = true` なら `/record` ノートの数値ブロックも更新する [test: crates/session-insights/tests/prose_claims.rs::claim_stop_writes_session_note_when_obsidian_log] [test: crates/session-insights/tests/prose_claims.rs::claim_stop_refreshes_record_when_record_true]。
- **SessionEnd フック**（`sessionend`）は `record = true` のときだけ `/record` ノートを書き出す／更新する [test: crates/session-insights/tests/prose_claims.rs::claim_sessionend_writes_record_only_when_record_true]。
- フックはブロックせず exit 0 だが、空・パース不能なペイロードを黙って捨てない。各フックはそのイベントを記録しなかったことを stderr に出す [test: crates/session-insights/tests/repro_hooks_garbage_payload_silent.rs::garbage_payload_is_reported_on_stderr] [test: crates/session-insights/tests/integration.rs::sessionend_hook_garbage_payload_is_reported_on_stderr]。`stop` は加えて `systemMessage` を出す [test: crates/session-insights/tests/integration.rs::stop_hook_garbage_payload_emits_system_message]。

- **size**: 記録したツールイベントの総数による XS / S / M / L / XL（閾値は設定可能） [test: crates/session-insights/tests/prose_claims.rs::claim_size_thresholds_default_boundaries] [test: crates/session-insights/tests/prose_claims.rs::claim_size_thresholds_configurable]。
- **category**: `coding`（Edit/Write/MultiEdit/NotebookEdit）、`ops`（Bash）、`research`（Read/Grep/Glob/WebFetch/WebSearch）のうち、分類対象イベントの半分以上を占めるグループ [test: crates/session-insights/tests/prose_claims.rs::claim_category_rule]。どれも半分に届かなければ `mixed`、分類対象が 0 件なら `empty` [test: crates/session-insights/tests/prose_claims.rs::claim_category_rule]。
- セッションごとのターン数・ツールイベント数・ファイル数・上位ツール [test: crates/session-insights/tests/prose_claims.rs::claim_report_shows_counts]。

同梱の `hooks/hooks.json` が 3 つのフックを自動で配線する（PostToolUse の matcher は `Bash|Edit|MultiEdit|Write|Read|Grep|Glob|WebFetch|WebSearch|NotebookEdit`） [test: crates/session-insights/tests/prose_claims.rs::claim_hooks_json_three_hooks]。PostToolUse が `record` を、Stop が `stop` を、SessionEnd が `sessionend` を呼ぶ [test: crates/session-insights/tests/prose_claims.rs::claim_hooks_json_three_hooks]。閾値の調整や Obsidian ログの有効化は `session-insights.toml` を置いて行う [test: crates/session-insights/tests/prose_claims.rs::claim_project_toml_tunes]。

```sh
session-insights report          # 最新セッション [test: crates/session-insights/tests/prose_claims.rs::claim_report_latest_default]
session-insights report --session <id-prefix>
session-insights report --all    # 記録済みセッションを 1 行ずつ [test: crates/session-insights/tests/prose_claims.rs::claim_report_all_one_line_each]
session-insights report --context # context-governor の台帳健全性も併記する [test: crates/session-insights/tests/integration.rs::report_context_joins_synthetic_ledger]
```

```
session a1b2c3d4  [2026-06-20T18:00:00+09:00]
  project: playbook
  size: L   category: coding
  turns: 12   tool events: 47   files: 9
  top tools: Edit 18, Bash 12, Read 9, Write 5, Grep 3
```

セッションストアが読めない場合、`report` は「セッション無し」とは表示せず、stderr に `unknown — session store could not be read: …` を出して exit 1 で終わる [test: crates/session-insights/tests/prose_claims.rs::claim_report_unreadable_store_exits_1]。個々のセッションファイルが読めない・パースできない場合も黙って飛ばさない。`report --all` は読めたセッションを一覧したうえで、読めなかったファイルを stderr に `incomplete — ` に続けて列挙し exit 1 する [test: crates/session-insights/tests/repro_report_all_skips_corrupt_session.rs::report_all_does_not_present_partial_list_as_complete]。`report`（最新）と `report --session` は、読めないファイルが答えでありうるとき `unknown` として exit 1 する [test: crates/session-insights/src/metrics.rs::tests::latest_with_a_corrupt_file_is_undetermined] [test: crates/session-insights/src/metrics.rs::tests::find_is_undetermined_only_for_a_matching_corrupt_file]。

記録ノートの数値ブロック（`## 数値サマリ` / `## コスト`）は同梱バイナリが充填し、`## 完了サマリ` / `## 認知の変化 / 修正された理解` / `## つまずき / 学び` / `## 振り返り / 確立した方針` / `## 自己批判 / 確信度` / `## 注意点 / 落とし穴` / `## 残課題` / `## 要追跡 / あとで確認` / `## 関連` の散文節は `<!-- fill: … -->` プレースホルダのまま残る [test: crates/session-insights/src/record.rs::skeleton_has_all_sections_and_placeholders] [test: crates/session-insights/tests/prose_claims.rs::claim_skeleton_prose_sections_placeholders]。

backlog / code / commit に書けないもの — 推論の経路、修正された理解（初期モデル → どう間違い → 何が直ったか）、確信度の較正 — を書くこと。ID と `path:line` はポインタに畳むこと [test: crates/session-insights/tests/prose_claims.rs::claim_record_md_fold_pointers]。`## 認知の変化 / 修正された理解` は `## 完了サマリ` の直下、`## 自己批判 / 確信度` は `## 振り返り / 確立した方針` の直下に置かれる [test: crates/session-insights/tests/prose_claims.rs::claim_cognition_sections_adjacent]。

`record = true` のときは Stop（毎ターン）と SessionEnd でもノートの数値ブロックが自動更新される [test: crates/session-insights/tests/prose_claims.rs::claim_stop_refreshes_record_when_record_true] [test: crates/session-insights/tests/prose_claims.rs::claim_sessionend_writes_record_only_when_record_true]。`record-now` は `record = false` でも書き込む [test: crates/session-insights/tests/prose_claims.rs::claim_record_note_path_and_vault_required]。内部では `session-insights record-now` が現在セッション（`$CLAUDE_CODE_SESSION_ID`）を解決し、`## コスト` / `## 数値サマリ` ブロックを更新してノートの絶対パスを出力する [test: crates/session-insights/tests/prose_claims.rs::claim_record_now_prints_path]。vault ディレクトリが存在しない場合はノートを書かず、その旨を stderr に出して終了する（vault は自動生成しない） [test: crates/session-insights/tests/prose_claims.rs::claim_record_note_path_and_vault_required]。

セッションをまたぐキューは、独立した [`backlog`](../backlog) クレート（リポジトリ単位の `<repo root>/.backlog/tasks.toml`）が担う [test: crates/backlog/src/config.rs::project_in_a_repo_resolves_to_the_repo_local_store]。pending タスクを注入するのは `backlog session-start` コマンドだ [test: crates/backlog/tests/backlog_016c2562_mirrors.rs::session_start_pending_listing_omits_wontfix_but_lists_normal_task]。session-insights は独自の backlog ストアを持たず、旧 `session-insights backlog` サブコマンドと `<vault>/backlog.md` / `backlog.json` は削除済み [test: crates/session-insights/tests/prose_claims.rs::claim_no_backlog_subcommand]。

```sh
backlog add --title "darwin-x86_64 を x86 Mac で再ビルド" --project harness --repro-test "<cmd>"
backlog list --project harness --status pending
backlog done <id> --test "cargo test -p x" --red-rev <rev>   # 証跡つきで閉じる [test: crates/backlog/tests/closure_evidence.rs::allowlisted_bash_script_with_red_rev_closes_and_records_closure]
```

ストアへの書き込み（`add` / `done` など）は primary の作業ツリーでは拒否される [test: crates/backlog/tests/store_worktree_boundary.rs::add_from_primary_checkout_is_refused_and_writes_nothing] [test: crates/backlog/tests/store_worktree_boundary.rs::done_from_primary_checkout_is_refused_and_file_unchanged]。
linked worktree から実行すること。
素の `backlog done <id>` は拒否される [test: crates/backlog/tests/closure_evidence.rs::bare_done_is_refused_and_status_unchanged]。
閉じるには証跡（`--test … --red-rev …`、`--doc-only <commit>`、`--duplicate-of <id>` のいずれか）が要る [test: crates/backlog/tests/closure_evidence.rs::allowlisted_bash_script_with_red_rev_closes_and_records_closure] [test: crates/backlog/tests/doc_only_closure.rs::plain_docs_paths_are_accepted] [test: crates/backlog/tests/closure_evidence.rs::duplicate_of_pending_target_is_accepted]。
再現する `--repro-test` の無い `add` は拒否され、store に何も書かない [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_without_repro_writes_nothing_anywhere] [test: crates/backlog/tests/no_confirm_no_unproven.rs::add_with_non_reproducing_repro_writes_nothing_anywhere]。
詳細は backlog の README を参照すること。

`obsidian_log = true` にして `obsidian_vault` を自分の vault に向けると、Stop（毎ターン）ごとにセッションが `<vault>/sessions/<date>-<id>.md` にフロントマター（`type: session`、size、category、turns…）つきで書き出される——ただし vault ディレクトリが既に存在する場合のみ（vault は決して作らない） [test: crates/session-insights/tests/prose_claims.rs::claim_stop_writes_session_note_when_obsidian_log] [test: crates/session-insights/tests/prose_claims.rs::claim_stop_session_note_never_creates_vault_and_needs_opt_in]。

```sh
cargo install --path .
session-insights install      # PostToolUse + Stop + SessionEnd フックをマージ [test: crates/session-insights/tests/prose_claims.rs::claim_install_merges_three_hooks]
session-insights report --all
session-insights status        # 解決済み設定を表示 [test: crates/session-insights/tests/prose_claims.rs::claim_status_resolved_config]
session-insights init          # state ディレクトリを作成 [test: crates/session-insights/tests/prose_claims.rs::claim_init_creates_state_dir]
session-insights uninstall
```

`install` / `uninstall` は冪等で、`settings.json` をバックアップし、他プラグインのフックグループを保持する [test: crates/session-insights/tests/prose_claims.rs::claim_install_idempotent_backup_foreign]。

`./session-insights.toml`（プロジェクト）または `~/.session-insights/config.toml`（グローバル）を置く [test: crates/session-insights/tests/prose_claims.rs::claim_config_lookup_order]。最初に見つかったものが優先される [test: crates/session-insights/tests/prose_claims.rs::claim_config_lookup_order]。選ばれたファイルが読めない・パースできない、または `size_thresholds` が 4 要素でない場合は黙って既定値に置き換えず、読み込みのたびに stderr へ出し、`status` は config を INVALID と表示して exit 1 する [test: crates/session-insights/tests/repro_config_invalid_silently_ignored.rs::invalid_config_is_not_silently_defaulted]。

- `enabled` — フック全体の有効/無効; 既定値 `true` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_enabled_false_is_noop]
- `size_thresholds` — `[S, M, L, XL]` のツールイベント数の下限; 既定値 `[5, 15, 40, 100]` [test: crates/session-insights/tests/prose_claims.rs::claim_size_thresholds_default_boundaries] [test: crates/session-insights/tests/prose_claims.rs::claim_size_thresholds_configurable]
- `ignore_tools` — メトリクスから除外するツール; 既定値 `["TodoWrite"]` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_todowrite_ignored]
- `obsidian_log` — 毎 Stop で日付つき `sessions/` ノートを書く; 既定値 `false` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_stop_session_note_never_creates_vault_and_needs_opt_in]
- `obsidian_vault` — vault ルート（`sessions/` と `records/` の下に書く）; 既定値 `~/Documents/vault/yukineko` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values]
- `record` — Stop と SessionEnd で `/record` ノートを書く/更新する; 既定値 `false` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_sessionend_writes_record_only_when_record_true]
- `record_dir` — record ノートを置く vault 内サブディレクトリ; 既定値 `records` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_record_note_path_and_vault_required]
- `state_dir` — ロールアップ state ディレクトリ; 既定値 `~/.session-insights/state` [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_values] [test: crates/session-insights/tests/prose_claims.rs::claim_init_creates_state_dir]
- `price_overrides` — `## コスト` ブロックのモデル別単価上書き; 既定値 （組み込み単価） [test: crates/session-insights/tests/prose_claims.rs::claim_config_defaults_price_overrides_change_cost]

`SESSION_INSIGHTS_DISABLE` に `0` 以外の空でない値を設定すると無効化できる [test: crates/session-insights/tests/prose_claims.rs::claim_kill_switch_env]。

```sh
make bins
cargo test
```

コミットされているのは `bin/session-insights` ランチャーだけで、プラットフォーム別の `bin/session-insights-<os>-<arch>` は git 管理外（.gitignore） [test: crates/session-insights/tests/prose_claims.rs::claim_bins_gitignored]。ホスト用ビルドが無い場合、ランチャーは「実行した」ふりをしない: `record` は stderr に 1 行、`stop` は `systemMessage` を出し、`sessionend` は exit 1、CLI 動詞は exit 127 で終わる [test: crates/session-insights/tests/prose_claims.rs::claim_launcher_missing_binary]。
