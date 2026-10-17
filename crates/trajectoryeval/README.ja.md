`trajectoryeval` は軌跡 (trajectory) 照合検証ツールである [test: crates/trajectoryeval/tests/integration.rs::help_describes_the_verifier]。

[record: 2026-06-29 rev: ccc8fc2d] [langchain-ai/agentevals](https://github.com/langchain-ai/agentevals) の trajectory matcher に着想を得ている。

`trajectoryeval` は worker が**たどった経路 (PATH)** を検証する、つまり worker が実行した**ツール呼び出しの順序付き列**を、期待される軌跡 spec と突き合わせる [test: crates/trajectoryeval/tests/integration.rs::check_matching_trajectory_passes_exit_zero] [test: crates/trajectoryeval/tests/integration.rs::check_deviating_trajectory_exits_one]。

`trajectoryeval check --expected <spec.json> --actual <actual.json> [--json]` は実際のツール列を期待 spec と照合し、`{ pass, missing, unexpected, out_of_order }` を報告する [test: crates/trajectoryeval/tests/prose_claims.rs::claim_check_json_flag]。`--json` を付けると人間向けレポートの代わりにシリアライズ結果を JSON で出力する [test: crates/trajectoryeval/tests/prose_claims.rs::claim_check_json_flag]。

`trajectoryeval tier --config <cfg.json> --flow <id> [...]` は、設定の allowlist でフローを **core**(ビジネスクリティカル)か **non-core** に分類する [test: crates/trajectoryeval/src/tier.rs::tier_core_vs_non_core]。

- tier の config は次の形の JSON である [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero]:
  ```json
  { "core": ["checkout", "payment"],
    "diff_strategy": "structured_data",
    "sample_one_in": 20 }
  ```
  `core` = コア allowlist(exact-match) [test: crates/trajectoryeval/tests/prose_claims.rs::claim_core_allowlist_exact_match]。
  `diff_strategy` の既定は `structured_data` [test: crates/trajectoryeval/tests/prose_claims.rs::claim_diff_strategy_default_structured]。
  `sample_one_in` = non-core フローを `1 in N` 回だけサンプルする率 [test: crates/trajectoryeval/src/tier.rs::seeded_sampling_rate_is_approximately_one_in_n] [test: crates/trajectoryeval/src/tier.rs::sample_one_in_one_always_samples] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_sampling_decision_reported]。
  `sample_one_in` が 0 ならサンプリング無効=存在チェックのみ [test: crates/trajectoryeval/src/tier.rs::non_core_sampling_disabled_is_existence_only]。

- **core フロー**は毎回、スナップショットを baseline と **diff** する(`--baseline <base.json> --snapshot <snap.json>` を渡す) [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero] [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_mismatched_snapshot_exits_one]。
  構造化データ照合は正規化された shape の JSON 等価性で、オブジェクトのキー順は正規化され無視、値・構造は一致必須である [test: crates/trajectoryeval/src/tier.rs::structured_match_key_order_normalized] [test: crates/trajectoryeval/src/tier.rs::structured_mismatch_value] [test: crates/trajectoryeval/src/tier.rs::structured_mismatch_missing_key_and_len]。
  差異は `/` 区切りのパスで列挙される [test: crates/trajectoryeval/src/tier.rs::structured_mismatch_value] [test: crates/trajectoryeval/src/tier.rs::structured_diff_is_deterministic_ordering]。
  `diff_strategy: "screenshot"` を選ぶと `DiffOutcome::Stubbed` を返す(未実装) [test: crates/trajectoryeval/src/tier.rs::screenshot_strategy_is_stubbed]。

- `diff_strategy: "fuzzy_hash"` は許容差つきの構造化データ照合である [test: crates/trajectoryeval/src/tier.rs::fuzzy_diff_small_drift_under_threshold_is_match]。決定論的なドリフト距離 `distance_permille = round(1000 × 差異のある葉の数 / 葉の総数)` を計算し(差異のある葉が1つでもあれば最低 `1` に切り上げ、実際のドリフトが `0` に丸められることはない [test: crates/trajectoryeval/tests/repro_fuzzy_hash_default_threshold_rounds_drift_away.rs::zero_tolerance_fuzzy_hash_does_not_pass_a_changed_leaf])、`distance_permille <= threshold_permille` なら **match** とみなす [test: crates/trajectoryeval/src/tier.rs::fuzzy_diff_boundary_distance_equal_threshold_is_match] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_fuzzy_distance_rounding]。超えた場合は `DiffOutcome::DriftedBeyondThreshold`(距離と差異位置の `/` 区切りパス)を返し [test: crates/trajectoryeval/src/tier.rs::fuzzy_diff_drift_over_threshold_reports_distance_and_paths]、これは fail ではなく **needs-human** 判定(exit `3`)になる [test: crates/trajectoryeval/src/tier.rs::verdict_for_diff_drifted_beyond_threshold_is_needs_human_not_fail] [test: crates/trajectoryeval/tests/integration.rs::tier_fuzzy_core_flow_drift_over_threshold_exits_three_needs_human]。
  `threshold_permille` は config のキーで、既定は `0` である [test: crates/trajectoryeval/tests/prose_claims.rs::claim_threshold_permille_default_zero]。
  [`examples/tier-config-fuzzy.json`](examples/tier-config-fuzzy.json) は `threshold_permille: 250` を設定している [test: crates/trajectoryeval/tests/prose_claims.rs::claim_example_fuzzy_threshold_250]。

- **non-core フロー**は軽量な **存在チェック** を受ける(`--exists true|false`) [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_absent_flow_exits_one]。`--exists` に既定値は無く、non-core フローで省略すると harness エラー(exit `2`)になる [test: crates/trajectoryeval/tests/repro_tier_exists_defaults_true.rs::omitted_exists_does_not_pass_a_non_core_flow]。さらに `sample_one_in > 0` なら、`(flow_id, --seed, --run-index)` を鍵にした **決定論的な低頻度サンプリング**で、その run がサンプル対象かを決め、同じ入力なら常に同じ判定になる [test: crates/trajectoryeval/src/tier.rs::seeded_sampling_is_reproducible] [test: crates/trajectoryeval/src/tier.rs::seeded_sampling_varies_by_run_index] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_sampling_keyed_by_seed_and_flow]。
  サンプリング判定は**報告されるだけ**(`present (sampled this run)` / `present (not sampled this run)`) [test: crates/trajectoryeval/tests/prose_claims.rs::claim_sampling_decision_reported]。
  どちらも pass [test: crates/trajectoryeval/src/tier.rs::verdict_for_non_core_never_needs_human]。
  non-core フローを fail にするのは `--exists false` だけ [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_existing_flow_passes_exit_zero] [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_absent_flow_exits_one] [test: crates/trajectoryeval/src/tier.rs::non_core_absent_fails_existence] [test: crates/trajectoryeval/src/tier.rs::verdict_for_non_core_never_needs_human]。

CLI はフローの core/non-core 分類と diff(match/mismatch)結果、または non-core の存在/サンプリング判定を、人間向けレポートまたは `--json` で報告する [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero] [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_existing_flow_passes_exit_zero] [test: crates/trajectoryeval/tests/integration.rs::tier_json_output_reports_verdict_field] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_tier_json_report_core_and_non_core]。

- `tier` の終了コード `0` は pass [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero]。
- `tier` の終了コード `1` は実際の逸脱(mismatch、または存在チェック失敗) [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_mismatched_snapshot_exits_one] [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_absent_flow_exits_one]。
- `tier` の終了コード `2` は harness エラー(入力が読めない/パースできない、または core フローで `--baseline` と `--snapshot` の両方が無い) [test: crates/trajectoryeval/tests/prose_claims.rs::claim_tier_exit_two_harness_error]。
- `tier` の終了コード `3` は **needs-human** — core フローが未実装の diff 戦略(現状 `screenshot`)を使った、または `fuzzy_hash` のドリフトが `threshold_permille` を超えたため、自動判定を出せない [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_screenshot_stub_is_needs_human_not_silently_red] [test: crates/trajectoryeval/tests/integration.rs::tier_fuzzy_core_flow_drift_over_threshold_exits_three_needs_human]。`3` は `1` と区別される [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_screenshot_stub_is_needs_human_not_silently_red]。未実装の戦略で run を黙って赤にしてはならない: exit `1` にも `result: fail` にもならない [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_screenshot_stub_is_needs_human_not_silently_red]。

- 実行可能な例: [`examples/tier-config.json`](examples/tier-config.json) [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero]
  ```sh
  trajectoryeval tier --config examples/tier-config.json --flow checkout \
    --baseline base.json --snapshot snap.json     # core フロー → 実 diff [test: crates/trajectoryeval/tests/integration.rs::tier_core_flow_matching_snapshot_passes_exit_zero]
  trajectoryeval tier --config examples/tier-config.json --flow settings \
    --exists true --seed 42 --run-index 3          # non-core → 存在チェック [test: crates/trajectoryeval/tests/integration.rs::tier_non_core_existing_flow_passes_exit_zero]
  ```

- **expected** spec は次の形の JSON である [test: crates/trajectoryeval/src/match_traj.rs::spec_deserializes_from_json]:
  ```json
  { "mode": "strict",
    "steps": [ { "tool": "Read" }, { "tool": "Edit", "optional": true } ] }
  ```
  `mode` は必須(`strict` / `unordered` / `subsequence`) [test: crates/trajectoryeval/tests/prose_claims.rs::claim_mode_required]。
  `optional` の既定値は `false` [test: crates/trajectoryeval/src/match_traj.rs::step_optional_defaults_false]。
  `steps` は省略可で既定は空 [test: crates/trajectoryeval/tests/prose_claims.rs::claim_steps_default_empty]。
- **actual** の JSON は、ツール名文字列の配列、例 `["Read", "Edit"]`(`extract` の出力をそのまま渡せる) [test: crates/trajectoryeval/tests/integration.rs::extract_then_check_pipeline_passes_on_matching_trajectory]。

`trajectoryeval extract --transcript <jsonl>` は Claude Code のトランスクリプトを読み、`tool_use` の名前を出現順に JSON 配列として stdout に出力し、そのまま `check --actual` に流し込める [test: crates/trajectoryeval/src/extract.rs::extracts_tool_names_in_order] [test: crates/trajectoryeval/tests/integration.rs::extract_then_check_pipeline_passes_on_matching_trajectory]。ファイル自体が読めない場合は exit `2` になる [test: crates/trajectoryeval/tests/prose_claims.rs::claim_extract_unreadable_exits_two]。JSON として不正な行がある場合や、イベントが1つも無い場合も exit `2` であり、読み飛ばして空の trajectory にすることはない [test: crates/trajectoryeval/src/extract.rs::unparseable_line_is_invalid_data_not_skipped] [test: crates/trajectoryeval/tests/repro_extract_garbage_transcript_exits_zero.rs::extract_of_unparseable_transcript_does_not_exit_zero]。

- **strict** — 実際の列が、期待される**必須**ステップと順序まで一致しなければならない [test: crates/trajectoryeval/src/match_traj.rs::strict_exact_match_passes]。optional ステップは欠けていてもよいが、存在する場合は所定の位置になければならない [test: crates/trajectoryeval/src/match_traj.rs::strict_optional_absent_passes] [test: crates/trajectoryeval/src/match_traj.rs::strict_optional_present_in_slot_passes] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_strict_optional_out_of_slot_fails]。
  `missing` = マッチしなかった必須ステップ [test: crates/trajectoryeval/src/match_traj.rs::strict_missing_required_step]、`unexpected` = 期待順序のどこにも収まらなかった実ツール [test: crates/trajectoryeval/src/match_traj.rs::strict_extra_tool_is_unexpected]、`out_of_order` = 正しい集合は揃ったが順序が誤っている [test: crates/trajectoryeval/src/match_traj.rs::strict_wrong_order_flags_out_of_order]。
- **unordered** — 順序を無視する [test: crates/trajectoryeval/src/match_traj.rs::unordered_ignores_order]。`missing` = 実際に存在しない必須期待ツール(集合として)、`unexpected` = 期待集合に無い実ツール [test: crates/trajectoryeval/src/match_traj.rs::unordered_reports_missing_and_unexpected_sets]、`out_of_order` は常に false [test: crates/trajectoryeval/src/match_traj.rs::unordered_ignores_order]。
- **subsequence** — 必須ステップが `actual` の中に順序どおり現れればよい(連続している必要はなく、他のツールが間に挟まってよい) [test: crates/trajectoryeval/src/match_traj.rs::subsequence_interleaved_passes]。`missing` = 順序付き部分列として見つからなかった必須ステップ [test: crates/trajectoryeval/src/match_traj.rs::subsequence_out_of_order_is_missing_not_reorder]。余分は許容されるので `unexpected` は空のまま、`out_of_order` は false [test: crates/trajectoryeval/src/match_traj.rs::subsequence_interleaved_passes]。必須ステップを1つも持たない `subsequence` spec は何も制約しないので、`check` は pass にせず exit `2` で拒否する [test: crates/trajectoryeval/tests/repro_empty_subsequence_spec_passes_anything.rs::empty_subsequence_spec_does_not_exit_zero]。

いずれのモードでも: `pass = missing.is_empty() && unexpected.is_empty() && !out_of_order` [test: crates/trajectoryeval/src/match_traj.rs::strict_exact_match_passes] [test: crates/trajectoryeval/src/match_traj.rs::strict_missing_required_step] [test: crates/trajectoryeval/src/match_traj.rs::strict_extra_tool_is_unexpected] [test: crates/trajectoryeval/src/match_traj.rs::strict_wrong_order_flags_out_of_order] [test: crates/trajectoryeval/src/match_traj.rs::unordered_ignores_order] [test: crates/trajectoryeval/src/match_traj.rs::unordered_reports_missing_and_unexpected_sets] [test: crates/trajectoryeval/src/match_traj.rs::subsequence_interleaved_passes] [test: crates/trajectoryeval/src/match_traj.rs::subsequence_out_of_order_is_missing_not_reorder]。

- `check` の終了コード `0`: 軌跡が spec と一致した (pass) [test: crates/trajectoryeval/tests/integration.rs::check_matching_trajectory_passes_exit_zero]。
- `check` の終了コード `1`: 逸脱あり (missing / unexpected / 順序違反) [test: crates/trajectoryeval/tests/integration.rs::check_deviating_trajectory_exits_one] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_check_exit_one_unexpected_and_out_of_order]。
- `check` の終了コード `2`: harness エラー (入力が読めない / パースできない) [test: crates/trajectoryeval/tests/integration.rs::check_unreadable_input_exits_two] [test: crates/trajectoryeval/tests/prose_claims.rs::claim_check_exit_two_unparseable]。

これはライフサイクル hook ではなく、ふつうの CLI **ゲート**である [test: crates/trajectoryeval/tests/prose_claims.rs::claim_not_a_lifecycle_hook]。

condukt のタスク schema は `expected_trajectory` という省略可フィールドを持つ(`crates/condukt/src/model.rs`) [test: crates/condukt/src/model.rs::task_with_expected_trajectory_round_trips] [test: crates/condukt/src/model.rs::task_without_expected_trajectory_defaults_none]。

```sh
trajectoryeval extract --transcript session.jsonl > actual.json
echo '{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}' > spec.json
trajectoryeval check --expected spec.json --actual actual.json --json
```
