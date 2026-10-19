`budgetguard gate` は Claude Code の **Stop** フックに配線され、各 Stop で次を行う [test: crates/budgetguard/tests/prose_claims.rs::claim_hooks_json_stop_gate]:

1. このセッションのモデルごとのトークン使用量を得る [test: crates/budgetguard/src/gate.rs::fresh_record_matches_transcript_parse_cost] [test: crates/budgetguard/src/gate.rs::missing_record_falls_back_to_transcript_parse]。
   gauge が永続化したセッションレコードが
   トランスクリプト以降に更新されていればそれを使い、そうでなければトランスクリプト (JSONL) を
   読み直して集計する (sub-agent の支出も含む) [test: crates/budgetguard/src/gate.rs::fresh_record_matches_transcript_parse_cost] [test: crates/budgetguard/src/gate.rs::stale_record_falls_back_to_transcript_parse] [test: crates/budgetguard/tests/prose_claims.rs::claim_cost_source_freshness] [test: crates/budgetguard/src/gate.rs::unreadable_subagents_dir_is_undetermined_and_blocks_when_a_limit_is_armed]。
2. 組み込みの料金テーブルで USD コストを見積もる [test: crates/budgetguard/src/gate.rs::fresh_record_matches_transcript_parse_cost]。
3. ローカルの **日次台帳** (`~/.budgetguard/state/ledger.json`) を、このセッションの最新コストで
   更新する (Stop ごとの合計ではない) [test: crates/budgetguard/tests/prose_claims.rs::claim_ledger_latest_cost]。
   書き込みは台帳ロックの下で行い、他セッションがロックを保持している間は台帳を書かず、日次合計を未計測とする [test: crates/budgetguard/tests/prose_claims.rs::claim_ledger_lock_serializes]。
   日付キーは**ローカル**の日付である [test: crates/budgetguard/tests/prose_claims.rs::claim_day_key_local_midnight]。
4. セッション合計とその日の累積合計を、設定した上限と照合する [test: crates/budgetguard/src/gate.rs::daily_block_triggers_independently_of_session] [test: crates/budgetguard/tests/prose_claims.rs::claim_day_cumulative] [test: crates/budgetguard/src/gate.rs::block_is_inclusive_at_threshold_and_beats_warn]。
5. 判定を出す [test: crates/budgetguard/tests/prose_claims.rs::claim_verdict_outputs]:
   - **上限内** → exit 0、Stop はそのまま進む [test: crates/budgetguard/tests/prose_claims.rs::claim_verdict_outputs]。
   - **warn 閾値超過** → `{"additionalContext":"…"}` を出す (助言のみ、ブロックしない) [test: crates/budgetguard/tests/prose_claims.rs::claim_verdict_outputs]。
   - **block 閾値超過** → `{"decision":"block","reason":"…"}` を出し、ターンが終わる前に
     エージェントへ保存・コミットを促す [test: crates/budgetguard/tests/prose_claims.rs::claim_verdict_outputs]。
   - 閾値は「上限以上」で判定し、block が warn に優先する [test: crates/budgetguard/src/gate.rs::warn_is_inclusive_at_threshold] [test: crates/budgetguard/src/gate.rs::block_is_inclusive_at_threshold_and_beats_warn]。

あわせて gauge のセッションレコードから **キャッシュヒットの健全性** を stderr に報告する [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_promoted_warning] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_below_floor_not_promoted]。
`input + cache_read` が `[cache].min_tokens` (既定 200000) に達した後、ヒット率が `[cache].min_rate`
(既定 0.5、範囲外の値は既定値に戻る) を下回ると、より強い通知が無い場合に限り助言の
`additionalContext` に昇格する [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_promoted_warning] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_below_floor_not_promoted] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_out_of_range_rate_uses_default] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_min_tokens_default_boundary] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_min_rate_default_and_inrange] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_never_blocks_louder_wins]。
ブロックはしない [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_promoted_warning]。

ブロックした Stop は overwatch の violation event も記録する [test: crates/budgetguard/tests/prose_claims.rs::claim_block_records_violation]。

許可されるもの (exit 0、判定なし) [test: crates/budgetguard/tests/prose_claims.rs::claim_reentry_allowed] [test: crates/budgetguard/tests/prose_claims.rs::claim_disable_env]:

- 設定ファイルが無い、または `[session]` / `[daily]` の上限がすべて `0` なら、すべての Stop を許可する
  (上限 0 は「上限なし」) [test: crates/budgetguard/src/config.rs::an_absent_config_is_known_default_not_undetermined] [test: crates/budgetguard/src/gate.rs::zero_threshold_means_disabled]。
- `stop_hook_active` 付きの再入 (前の Stop フックが既にブロックした) — 予算超過のセッションは
  一度ブロックされた後、停止を許される [test: crates/budgetguard/tests/prose_claims.rs::claim_reentry_allowed]。
- `BUDGETGUARD_DISABLE=1` ならフックは no-op になる (設定ファイルが parse できない場合も同じ) [test: crates/budgetguard/tests/prose_claims.rs::claim_disable_env]。
- 設定で `enabled = false` ならフックは no-op になる [test: crates/budgetguard/tests/prose_claims.rs::claim_example_toml]。
- [record: 2026-10-16 rev: 1a84e601] session と daily の `block_usd` を設定した状態で、ビルドしたバイナリは、存在しない `transcript_path`、ディレクトリである `transcript_path`、JSON でない stdin、空の `session_id` のいずれでも exit 0・判定なし (stderr も無出力) で終了した — これらは報告されない fail-open の経路であり、意図した許可規則ではない。

*判定不能* は予算内として**扱わない** [test: crates/budgetguard/src/gate.rs::an_undetermined_config_blocks_despite_zero_default_limits] [test: crates/budgetguard/src/gate.rs::a_contended_ledger_lock_makes_the_day_undetermined_and_blocks]:

- 存在するが読めない／parse できない設定ファイル → 上限が何であれ**ブロック** (`config-undetermined`) [test: crates/budgetguard/src/gate.rs::an_undetermined_config_blocks_despite_zero_default_limits] [test: crates/budgetguard/src/config.rs::an_unparseable_config_is_undetermined_not_an_unconfigured_gate] [test: crates/budgetguard/src/config.rs::an_unreadable_config_is_undetermined_not_an_unconfigured_gate] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::control_unparseable_config_blocks_naming_the_file]。
- セッション支出を測定できない (sub-agent のトランスクリプトが読めない、料金不明のモデルがある等) → block 上限が 1 つでも
  あれば**ブロック**、無く warn 上限があれば**警告** [test: crates/budgetguard/src/gate.rs::unreadable_subagents_dir_is_undetermined_and_blocks_when_a_limit_is_armed] [test: crates/budgetguard/tests/prose_claims.rs::claim_session_undetermined_warn_only] [test: crates/budgetguard/tests/prose_claims.rs::claim_session_undetermined_daily_arms]。
- 日次合計を測定できない (台帳ロックを他セッションが保持、または `ledger.json` が破損 — 破損ファイルは
  そのまま保存される) → *日次*上限に従って block/warn [test: crates/budgetguard/src/gate.rs::a_contended_ledger_lock_makes_the_day_undetermined_and_blocks] [test: crates/budgetguard/src/gate.rs::a_corrupt_ledger_makes_the_day_undetermined_and_blocks] [test: crates/budgetguard/tests/prose_claims.rs::claim_day_undetermined_warn_only]。
  セッション自身の block 上限を既に超えていれば
  その具体的な理由を返す [test: crates/budgetguard/src/gate.rs::a_contended_lock_still_reports_a_session_block_precisely]。
- 測定できなかった合計は stderr に `$0` ではなく `unknown` と表示される [test: crates/budgetguard/tests/prose_claims.rs::claim_unknown_not_zero] [test: crates/budgetguard/tests/prose_claims.rs::claim_session_undetermined_warn_only]。

本リポジトリのルートにあるマーケットプレイスのカタログに budgetguard が載っている [test: crates/budgetguard/tests/prose_claims.rs::claim_marketplace_entry]:

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install budgetguard@yukineko
```

```sh
cargo install --path .
budgetguard init          # 雛形の ./budgetguard.toml を書き出す [test: crates/budgetguard/tests/prose_claims.rs::claim_init_writes_starter]
budgetguard install       # Stop フックを ~/.claude/settings.json にマージする [test: crates/budgetguard/tests/prose_claims.rs::claim_install_merges_stop_hook]
```

プロジェクトルートに `budgetguard.toml` を置くか、グローバル既定として
`~/.budgetguard/config.toml` を置く [test: crates/budgetguard/tests/prose_claims.rs::claim_config_lookup_home_and_project_not_merged]。
`budgetguard.example.toml` はそのまま使える雛形で、`enabled`、コメントアウトされた `state_dir`、`[session]`、`[daily]`、コメントアウトされた `[[price]]` を含む [test: crates/budgetguard/tests/prose_claims.rs::claim_example_toml] [test: crates/budgetguard/tests/prose_claims.rs::claim_example_commented]。

```toml
[session]
warn_usd  = 0.50
block_usd = 2.00

[daily]
warn_usd  = 5.00
block_usd = 20.00
```

プロジェクトのファイルがあれば home のファイルの*代わりに*使われる (マージはしない) [test: crates/budgetguard/tests/prose_claims.rs::claim_config_lookup_home_and_project_not_merged]。
trust ゲートは
無く、リポジトリ同梱の `budgetguard.toml` もそのまま honor される [test: crates/budgetguard/tests/prose_claims.rs::claim_no_trust_gate]。
他のキー: `enabled`、`state_dir`
(既定 `~/.budgetguard/state`)、`[[price]]`、`[cache]` (`min_rate`, `min_tokens`) [test: crates/budgetguard/tests/prose_claims.rs::claim_example_toml] [test: crates/budgetguard/tests/prose_claims.rs::claim_ledger_latest_cost] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_below_floor_not_promoted] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_min_rate_default_and_inrange] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::control_valid_finite_override_allows] [test: crates/budgetguard/tests/prose_claims.rs::claim_cache_health_promoted_warning]。

```sh
budgetguard gate           # Stop フック (stdin の JSON を読み、判定を出す) [test: crates/budgetguard/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_known_model_over_limit_blocks]
budgetguard status         # 解決済みの設定 + 今日の支出 [test: crates/budgetguard/tests/integration.rs::status_prints_spend_summary]
budgetguard status --json  # 機械可読な予算プレッシャー [test: crates/budgetguard/tests/prose_claims.rs::claim_status_json_fields]:
                           #   {day_usd, daily_warn_usd, daily_block_usd, pressure} [test: crates/budgetguard/tests/prose_claims.rs::claim_status_json_fields]。
                           #   設定が読めなければ {error, undetermined:true} を出し exit 2 [test: crates/budgetguard/tests/prose_claims.rs::claim_status_json_fields] [test: crates/budgetguard/tests/prose_claims.rs::claim_status_json_unreadable]
budgetguard init [--force]          # 雛形の budgetguard.toml を書き出す [test: crates/budgetguard/tests/prose_claims.rs::claim_init_writes_starter]。--force が無ければ既存ファイルは変更しない [test: crates/budgetguard/tests/prose_claims.rs::claim_init_writes_starter]
budgetguard install [--dry-run]     # フックを ~/.claude/settings.json にマージする [test: crates/budgetguard/tests/prose_claims.rs::claim_install_merges_stop_hook]
budgetguard uninstall [--dry-run]   # フックを取り除く [test: crates/budgetguard/tests/prose_claims.rs::claim_install_merges_stop_hook]
```

`harness-core` の組み込み料金テーブルを使う [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table]:

| ファミリ | 入力 $/1M | 出力 $/1M [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table] [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_split] |
|---|---|---|
| Fable / Mythos | 10 | 50 [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table] [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_split] |
| Opus | 5 | 25 [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table] [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_split] [test: crates/harness-core/src/pricing.rs::opus_input_output] |
| Sonnet | 3 | 15 [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table] [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_split] |
| Haiku | 1 | 5 [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_table] [test: crates/budgetguard/tests/prose_claims.rs::claim_pricing_split] |

`budgetguard.toml` の `[[price]]` スタンザで任意のモデルの料金を上書きできる（`pattern` はモデル id に対する大文字小文字を区別しない部分一致）[test: crates/budgetguard/tests/price_pattern_case_ff98fed0.rs::f4_mixed_case_price_pattern_matches_and_allows] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::control_valid_finite_override_allows]。表にも上書きにも無いモデルの使用量の費用は 0 ではなく **不明** であり、ゲートはそのセッションを未計測（`cost-undetermined`）として扱う [test: crates/budgetguard/tests/backlog_ff98fed0.rs::backlog_ff98fed0_unknown_model_with_usage_blocks_not_priced_zero] [test: crates/budgetguard/src/gate.rs::fresh_gauge_record_with_unpriced_model_is_undetermined_and_blocks]。本当に無料のモデルは `input = 0.0` / `output = 0.0` で宣言する [test: crates/budgetguard/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_explicit_zero_override_allows]。`pattern` が空（全モデルに一致してしまう）または単価が有限の非負数でない `[[price]]` 行は、パース失敗と同じく設定全体を使用不能にする [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::r3_empty_pattern_is_config_undetermined_not_a_wildcard] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::r3_nan_rate_is_config_undetermined] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::r3_inf_rate_is_config_undetermined] [test: crates/budgetguard/tests/price_config_residual_00880b3d.rs::r3_negative_rate_is_config_undetermined]。

ライセンス: MIT [test: crates/budgetguard/tests/prose_claims.rs::claim_license_mit]。
