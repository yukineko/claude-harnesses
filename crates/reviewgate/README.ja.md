**コードレビュー・ゲート** [test: crates/reviewgate/tests/integration.rs::help_describes_the_gate]。

1 本の Stop フックを登録し、それがバンドルされた `reviewgate review` バイナリを実行する [test: crates/reviewgate/tests/prose_claims.rs::claim_hooks_one_stop]。

- `inject` は `mode` のデフォルト [test: crates/reviewgate/tests/prose_claims.rs::claim_default_mode_inject]。
- `inject` モードでは、セッション内でレビュー対象の変更がある最初の stop をブロックする [test: crates/reviewgate/tests/prose_claims.rs::claim_inject_once_per_diff_state]。
- 注入する理由文はレビュー用の**ルーブリック**をそのまま含み、ファイル一覧を示してレビューを求める [test: crates/reviewgate/src/review.rs::inject_reason_still_lists_the_files_carries_the_rubric_and_asks_for_a_review]。
- 注入する理由文は、一覧を作業ツリーの未コミット変更として記述し、作成者は検証していないと明記する [test: crates/reviewgate/src/review.rs::inject_reason_describes_the_files_as_unattributed_uncommitted_changes]。
- `subprocess` モードは `reviewer_cmd`（デフォルト `claude -p`）をレビュアーとして走らせる [test: crates/reviewgate/src/config.rs::project_reviewer_cmd_is_gated_behind_trust] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::reviewer_that_prints_lgtm_is_allowed_and_clean]。
- `subprocess` モードでは、レビュアーが LGTM と答えれば stop を通し [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::reviewer_that_prints_lgtm_is_allowed_and_clean]、報告された指摘はそのままブロック理由に描画される [test: crates/reviewgate/src/review.rs::subprocess_reason_still_lists_every_file_and_renders_the_findings_verbatim]。

最後にレビューを強制したときと同じ diff の stop は通過させる（ログタグ `already-reviewed`） [test: crates/reviewgate/tests/prose_claims.rs::claim_inject_once_per_diff_state]。
ブロックするラウンド数は `max_attempts` で上限がかかるため、エージェントが無限に閉じ込められることはない [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::inject_giveup_keeps_the_plain_giveup_tag]。
`max_attempts` のデフォルトは 2 [test: crates/reviewgate/tests/prose_claims.rs::claim_default_max_attempts_2]。

レビュー対象がそもそも存在しない場合（git リポジトリでない・レビュー対象の変更が無い）は stop を**許可**する [test: crates/reviewgate/tests/git_probe_wiring.rs::genuine_non_repo_with_real_git_still_allows] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::valid_include_that_excludes_the_change_allows_normally] [test: crates/reviewgate/tests/backlog_7d56f1e3_unknown_mode_blocks.rs::known_mode_on_clean_repo_allows]。
判定できなかった場合は許可しない：git コマンドが実リポジトリ内で失敗した変更集合は *undetermined* であり、**ブロック**する（`max_attempts` で有界） [test: crates/reviewgate/tests/git_probe_wiring.rs::unspawnable_git_in_a_repo_blocks_the_stop] [test: crates/reviewgate/src/review.rs::failed_git_scan_blocks_it_does_not_allow] [test: crates/reviewgate/src/review.rs::failed_git_scan_gives_up_after_max_attempts_but_never_traps]。
設定ファイルが読めない／parse できない場合もゲートは無効化されず、WARNING を出して組み込み既定値で動く [test: crates/reviewgate/tests/prose_claims.rs::claim_broken_config_still_gates]。
`review` フックは `harness_core::gate::run::run_guarded` の下で走る [test: crates/reviewgate/tests/prose_claims.rs::claim_panic_fails_closed]：最初の stop での panic はブロック（**fail closed**）し、再入時（`stop_hook_active`）の panic だけが許可へ落ちるので、壊れた reviewgate が turn を永久に塞ぐことはない [test: crates/harness-core/src/gate/run.rs::guard_maps_panic_to_action] [test: crates/harness-core/src/gate/run.rs::fail_closed_block_json_is_a_block_decision]。
ロックファイルはデフォルトで除外される [test: crates/reviewgate/src/review.rs::lockfile_excluded_by_default]。
`node_modules`・`target`・生成物はデフォルトで除外される [test: crates/reviewgate/tests/prose_claims.rs::claim_default_excludes]。

**誰の変更か**：変更集合はセッション帰属（`harness_core::attribution`、hook 入力の `transcript_path` を読む）で絞り込まれる：別セッションの transcript が主張し本セッションの transcript が主張しないファイルは除外される [test: crates/reviewgate/tests/session_attribution.rs::peer_written_file_is_excluded_from_the_review_set]。
その除外はブロック理由に追記される [test: crates/reviewgate/tests/session_attribution.rs::peer_written_file_is_excluded_from_the_review_set]。
その除外は stderr に出力される [test: crates/reviewgate/tests/prose_claims.rs::claim_attribution_exclusion_stderr]。
帰属が判定不能（transcript が無い／読めない）なら変更ファイルを全件レビューする [test: crates/reviewgate/tests/session_attribution.rs::unreadable_transcript_does_not_narrow_the_review_set] [test: crates/reviewgate/tests/session_attribution.rs::empty_transcript_path_does_not_narrow_the_review_set]。

*レビュアー*自体の失敗は「レビュー結果クリーン」とは**異なる**ため、無言で許可はしない（壊れたレビュアーがバイパスになってしまうため） [test: crates/reviewgate/src/review.rs::reviewer_error_blocks_it_does_not_allow]：

- レビュアーの subprocess が起動できない / timeout / 解析不能な出力（stdout が空・非 UTF-8）の場合 → **ブロック**（`max_attempts` で有界） [test: crates/reviewgate/src/review.rs::run_reviewer_reports_error_for_unspawnable_command] [test: crates/reviewgate/tests/prose_claims.rs::claim_key_fields_defaults] [test: crates/reviewgate/src/review.rs::reviewer_timeout_blocks_it_does_not_allow] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::non_utf8_reviewer_stdout_is_undetermined_not_clean] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::silent_reviewer_exit_zero_empty_stdout_is_undetermined_not_clean] [test: crates/reviewgate/src/review.rs::reviewer_error_gives_up_after_max_attempts_but_never_traps]。
- レビュアーに prompt を渡せなかった場合 → **ブロック** [test: crates/reviewgate/tests/prose_claims.rs::claim_prompt_undelivered_blocks]。
- diff が大きすぎて丸ごとレビューできず切り詰められた（`max_diff_bytes` で truncate）場合、未レビューの末尾が残る → **ブロック**（`max_attempts` で有界） [test: crates/reviewgate/src/review.rs::truncated_diff_blocks_it_does_not_allow] [test: crates/reviewgate/src/review.rs::truncated_diff_gives_up_after_max_attempts_but_never_traps] [test: crates/reviewgate/src/git.rs::over_limit_is_flagged_and_marked]。
- diff 内容の取得が一部でも失敗した（`git diff` / `git ls-files` のエラー、未追跡ファイルが読めない）場合、diff は不完全 → **ブロック**（変更スキャン失敗と同じ有界経路） [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::partial_untracked_fetch_failure_does_not_certify_the_diff_as_clean] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::partial_unstaged_diff_failure_does_not_certify_the_diff_as_clean] [test: crates/reviewgate/tests/backlog_922f0a96_diff_fetch_failure_wiring.rs::unreadable_untracked_file_blocks_via_fetch_failed]。
- 独立レビュアーが `max_attempts` 回を超えても所見を返し続けた場合は通過させるが、stderr に WARNING を出し、専用タグ `review-giveup` で記録し、overwatch の violation event も残す [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::violation_giveup_is_loud_distinctly_tagged_and_recorded_as_a_violation]。
- `include` の glob が 1 つでも不正でもレビュー対象は狭まらず、stderr にそのパターンが示される [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::partially_malformed_include_glob_does_not_silently_narrow_scope]。
- `exclude` の不正 glob は何も除外せず、stderr に WARNING を出す [test: crates/reviewgate/tests/prose_claims.rs::claim_malformed_exclude]。
- 設定ファイルが読めない／parse できない場合は stderr に WARNING を出し、`status` に `FAILED to load` と表示する [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::broken_home_config_warns_on_review_and_reports_failed_in_status] [test: crates/reviewgate/tests/prose_claims.rs::claim_broken_config_still_gates]。

切り詰めのブロック理由は `reviewgate skip --reason "<理由>"`・`REVIEWGATE_DISABLE=1`・`max_diff_bytes` の引き上げを示す [test: crates/reviewgate/src/review.rs::truncated_diff_blocks_it_does_not_allow]。レビュアー指摘のブロック理由は `reviewgate skip --reason` と `REVIEWGATE_DISABLE=1` を示す [test: crates/reviewgate/src/review.rs::subprocess_reason_remains_a_real_instruction_with_escape_hatches]。

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install reviewgate@yukineko
```

```
cargo install --path .
reviewgate init          # 雛形の ./reviewgate.toml を書き出す [test: crates/reviewgate/tests/prose_claims.rs::claim_init_force]
reviewgate install       # Stop フックを ~/.claude/settings.json に配線する [test: crates/reviewgate/tests/prose_claims.rs::claim_install_uninstall]
```

- `reviewgate review` — Stop フック本体（`session_id` を含むフック JSON を stdin から読む） [test: crates/reviewgate/tests/integration.rs::review_hook_valid_payload_exits_zero] [test: crates/reviewgate/tests/session_scoped_skip.rs::a_skip_issued_by_another_session_does_not_wave_my_reviewgate_gate_through]。
- `reviewgate install [--dry-run]` / `uninstall [--dry-run]` — フックの配線を管理する [test: crates/reviewgate/tests/prose_claims.rs::claim_install_uninstall]。
- `reviewgate init [--force]` — 雛形の `reviewgate.toml` を書き出す [test: crates/reviewgate/tests/prose_claims.rs::claim_init_force]。
- `reviewgate status` — 解決済みの設定と、いま何がレビュー対象になるかを表示する [test: crates/reviewgate/tests/prose_claims.rs::claim_status_shows_config_and_files]。
- `reviewgate trust` — 現在のプロジェクトを信頼し、その `./reviewgate.toml`（`reviewer_cmd` を含む）を honored にする [test: crates/reviewgate/tests/prose_claims.rs::claim_trust_cli_honors_project_config]。
- 信頼するまで、リポジトリ同梱の設定は無視される [test: crates/reviewgate/src/config.rs::project_reviewer_cmd_is_gated_behind_trust] [test: crates/reviewgate/src/trust_worktree_tests.rs::linked_worktree_of_untrusted_main_still_ignores_project_config]。
- `reviewgate skip --reason "<理由>"` — 一度だけ [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_consumed_once]・セッション限定のスキップ（下記参照） [test: crates/reviewgate/tests/session_scoped_skip.rs::a_skip_issued_by_another_session_does_not_wave_my_reviewgate_gate_through]。

`reviewgate review` を stdin 無しで手実行するとドライチェックになり、ブロック時は exit 1 でフック JSON を出さない [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::empty_stdin_keeps_manual_cli_mode_exit_1_on_block]。

[`reviewgate.example.toml`](reviewgate.example.toml) を参照 [test: crates/reviewgate/tests/prose_claims.rs::claim_example_toml]。
プロジェクトの `./reviewgate.toml` が `~/.reviewgate/config.toml` より、それが組み込みデフォルトより優先される（ただし `reviewer_cmd` を subprocess 実行するため、project root を **trust**（`reviewgate trust`）して初めて project 設定が honored される） [test: crates/reviewgate/src/config.rs::project_reviewer_cmd_is_gated_behind_trust]。

主なフィールド：`mode`、`max_attempts`、`reset_after_secs`（既定 600、セッションごとの試行カウンタがこの無操作秒数でリセットされる）、`min_changed_files`、`max_diff_bytes`（既定 200000）、`include`/`exclude` の glob、`rubric`、`state_dir`、そして（subprocess 用の）`reviewer_cmd` / `reviewer_timeout_secs` [test: crates/reviewgate/tests/prose_claims.rs::claim_key_fields_defaults]。
`max_attempts` / `min_changed_files` の `0` は 1 に clamp され、`max_diff_bytes` の `0` は既定値に戻る [test: crates/reviewgate/tests/prose_claims.rs::claim_zero_clamp]。

`mode` に指定できるのは `"inject"` か `"subprocess"`（別名 `reviewer` / `independent`） [test: crates/reviewgate/src/config.rs::known_modes_parse_without_err] [test: crates/reviewgate/src/config.rs::known_modes_loaded_from_config_leave_config_invalid_none]。
それ以外の値は **config-invalid** で、どちらのモードにも推測で割り当てず、（レビュー対象が無い場合も含め）stop を `config-invalid`（値を明示）でブロックする [test: crates/reviewgate/tests/backlog_7d56f1e3_unknown_mode_blocks.rs::unknown_mode_blocks_config_invalid_on_clean_repo] [test: crates/reviewgate/tests/backlog_7d56f1e3_unknown_mode_blocks.rs::unknown_mode_blocks_config_invalid_with_reviewable_change] [test: crates/reviewgate/src/config.rs::unknown_mode_is_err_naming_the_value]。
状態は `reviewgate status` で確認でき、抜け道は `reviewgate skip --reason ...` か `REVIEWGATE_DISABLE=1` [test: crates/reviewgate/tests/prose_claims.rs::claim_status_and_escapes_on_invalid_mode]。

プラグインは `agents/reviewgate-typing.md` も同梱する：tools に Read を含み Edit・Write を含まない subagent 定義である [test: crates/reviewgate/tests/prose_claims.rs::claim_typing_agent_readonly]。
`review` フックはこれを呼ばないので、明示的に呼び出して使う [test: crates/reviewgate/tests/prose_claims.rs::claim_review_hook_does_not_invoke_agent]。

- 一度だけ：`reviewgate skip --reason "<理由>"` [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_reason_required] [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_consumed_once]。
- 理由は必須 [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_reason_required]。
- スキップは**発行したセッションにだけ**適用される [test: crates/reviewgate/tests/session_scoped_skip.rs::a_skip_issued_by_another_session_does_not_wave_my_reviewgate_gate_through]。
- スキップは一度消費されて次の stop を許可する [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_consumed_once]。
- 発行と消費の両方がゲートログに記録される [test: crates/reviewgate/tests/prose_claims.rs::claim_skip_logged]。
- 旧来の project root の `.reviewgate-skip` ファイルは撤去した：もう stop を素通りさせない [test: crates/reviewgate/tests/session_scoped_skip.rs::an_unattributed_project_root_marker_no_longer_waves_a_reviewgate_stop_through]。
- 完全に無効化：設定で `enabled = false`、または `REVIEWGATE_DISABLE=1` [test: crates/reviewgate/tests/prose_claims.rs::claim_disable_switches]。

各判定は `<state_dir>/log.jsonl`（デフォルトは `~/.reviewgate/state/log.jsonl`）に JSONL 1 行として追記される [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::reviewer_that_prints_lgtm_is_allowed_and_clean] [test: crates/reviewgate/tests/verdict_paths_p2_p8.rs::inject_giveup_keeps_the_plain_giveup_tag]。

License: MIT [test: crates/reviewgate/tests/prose_claims.rs::claim_license_mit]
