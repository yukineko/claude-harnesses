**read-only** であり、HOME・作業ディレクトリ・TMPDIR の下に何も書き込まず、他プラグインのストアを集約するだけである [test: crates/harness-status/tests/prose_claims.rs::claim_read_only]。

汎用の `SessionStart` ダッシュボード hook は登録**しない** [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_json_single_session_start]。

**Claude Code 向けの統合 HOTL ステータスダッシュボード** [test: crates/harness-status/tests/prose_claims.rs::claim_full_dashboard]。

- **予算 (budgetguard)**: 今日の支出額とセッション数 [test: crates/harness-status/tests/prose_claims.rs::claim_budget_from_ledger]。`~/.budgetguard/state/ledger.json` から読む [test: crates/harness-status/tests/prose_claims.rs::claim_budget_from_ledger]。
- **直近セッション (gauge)**: 直近 N 件のセッションの turn 数・トークン数・USD コスト [test: crates/harness-status/tests/prose_claims.rs::claim_sessions_from_gauge_limit]。`~/.gauge/store/sessions/` から読む [test: crates/harness-status/tests/prose_claims.rs::claim_sessions_from_gauge_limit]。
- **進捗ファイル (taskprog)**: カレントディレクトリの `.claude/progress.md` のプレビュー [test: crates/harness-status/tests/prose_claims.rs::claim_progress_preview]。
- **Stop-hook レイテンシ**: `~/.harness/state/hook-latency.jsonl` からのセッション単位の合計 [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_latency_per_session]。
- **UserPromptSubmit 注入サイズ**: `~/.harness/state/inject-metrics.jsonl` からのターン単位の合計 [test: crates/harness-status/tests/prose_claims.rs::claim_inject_per_turn]。
- **hook binary の健全性**: `~/.claude/settings.json` に登録された hook のうち、binary がディスク上に無いもの [test: crates/harness-status/tests/integration.rs::session_start_warns_via_additional_context_when_binary_missing] [test: crates/harness-status/src/hooks_health.rs::read_at_reports_missing_binaries_and_tolerates_absent_file]。
- **PATH shadowing**: plugin cache の同名 binary を PATH 上で覆い隠している PATH 上のコピー [test: crates/harness-status/src/path_shadow.rs::earlier_dir_with_same_name_is_flagged_as_shadowing] [test: crates/harness-status/tests/prose_claims.rs::claim_path_shadow_cli]。

`SessionStart` hook (`hooks/hooks.json`) を 1 本だけ登録し、hook-binary health check (`harness-status session-start`) を実行する [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_json_single_session_start]。
`~/.claude/settings.json` に登録された hook の binary がディスク上に無い場合、`additionalContext` を注入する [test: crates/harness-status/tests/integration.rs::session_start_warns_via_additional_context_when_binary_missing]。
plugin cache の binary が PATH 上のコピーに shadow されている場合、`additionalContext` を注入する [test: crates/harness-status/tests/prose_claims.rs::claim_session_start_shadow_injects]。
PATH-shadow スキャン自体が完了しなかった場合 (判定不能として報告)、`additionalContext` を注入する [test: crates/harness-status/tests/prose_claims.rs::claim_session_start_undetermined_injects]。
それ以外のときは無出力・無注入でサイレントである [test: crates/harness-status/tests/integration.rs::session_start_is_silent_when_all_hook_binaries_present]。

```
╔══════════════════════════════════════════════╗
║         harness-status  (2026-06-23)         ║
╚══════════════════════════════════════════════╝

── Budget (budgetguard) ──────────────────────────
  Today spend:  $1.8420  (3 session(s))

── Recent sessions (gauge) ───────────────────────
  Session          Project              Turns       Tokens  Cost USD
  ----------------------------------------------------------------------
  3c8d91a2         harness                 12        35000    0.1850

── Progress file (taskprog) ──────────────────────
  cwd: /repo
  /repo/.claude/progress.md
  │ - specforge ⑤ worktree merge
```

```
/plugin install harness-status@yukineko
```

導入後、任意のセッションで `/status` を実行する [test: crates/harness-status/tests/prose_claims.rs::claim_status_command]。

```sh
cargo install --path .
harness-status
```

```sh
harness-status                       # フルダッシュボード [test: crates/harness-status/tests/prose_claims.rs::claim_full_dashboard]
harness-status budget                # 今日の支出のみ [test: crates/harness-status/tests/prose_claims.rs::claim_budget_from_ledger]
harness-status sessions --sessions 10  # 直近セッション (件数 N を指定) [test: crates/harness-status/tests/prose_claims.rs::claim_sessions_from_gauge_limit]
harness-status progress              # 進捗ファイルのみ [test: crates/harness-status/tests/prose_claims.rs::claim_progress_preview]
harness-status hooks                 # Stop-hook レイテンシ集約 (予算モニタ) [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_latency_per_session]
harness-status inject                # UserPromptSubmit 注入サイズ集約 (予算モニタ) [test: crates/harness-status/tests/prose_claims.rs::claim_inject_per_turn]
harness-status plugins               # 全プラグインを活性化スコープで分類 [test: crates/harness-status/tests/prose_claims.rs::claim_plugins_cli]
harness-status hooks-health          # settings.json の hook のうち binary 欠落のもの [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_health_cli]
harness-status path-shadow           # plugin cache 版を覆う PATH 上の binary [test: crates/harness-status/src/path_shadow.rs::earlier_dir_with_same_name_is_flagged_as_shadowing] [test: crates/harness-status/tests/prose_claims.rs::claim_path_shadow_cli]
harness-status --json                # 機械可読出力 (フルダッシュボード・budget・sessions・progress・hooks・inject・plugins・hooks-health・path-shadow で有効) [test: crates/harness-status/tests/prose_claims.rs::claim_json_any_subcommand]
```

`plugins` サブコマンドは全プラグインを **always-on** / **event-scoped** / **manual** に分類する [test: crates/harness-status/src/plugins.rs::classify_always_on_on_high_frequency_event] [test: crates/harness-status/src/plugins.rs::classify_event_scoped_on_only_low_frequency_events] [test: crates/harness-status/src/plugins.rs::classify_manual_when_no_hooks] [test: crates/harness-status/tests/prose_claims.rs::claim_plugins_cli]。
チェックアウトから実行すること。リポジトリのレイアウトから分類する [test: crates/harness-status/src/plugins.rs::find_repo_root_walks_up_to_marketplace] [test: crates/harness-status/tests/prose_claims.rs::claim_plugins_cli]。

- ゼロ・空のパネル・沈黙した hook health check を、問題なしの証明として読まないこと。
- プラグインのストアが存在しないとき、そのセクションは "not installed?" と表示し、コマンドは exit 0 で終わる [test: crates/harness-status/tests/prose_claims.rs::claim_not_installed]。
- `sessions` と `path-shadow` は読み取り元を読めないとき exit 1 で終わる: gauge のストアが読めない場合 `sessions` は stderr に `sessions: unknown — …` を出し (`--json` では stdout に `{"status":"unknown",…}`)、PATH スキャンが完了しない場合 `path-shadow` は stderr に `harness-status: PATH-shadow scan could not be completed: …` を出す [test: crates/harness-status/tests/prose_claims.rs::claim_unknown_exit1]。フルダッシュボードはどちらも `unknown — …` と表示する [test: crates/harness-status/tests/prose_claims.rs::claim_unknown_exit1]。
- `harness-status hooks` は、Stop-hook の合計実行時間が `HARNESS_HOOK_LATENCY_BUDGET_MS` (デフォルト `30000`、数値として読めない値はデフォルトに戻る) を超えるセッションを警告する [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_budget_default_and_env]。
- `harness-status inject` は、UserPromptSubmit 注入の合計が `HARNESS_INJECT_BUDGET_CHARS` (デフォルト `20000`) を超えるターンを警告する [test: crates/harness-status/tests/prose_claims.rs::claim_inject_budget_default_and_env]。
- 日付はシステムクロックから導出する [test: crates/harness-status/tests/prose_claims.rs::claim_date_system_clock]。
- テスト時は `HARNESS_DATE=YYYY-MM-DD` で日付を上書きできる [test: crates/harness-status/tests/integration.rs::budget_subcommand_reports_pinned_date]。

ライセンス: MIT [test: crates/harness-status/tests/prose_claims.rs::claim_license_mit]
