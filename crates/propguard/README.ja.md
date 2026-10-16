`Stop` のたびに、現在のタスクの `done_criteria` から *semantic property*（不変条件）を導出し [test: crates/propguard/src/derive.rs::derives_matching_properties_from_criteria] [test: crates/propguard/src/derive.rs::derivation_is_capped_at_max] [test: crates/propguard/src/derive.rs::thin_criteria_are_padded_to_the_minimum]、生成コードがそれを満たすかを検査してからターン完了を許可します [test: crates/propguard/tests/integration.rs::with_criteria_inject_blocks_first_stop_but_exits_zero] [test: crates/propguard/tests/integration.rs::block_appends_propguard_signed_violation]。

[record: 2026-07-01 rev: 10de5291] プロパティの導出は **PGS**（Property-Generated Solver, [arXiv:2506.18315](https://arxiv.org/pdf/2506.18315)）に倣います。

Stop hook 1 本が同梱バイナリの `propguard check` を実行する [test: crates/propguard/tests/prose_claims.rs::claim_one_stop_hook]。

バイナリはプロパティの *導出* と count→threshold のブロック判定を決定論的に担い [test: crates/propguard/src/derive.rs::derivation_is_deterministic] [test: crates/propguard/src/gate.rs::below_threshold_is_the_single_comparison]、各プロパティが実際に *成り立つか* の意味判断は、subscription 上で動いている当のエージェント（inject モード）か、設定した独立チェッカー（subprocess モード）に委ねます [test: crates/propguard/src/gate.rs::inject_mode_block_reason_states_that_propguard_itself_did_not_judge] [test: crates/propguard/tests/integration.rs::block_appends_propguard_signed_violation]。

導出は bilingual なキーワード分類に基づく決定論的ルールで、`max_properties` 個に上限 [test: crates/propguard/src/derive.rs::derivation_is_deterministic] [test: crates/propguard/src/derive.rs::japanese_criteria_derive_properties] [test: crates/propguard/src/derive.rs::derivation_is_capped_at_max]。

| id | 不変条件 [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
|----|----------|
| `error-path` | 失敗パスは panic せず Err/エラーを返す [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
| `output-schema` | 出力スキーマ/フォーマットが安定している [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
| `determinism` | 決定論的: 同一入力は同一出力を返す [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
| `idempotence` | 冪等: 複数回実行しても結果が変わらない [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
| `bounds-monotonicity` | 境界・単調性・閾値が守られる [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |
| `no-partial-write` | 部分書き込みが起きない [test: crates/propguard/tests/prose_claims.rs::claim_catalog] |

done_criteria にキーワードが現れたプロパティを導出し、`min_properties` に満たなければ *universal*（基礎）不変条件（`error-path`, `output-schema`, `determinism`）で補完します [test: crates/propguard/src/derive.rs::derives_matching_properties_from_criteria] [test: crates/propguard/src/derive.rs::thin_criteria_are_padded_to_the_minimum]。

オフラインで導出を確認せよ [test: crates/propguard/tests/integration.rs::derive_subcommand_lists_properties]：

```
propguard derive "冪等に再実行でき、失敗時は panic せずエラーを返し、出力スキーマを壊さないこと"
```

done_criteria の取得元は優先順に [test: crates/propguard/src/derive.rs::env_criteria_take_priority] [test: crates/propguard/tests/prose_claims.rs::claim_criteria_file_default]：

1. 環境変数 `PROPGUARD_CRITERIA` [test: crates/propguard/src/derive.rs::env_criteria_take_priority]
2. project root の `criteria_file`（既定 `.propguard-criteria`） [test: crates/propguard/tests/prose_claims.rs::claim_criteria_file_default]
3. `propguard.toml` の inline `done_criteria` [test: crates/propguard/src/derive.rs::inline_criteria_used_when_no_env_or_file]

いずれも設定されていなければ **すべての停止を許可** [test: crates/propguard/tests/integration.rs::no_criteria_allows_the_stop] [test: crates/propguard/src/derive.rs::no_criteria_source_is_none]。ただし `criteria_file` が存在するのに読めず inline `done_criteria` も空の場合は「未設定」ではなく判定不能として **ブロック**（`criteria-unreadable`） [test: crates/propguard/src/gate.rs::ca_propguard_01_unreadable_criteria_file_blocks_fail_closed]。

| モード | 動作 [test: crates/propguard/src/config.rs::known_modes_parse_and_leave_config_valid] |
|--------|------|
| `inject` | 新しい diff ごとに 1 回ブロックして [test: crates/propguard/src/gate.rs::inject_first_pass_blocks_as_unverified] [test: crates/propguard/src/gate.rs::inject_block_arms_trust_after_one_block] **プロパティ・チェックリスト** を注入する [test: crates/propguard/src/gate.rs::inject_mode_block_reason_still_lists_the_derived_properties_and_target_files]。propguard は各ファイルの作成者を検証していないため、検査対象には人間や別セッションの変更も含まれうる [test: crates/propguard/src/gate.rs::inject_mode_block_reason_describes_the_files_as_unattributed_uncommitted_changes]。 |
| `subprocess` | `checker_cmd`（既定 `claude -p`） [test: crates/propguard/src/trust_worktree_tests.rs::linked_worktree_of_untrusted_main_still_ignores_project_config] を起動し、プロパティごとに `PROP <id>: PASS\|FAIL` を 1 行出力させ、PASS を数える [test: crates/propguard/src/gate.rs::parse_counts_only_explicit_pass] [test: crates/propguard/src/gate.rs::zero_exit_with_a_pass_line_still_parses_as_verified]。 |

既定のモードは `inject` [test: crates/propguard/tests/prose_claims.rs::claim_default_mode_inject]。

`gate::below_threshold` で：**`satisfied < threshold` のときブロック** [test: crates/propguard/src/gate.rs::below_threshold_is_the_single_comparison] [test: crates/propguard/src/gate.rs::below_threshold_blocks] [test: crates/propguard/src/gate.rs::at_threshold_allows_and_records_hash]。
inject モードでは新しい diff は *未検証* なので、まずチェックリスト注入のため 1 回ブロックし、次の停止では同じ diff を許可する — *trust-after-one-block*：エージェントが実際にプロパティを検証したかを propguard は観測しておらず、保証するのは「チェックリストを 1 回提示した」ことだけ [test: crates/propguard/tests/prose_claims.rs::claim_already_verified] [test: crates/propguard/src/gate.rs::inject_mode_block_reason_states_that_propguard_itself_did_not_judge] [test: crates/propguard/src/gate.rs::inject_first_pass_blocks_as_unverified] [test: crates/propguard/src/gate.rs::inject_block_arms_trust_after_one_block]。
subprocess モードでは PASS 数を直接比較 [test: crates/propguard/src/gate.rs::subprocess_mode_block_reason_keeps_the_count_a_checker_actually_produced]。
閾値は実際に導出されたプロパティ数にクランプされる [test: crates/propguard/src/gate.rs::effective_threshold_is_clamped_to_property_count]。

`(diff, properties)` をハッシュ化 [test: crates/propguard/tests/prose_claims.rs::claim_hash_diff_and_props]。
ハッシュが記録されるのは subprocess の合格（`properties-satisfied`）か inject の trust-after-one-block のとき [test: crates/propguard/src/gate.rs::at_threshold_allows_and_records_hash] [test: crates/propguard/src/gate.rs::inject_block_arms_trust_after_one_block] [test: crates/propguard/src/gate.rs::subprocess_block_does_not_arm_already_verified]。
記録済みハッシュと一致する停止は許可（`already-verified`） [test: crates/propguard/tests/prose_claims.rs::claim_already_verified]。
subprocess の閾値未満はハッシュを記録しないので次の停止で再検査する [test: crates/propguard/src/gate.rs::subprocess_block_does_not_arm_already_verified]。
ブロックした各ラウンドは `max_attempts`（既定 2、`reset_after_secs`〈既定 600〉の無操作でリセット）に数えられる [test: crates/propguard/tests/prose_claims.rs::claim_max_attempts_defaults]。
超えると閾値未満のまま **許可**（tag `giveup`、プロパティは未充足） [test: crates/propguard/src/gate.rs::below_threshold_gives_up_after_max_attempts]。

- git リポジトリでない / 検査対象なし / done_criteria が一切設定されていない → **許可** [test: crates/propguard/tests/git_probe_wiring.rs::genuine_non_repo_with_real_git_still_allows] [test: crates/propguard/src/gate.rs::a_genuinely_clean_repo_still_allows] [test: crates/propguard/tests/integration.rs::no_criteria_allows_the_stop]
- 設定ファイル（`propguard.toml` / `~/.propguard/config.toml`）が存在するのに読めない・解析できない → **ブロック**（`config-unreadable`、自動通過なし）— 組み込み既定値で黙って代用しない [test: crates/propguard/tests/prose_claims.rs::claim_no_giveup_config_errors] [test: crates/propguard/src/config.rs::ca_propguard_03_unreadable_or_unparseable_project_toml_blocks_fail_closed] [test: crates/propguard/src/gate.rs::config_load_error_blocks_fail_closed]
- 解析はできたが `mode` の値が未知 → **ブロック**（`config-invalid`、自動通過なし）— どちらのモードにも写さない [test: crates/propguard/tests/prose_claims.rs::claim_no_giveup_config_errors] [test: crates/propguard/tests/integration.rs::unknown_mode_blocks_config_invalid_naming_the_value] [test: crates/propguard/src/config.rs::unknown_mode_is_recorded_as_config_invalid_not_mapped] [test: crates/propguard/src/gate.rs::config_invalid_blocks_naming_the_value_and_known_mode_does_not]
- `criteria_file` が存在するのに読めず inline も空 → **ブロック**（`criteria-unreadable`、自動通過なし） [test: crates/propguard/tests/prose_claims.rs::claim_no_giveup_config_errors] [test: crates/propguard/src/gate.rs::ca_propguard_01_unreadable_criteria_file_blocks_fail_closed] [test: crates/propguard/src/derive.rs::unreadable_criteria_file_falls_through_to_inline]
- `checker-error-giveup` は overwatch の violation ledger に記録し、セッションを跨いで再発したら **ブロック**（`checker-outage-systemic`）、ledger が読めなければ **ブロック**（`checker-outage-undetermined`） [test: crates/propguard/tests/prose_claims.rs::claim_outage_ledger_e2e] [test: crates/propguard/src/gate.rs::escalate_giveup_systemic_becomes_block]。
  エスカレーションの mapper（`gate::escalate_giveup_on_systemic`）は systemic な `git-scan-failed-giveup` / `diff-read-failed-giveup` もブロックに写す [test: crates/propguard/src/gate.rs::ca_propguard_02_git_scan_giveup_also_escalates_when_systemic] [test: crates/propguard/src/gate.rs::ca_propguard_02_diff_read_giveup_also_escalates_when_systemic] [test: crates/propguard/src/gate.rs::ca_propguard_02_outage_scan_covers_new_giveups_but_not_truncated]。
  `truncated-giveup` は障害ではないので対象外 [test: crates/propguard/src/gate.rs::ca_propguard_02_outage_scan_covers_new_giveups_but_not_truncated]
- チェッカーが crash / timeout / 解析不能出力 → **ブロック**（有界）後に通過 [test: crates/propguard/src/gate.rs::shell_wrapped_timeout_kills_the_real_backgrounded_checker] [test: crates/propguard/src/gate.rs::checker_error_blocks_it_does_not_allow] [test: crates/propguard/src/gate.rs::checker_error_gives_up_after_max_attempts_but_never_traps] [test: crates/propguard/src/gate.rs::nonzero_exit_with_a_pass_line_on_stdout_is_an_error_not_a_verdict] [test: crates/propguard/src/gate.rs::parse_unrelated_output_is_error_not_all_pass]
- 実リポジトリ内で `git` コマンドが失敗（変更集合が判定不能）→ **ブロック**（有界、`git-scan-failed`）後に通過（`git-scan-failed-giveup`） [test: crates/propguard/src/gate.rs::failed_git_scan_blocks_it_does_not_allow] [test: crates/propguard/src/gate.rs::failed_git_scan_gives_up_after_max_attempts_but_never_traps] [test: crates/propguard/tests/git_probe_wiring.rs::unspawnable_git_in_a_repo_blocks_with_the_scan_failed_reason]
- 大きすぎて切り詰められた diff（未検査の末尾）→ **ブロック**（有界）後に通過 [test: crates/propguard/src/gate.rs::truncated_diff_blocks_then_gives_up]
- diff が **読めなかった** — `git diff` / `git diff --cached` / untracked の `ls-files` / untracked ファイル本体のいずれかがデコードできない → **ブロック**（有界、`diff-read-failed`）後に通過 [test: crates/propguard/src/gate.rs::a_diff_that_could_not_be_read_blocks_it_does_not_allow_as_empty] [test: crates/propguard/src/gate.rs::a_partially_read_diff_is_not_handed_on_as_complete] [test: crates/propguard/src/gate.rs::an_untracked_file_whose_body_could_not_be_read_blocks_as_undetermined] [test: crates/propguard/src/git.rs::diff_text_with_an_unreadable_untracked_scan_is_undetermined_not_an_empty_diff] [test: crates/propguard/src/gate.rs::unreadable_diff_gives_up_after_max_attempts_but_never_traps]。
  一方が読めて他方が読めない部分的なケースも同じで、読めた半分を diff 全体であるかのように渡さない [test: crates/propguard/src/gate.rs::a_partially_read_diff_is_not_handed_on_as_complete] [test: crates/propguard/src/git.rs::diff_text_with_one_unreadable_subcommand_is_undetermined_not_a_partial_diff]。
  読めなかった diff は判定不能であって「空 ⇒ 変更なし」ではない [test: crates/propguard/src/gate.rs::a_diff_that_could_not_be_read_blocks_it_does_not_allow_as_empty] [test: crates/propguard/src/git.rs::diff_text_of_an_undecodable_diff_is_undetermined_not_empty]

共有された project root の `.propguard-skip` ファイルはもう停止を通さない [test: crates/propguard/tests/session_scoped_skip.rs::an_unattributed_project_root_marker_no_longer_waves_a_propguard_stop_through]。
エスケープ：`propguard skip --reason "<理由>"`（1 回限り・理由必須・**発行したセッション限定** [test: crates/propguard/tests/session_scoped_skip.rs::a_skip_issued_by_another_session_does_not_wave_my_propguard_gate_through]・発行と消費の両方をゲートログに記録 [test: crates/propguard/tests/prose_claims.rs::claim_skip_contract]）、設定の `enabled = false` [test: crates/propguard/tests/prose_claims.rs::claim_enabled_false]、または `PROPGUARD_DISABLE=1` [test: crates/propguard/tests/integration.rs::disable_env_allows]。

## プラグインをインストールせよ

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install propguard@yukineko
```

## ソースからインストールせよ

```
cargo build --release -p propguard
./target/release/propguard install     # Stop hook を ~/.claude/settings.json に配線 [test: crates/propguard/tests/prose_claims.rs::claim_install_wires_stop]
propguard init                         # 雛形 ./propguard.toml を生成 [test: crates/propguard/tests/prose_claims.rs::claim_init_starter]
```

`propguard uninstall` は Stop hook を除去する [test: crates/propguard/tests/prose_claims.rs::claim_uninstall_removes]。
`propguard --protects` はこのゲートが何を・何から・なぜ守るかを表示する [test: crates/propguard/tests/protection_contract.rs::protects_prints_three_labelled_nonempty_lines_and_exits_zero]。
セッション状態は `~/.propguard/state`（設定キー `state_dir`、環境変数 `PROPGUARD_STATE_DIR` で上書き可） [test: crates/propguard/tests/prose_claims.rs::claim_state_dir]。

[`propguard.example.toml`](./propguard.example.toml) を参照 [test: crates/propguard/tests/prose_claims.rs::claim_example_toml]。
主なつまみ：`mode`、`min_properties`/`max_properties`（`max_properties` は 5 に上限、`min_properties` = 1 は 3 へ引き上げずそのまま受け付ける）、`threshold`、`max_attempts`、`reset_after_secs`、`min_changed_files`、`max_diff_bytes`、`criteria_file`、`include`/`exclude`、`enabled`、（subprocess）`checker_cmd` / `checker_timeout_secs` [test: crates/propguard/tests/prose_claims.rs::claim_key_knobs]。
project の `propguard.toml` は root を **trust** して初めて honored される [test: crates/propguard/src/trust_worktree_tests.rs::linked_worktree_of_untrusted_main_still_ignores_project_config] [test: crates/propguard/src/trust_worktree_tests.rs::trusted_plain_checkout_still_loads_project_config]。
root は `propguard trust` で trust する [test: crates/propguard/tests/prose_claims.rs::claim_trust_cmd]。

`propguard status` で解決済み設定と現タスクのプロパティを確認できる [test: crates/propguard/tests/prose_claims.rs::claim_status_shows]。

`subprocess` モードでは `checker_cmd` は実 OS subprocess として起動され、タイムアウトで上限が付く [test: crates/propguard/src/gate.rs::checker_stdout_flood_without_draining_stdin_does_not_hang] [test: crates/propguard/src/gate.rs::shell_wrapped_timeout_kills_the_real_backgrounded_checker]：

- **既定 300 秒** [test: crates/propguard/src/config.rs::checker_timeout_secs_defaults_to_300]。
- `propguard.toml`（または `~/.propguard/config.toml` [test: crates/propguard/tests/prose_claims.rs::claim_timeout_home_config]）のトップレベルキー `checker_timeout_secs` で設定 [test: crates/propguard/src/config.rs::checker_timeout_secs_load_from_toml_text_overrides_default]：

  ```toml
  checker_cmd = "claude -p"
  checker_timeout_secs = 45   # 既定 300 秒を上書き [test: crates/propguard/src/config.rs::checker_timeout_secs_is_overridable_from_file_config]
  ```

- `0` はタイムアウト無効化ではなく既定 300 秒へ sanitize される [test: crates/propguard/src/config.rs::claim_timeout_zero_sanitized]。
  不正値・負値は TOML 解析に失敗し、設定全体が読めない扱いで **ブロック**（`config-unreadable`）になる（黙って既定値にはならない） [test: crates/propguard/tests/prose_claims.rs::claim_timeout_negative_blocks]。
- **タイムアウト時はチェッカーのバックグラウンド化された子孫も kill する**：シェル経由の `checker_cmd` が `&` でバックグラウンド化した孫はタイムアウト後に生き残らない [test: crates/propguard/tests/prose_claims.rs::claim_timeout_kills_tree]。
  stdout パイプを開いたままの子孫がいても停止はハングしない [test: crates/propguard/src/gate.rs::lingering_stdout_holder_does_not_hang_the_read]。
- タイムアウトはチェッカーの `Error` として報告され（「全件合格」には決してならない）、他のチェッカー失敗と同じ fail-closed・`max_attempts` で有界な give-up 経路を通る [test: crates/propguard/src/gate.rs::shell_wrapped_timeout_kills_the_real_backgrounded_checker] [test: crates/propguard/src/gate.rs::checker_error_blocks_it_does_not_allow] [test: crates/propguard/src/gate.rs::checker_error_gives_up_after_max_attempts_but_never_traps]
