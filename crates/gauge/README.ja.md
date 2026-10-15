記録はセッションごとに 1 レコード（`<store>/sessions/<session_id>.json`）として保存される [test: crates/gauge/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_opus_session_costs_five_dollars]。
レコードは毎ターン上書きされ、最新の書き込みがここまでのセッション全体を保持する [test: crates/gauge/tests/prose_claims.rs::claim_record_cumulative_single_file]：

- モデル別の input / output / **キャッシュ書き込み (5分・1時間 TTL)** / **キャッシュ読み込み** トークン [test: crates/gauge/tests/prose_claims.rs::claim_record_token_kinds_per_model]
- モデルリクエスト数（ターン数）とツール別の呼び出し回数（Bash, Edit, …） [test: crates/gauge/tests/prose_claims.rs::claim_record_turns_and_tool_counts]
- 最初/最後のタイムスタンプ → セッション継続時間 [test: crates/gauge/tests/prose_claims.rs::claim_record_first_last_duration]

**推定コスト** はレコードの読み出し時に組み込み価格表（モデル単位で上書き可能）から算出される [test: crates/gauge/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_opus_session_costs_five_dollars] [test: crates/gauge/tests/config_residual_00880b3d.rs::control_valid_finite_override_applies]。

`gauge report` が、プロジェクト・モデル・日付ごとにこれらを集計する [test: crates/gauge/tests/prose_claims.rs::claim_report_rollup_project_model_day]。
`gauge subagents` は、サブエージェント（Task）ごとのトランスクリプトからコストを個別に按分する [test: crates/gauge/tests/prose_claims.rs::claim_subagents_live_per_task]。

記録を完全に止めたいときは `GAUGE_DISABLE=1` を指定する [test: crates/gauge/tests/prose_claims.rs::claim_gauge_disable_env]。

Claude Code プラグインとしては、バンドル済みの `bin/gauge` が `hooks/hooks.json` 経由（`${CLAUDE_PLUGIN_ROOT}/bin/gauge record`）で Stop フックとして自動的に呼ばれる [test: crates/gauge/tests/prose_claims.rs::claim_hooks_json_stop_record]。
追加設定なしで記録が始まる [test: crates/gauge/tests/prose_claims.rs::claim_records_without_config]。

スタンドアロンでは次を実行せよ。

```sh
cargo install --path .
gauge install        # Stop フックを ~/.claude/settings.json にマージする [test: crates/gauge/tests/prose_claims.rs::claim_install_merges_stop_hook]
gauge uninstall      # 元に戻す [test: crates/gauge/tests/prose_claims.rs::claim_uninstall_removes_hook]
```

```sh
gauge report                       # 合計とプロジェクト/モデル/日別の内訳 [test: crates/gauge/tests/prose_claims.rs::claim_report_rollup_project_model_day]
gauge report --project myrepo      # プロジェクトで絞り込む [test: crates/gauge/tests/prose_claims.rs::claim_report_project_filter]
gauge report --since 2026-06-01    # 日付で絞り込む [test: crates/gauge/tests/prose_claims.rs::claim_report_since_filter]
gauge session                      # 直近セッションの詳細 [test: crates/gauge/tests/prose_claims.rs::claim_session_most_recent]
gauge session --session <ID> --json  # 指定セッションを {session_id, cost_usd, models, agents} の JSON で [test: crates/gauge/tests/prose_claims.rs::claim_session_json_shape]
gauge subagents                    # サブエージェント(Task)別のコスト按分 [test: crates/gauge/tests/prose_claims.rs::claim_subagents_live_per_task]
gauge subagents --session <ID> --json  # JSON 配列 [test: crates/gauge/tests/prose_claims.rs::claim_subagents_json_array_null_times]。時刻フィールドは不明なら null [test: crates/gauge/tests/prose_claims.rs::claim_subagents_json_array_null_times]
gauge status                       # 解決済み設定・ストアパス・セッション数 [test: crates/gauge/tests/prose_claims.rs::claim_status_fields]
gauge init [--force]               # スターター ./gauge.toml を書き出す(--force で上書き) [test: crates/gauge/tests/prose_claims.rs::claim_init_force]
```

`gauge install` / `gauge uninstall` は `--dry-run` を受け付ける [test: crates/gauge/tests/prose_claims.rs::claim_install_dry_run]。

```sh
gauge config show                  # 登録済みの窓設定があれば表示する [test: crates/gauge/tests/backlog_49d9bcae.rs::control_window_show_reads_valid_and_missing]
```

組み込みレート（USD / 100万トークン, input/output）: **Opus** 5/25 ・ **Sonnet** 3/15 ・ **Haiku** 1/5 ・ **Fable/Mythos** 10/50 [test: crates/gauge/tests/prose_claims.rs::claim_builtin_rates]。
キャッシュ書き込みは input の 1.25倍（5分 TTL）または 2倍（1時間 TTL）、キャッシュ読み込みは input の 0.1倍で課金される [test: crates/gauge/tests/prose_claims.rs::claim_cache_multipliers]。
認識できないモデルの使用量の費用は 0 ではなく **不明** として扱われ、`unknown` と表示される [test: crates/gauge/tests/consumer_erasure_ff98fed0.rs::ff98fed0_consumer_session_text_cost_is_unknown] [test: crates/gauge/tests/consumer_erasure_ff98fed0.rs::ff98fed0_consumer_report_total_is_unknown_not_partial_sum] [test: crates/gauge/tests/consumer_erasure_ff98fed0.rs::ff98fed0_consumer_status_total_is_unknown_not_partial_sum]。
JSON では `cost_usd: null` と `cost_undetermined` になる [test: crates/gauge/tests/prose_claims.rs::claim_unpriced_json_null_undetermined]。
budgetguard はそのセッションを未計測とみなす [test: crates/budgetguard/src/gate.rs::fresh_gauge_record_with_unpriced_model_is_undetermined_and_blocks]。
本当に無料のモデルは `input = 0.0` / `output = 0.0` の明示的な上書きで宣言する [test: crates/gauge/tests/config_residual_00880b3d.rs::control_explicit_zero_rate_is_a_known_zero] [test: crates/gauge/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_explicit_zero_override_is_known_zero]。
`gauge.toml` でモデルを上書き・追加できる [test: crates/gauge/tests/config_residual_00880b3d.rs::control_valid_finite_override_applies] [test: crates/gauge/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_explicit_zero_override_is_known_zero]。

```toml
[[pricing]]
pattern = "opus"   # モデル id への部分一致 [test: crates/gauge/tests/prose_claims.rs::claim_pricing_pattern_first_match]。最初に一致したものが勝つ [test: crates/gauge/tests/prose_claims.rs::claim_pricing_pattern_first_match]
input = 5.0
output = 25.0
```

コストはレポートのたびに再計算されるため、価格表を編集すれば過去分も再評価される [test: crates/gauge/tests/prose_claims.rs::claim_report_reprices_history]。

プロジェクトの `./gauge.toml` > `~/.gauge/config.toml` > 組み込みデフォルトの順（最初に存在したファイルが勝つ） [test: crates/gauge/tests/prose_claims.rs::claim_config_lookup_order]。
`gauge.example.toml` を参照 [test: crates/gauge/tests/config_residual_00880b3d.rs::control_example_toml_verbatim_loads]。
ストアの既定値は `~/.gauge/store` [test: crates/gauge/tests/backlog_ff98fed0.rs::backlog_ff98fed0_control_opus_session_costs_five_dollars]。

プロジェクトの `./gauge.toml` が存在するのに読めない・パースできない（`[[pricng]]` や `enabeld` のような typo を含む未知キーはすべてパース失敗扱い）、または `[[pricing]]` 行に `pattern`/`input`/`output` が欠けている・単価が有限の非負数でない場合、既定値へ黙って落ちない [test: crates/gauge/tests/prose_claims.rs::claim_bad_config_refuses] [test: crates/gauge/tests/prose_claims.rs::claim_unreadable_config_refuses]。読み取りコマンド（`report` / `session` / `subagents` / `status`）はファイル名を含むメッセージを出して exit 1 し、`record` hook は何も記録せず stderr に理由を出す [test: crates/gauge/tests/prose_claims.rs::claim_bad_config_refuses] [test: crates/gauge/tests/prose_claims.rs::claim_unreadable_config_refuses]。

```sh
cargo build --release
cargo test
```
