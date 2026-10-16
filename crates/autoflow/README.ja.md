セッション終了時の auto-flow ゲート — `/record` を促した後は、走っている condukt run に保留タスクがある間セッションが終わるのを防ぐ **Stop** フックと、`/compact` を跨いでもループを継続させる **PreCompact** + **UserPromptSubmit** の組 [test: crates/autoflow/tests/undetermined_stop.rs::pending_run_state_still_drives_condukt] [test: crates/autoflow/tests/precompact_lock.rs::precompact_resumes_flow_only_when_this_session_holds_its_own_project_lock]。

具体的には、ターン完了時にまず一度だけ `/record`（`/session-insights:record`）を促し、続いて**現在の condukt run** の保留タスクが片付くまで `/condukt` をループで促す [test: crates/autoflow/tests/prose_claims.rs::claim_record_then_condukt_sequence]。
run-state を読めなかった場合は黙らずにブロックする [test: crates/autoflow/tests/undetermined_stop.rs::undetermined_run_state_unparseable_does_not_latch_done] [test: crates/autoflow/tests/undetermined_stop.rs::undetermined_run_state_missing_a_task_status_does_not_latch_done]。

<!-- record: 2026-08-20 rev: 5289312f -->
backlog キューは 2026-08-20 に対象外になった。それ以前は、condukt の保留が空になると backlog キューを読み、未消化アイテムがあれば `/backlog を実行してください` で毎ターン stop をブロックしていた（compass charter が陳腐化していれば代わりに `/compass` を促して撤退）。同じ要求を **SessionStart** フックが `バックログに N 件 (最優先: '…')。/flow で開始しますか？` としてセッション冒頭でも出していた。この3経路（SessionStart 提案 / Stop の backlog アーム / その compass ゲート）はユーザーの指示で撤去した。
<!-- /record -->

backlog キューは対象外であり、未消化の backlog アイテムがあっても Stop はブロックされない [test: crates/autoflow/tests/undetermined_stop.rs::a_pending_backlog_queue_no_longer_nudges]。

- **Idle**（このセッションで十分なターン数とツールイベントがある）: block → `/session-insights:record` [test: crates/autoflow/tests/prose_claims.rs::claim_idle_record_threshold]。
- **RecordRequested / Continuing**（condukt タスクがまだ保留中）: block → `/condukt` [test: crates/autoflow/tests/undetermined_stop.rs::pending_run_state_still_drives_condukt]。保留集合が縮んでいる限り継続し、回数上限は無い [test: crates/autoflow/tests/undetermined_stop.rs::pending_run_state_still_drives_condukt] [test: crates/autoflow/src/state.rs::decreasing_size_continues_forever_with_no_iteration_count_ceiling]。
- **RecordRequested / Continuing**（保留集合が `stuck_threshold` 回連続で縮まなかった）: 可視のエスカレーション [test: crates/autoflow/src/state.rs::no_progress_reaching_threshold_escalates_and_resets_streak]。
- `stuck_threshold` の既定値は 3 である [test: crates/autoflow/tests/prose_claims.rs::claim_config_defaults]。
- **RecordRequested / Continuing**（condukt の run-state を読み取れない）: 「判定不能」の理由付きで block する [test: crates/autoflow/tests/prose_claims.rs::claim_undetermined_reason_phase_untouched]。フェーズは更新しないので次の Stop で観測し直す [test: crates/autoflow/tests/prose_claims.rs::claim_undetermined_reason_phase_untouched]。
- **Done**（condukt run に保留タスクなし）: ターンの終了を許可し、backlog キューは見ない [test: crates/autoflow/tests/undetermined_stop.rs::genuinely_empty_world_still_latches_done_silently] [test: crates/autoflow/tests/undetermined_stop.rs::a_pending_backlog_queue_no_longer_nudges]。

停滞時に黙って撤退はしない [test: crates/autoflow/tests/prose_claims.rs::claim_config_defaults] [test: crates/autoflow/tests/prose_claims.rs::claim_escalation_autonomy_wording]。
エスカレーションは共有の autonomy スイッチ（`harness_core::autonomy`）から自律かどうかを読み、自律なら継続を宣言、非自律ならユーザーに確認する文言でブロックする（どちらもブロックする） [test: crates/autoflow/tests/prose_claims.rs::claim_escalation_autonomy_wording]。
エスカレーションする Stop だけは保留タスクを `running` に再マークしない [test: crates/autoflow/tests/mark_running_on_escalation.rs::escalate_stuck_does_not_re_mark_tasks_running]。

- **ユーザーの明示的な停止**（状態機械より前）: transcript 上でユーザーの直近のシグナル（入力したメッセージ、または `AskUserQuestion` への回答）が明示的な停止指示（「止めて」「stop」等）なら、*その* Stop は何もせず通す [test: crates/autoflow/tests/prose_claims.rs::claim_explicit_stop_allows_this_stop]。
- 明示的な停止は `Done` には latch しないので、ユーザーが作業を続ければ促しは再開する [test: crates/autoflow/tests/prose_claims.rs::claim_explicit_stop_not_latched]。
- 読めない・無いシグナルを停止とみなすことはない [test: crates/autoflow/src/stop_intent.rs::unreadable_transcript_path_is_false] [test: crates/autoflow/src/stop_intent.rs::empty_transcript_path_is_false] [test: crates/autoflow/tests/prose_claims.rs::claim_unreadable_transcript_not_stop]。
- **委譲記録の advisory**（RecordRequested / Continuing 中）: このセッションの transcript で `/flow` が condukt run を完了させたのに `fugu-router record` が呼ばれていなければ、セッションにつき一度だけ `fugu-router record --class flow-delegation …` を促してブロックする [test: crates/autoflow/tests/prose_claims.rs::claim_delegation_advisory_once]。

`backlog lock status` がこのプロジェクトの生きた driver を報告している間は autoflow は完全に撤退し（Stop を許可し）、稼働中の `/flow` や `/backlog` driver を二重に駆動しない [test: crates/autoflow/tests/prose_claims.rs::claim_live_driver_stands_down]。
backlog が*観測上*インストールされていない場合は撤退しない [test: crates/autoflow/tests/prose_claims.rs::claim_backlog_absent_no_standdown]。
撤退するのは生きた driver が*積極的に*報告された場合だけである [test: crates/autoflow/tests/undetermined_liveness.rs::control_live_driver_stands_down_silently] [test: crates/autoflow/tests/undetermined_liveness.rs::a1_lock_exit_nonzero_blocks]。
driver の稼働状況を判定できない場合（`backlog lock status` が非0で終了した・空または解釈不能な出力をした・自身の `undetermined` オブジェクトを返した、または plugin cache が読めない）、Stop は「判定不能」の理由付きで **block** し、フェーズは更新しない [test: crates/autoflow/tests/undetermined_liveness.rs::a1_lock_exit_nonzero_blocks] [test: crates/autoflow/tests/undetermined_liveness.rs::a2_lock_garbage_blocks] [test: crates/autoflow/tests/undetermined_liveness.rs::a3_lock_empty_blocks] [test: crates/autoflow/tests/undetermined_liveness.rs::a4_lock_undetermined_object_blocks] [test: crates/autoflow/tests/undetermined_liveness.rs::a5_unreadable_plugin_cache_blocks] [test: crates/autoflow/tests/undetermined_stop.rs::unreadable_plugin_cache_blocks_instead_of_silently_standing_down] [test: crates/autoflow/src/lock.rs::driver_active_from_status_uninterpretable_output_is_undetermined] [test: crates/autoflow/src/lock.rs::driver_active_from_status_non_object_json_is_undetermined]。
session id が無い Stop（payload にも `CLAUDE_CODE_SESSION_ID` にも無い）も同様に block する [test: crates/autoflow/tests/undetermined_liveness.rs::b_no_session_id_blocks]。
無限ブロックを避けるため、これらの判定不能 block は `stop_hook_active` で上限が付く [test: crates/autoflow/tests/undetermined_liveness.rs::c1_lock_exit_nonzero_bounded_systemmessage] [test: crates/autoflow/tests/undetermined_liveness.rs::c2_lock_garbage_bounded_systemmessage] [test: crates/autoflow/tests/undetermined_liveness.rs::c3_no_session_id_bounded_systemmessage]。
そうした block の後の再入時は stop を通すが、判定不能の原因と未解消であることを示す可視の `systemMessage` を出す（`decision` ではない） [test: crates/autoflow/tests/undetermined_liveness.rs::c1_lock_exit_nonzero_bounded_systemmessage] [test: crates/autoflow/tests/undetermined_liveness.rs::c2_lock_garbage_bounded_systemmessage] [test: crates/autoflow/tests/undetermined_liveness.rs::c3_no_session_id_bounded_systemmessage]。
ユーザーの明示的な停止は、稼働状況が判定不能でも優先される [test: crates/autoflow/tests/undetermined_liveness.rs::control_explicit_user_stop_wins_over_undetermined_liveness]。

**PreCompact** では、このセッションが `backlog lock status` 上で排他ロック保持者または登録済み driver の一つであり（＝実際にこのセッションから `/flow` ループを駆動中）、ユーザーが opt-out（`resume_flow_on_compact = false`）していなければ、resume マーカーを書き込む [test: crates/autoflow/tests/precompact_lock.rs::precompact_resumes_flow_only_when_this_session_holds_its_own_project_lock] [test: crates/autoflow/src/lock.rs::holds_lock_from_status_matches_any_registered_driver] [test: crates/autoflow/tests/prose_claims.rs::claim_precompact_opt_out_with_lock]。
PreCompact は compaction 自体をブロックすることはない [test: crates/autoflow/tests/prose_claims.rs::claim_precompact_never_blocks]。
このセッションが driver かどうかを判定できない場合（session id が無い、または `backlog lock status` が失敗・解釈不能）、PreCompact はマーカーを書かず、その旨を可視の `systemMessage` で出す [test: crates/autoflow/tests/undetermined_liveness.rs::e1_precompact_lock_exit_nonzero_visible] [test: crates/autoflow/tests/undetermined_liveness.rs::e2_precompact_lock_garbage_visible] [test: crates/autoflow/tests/undetermined_liveness.rs::e3_precompact_no_session_id_visible]。
続く **UserPromptSubmit** はそのマーカーを（あれば）消費し、「中断した /flow の loop を次の一手から再開せよ」を含む指示を一度だけ注入する [test: crates/autoflow/src/main.rs::prompt_submit_consumes_marker_once] [test: crates/autoflow/tests/prose_claims.rs::claim_prompt_submit_inject_text]。
マーカーが無い通常のターンでは常に黙る [test: crates/autoflow/src/main.rs::hooks_are_silent_without_marker_or_session]。

Stop フックは理由付きの `block` 判定を出すだけで、作業そのものを実行することはない（副作用は自身のセッション状態ファイル、通常の継続時に保留 condukt タスクを `running` にマークすること、block ごとの overwatch violation 記録のみ） [test: crates/autoflow/tests/prose_claims.rs::claim_stop_side_effects_only] [test: crates/autoflow/tests/prose_claims.rs::claim_stop_side_effects_other_blocks]。
ホスト向けバイナリが無い場合も launcher が `stop` に対して `block` を出す [test: crates/autoflow/tests/launcher_missing_binary.rs::stop_missing_binary_blocks_instead_of_silently_certifying]。
ゲートが無効化されている場合、condukt の run ファイルが存在しない場合（condukt がこのプロジェクトで走ったことが無い＝「保留タスク無し」の観測）、上記の撤退条件の場合も Stop は黙って終わる [test: crates/autoflow/tests/prose_claims.rs::claim_silent_when_disabled_or_no_run_file] [test: crates/autoflow/tests/prose_claims.rs::claim_live_driver_stands_down] [test: crates/autoflow/tests/prose_claims.rs::claim_explicit_stop_allows_this_stop]。

プラグインマーケットプレイス経由でインストールすると、同梱の `hooks/hooks.json` が **Stop**・**PreCompact**・**UserPromptSubmit** の 3 フックを `${CLAUDE_PLUGIN_ROOT}/bin/autoflow` に自動配線する [test: crates/autoflow/tests/prose_claims.rs::claim_hooks_json_wiring]。
[record: 2026-08-20 rev: 5289312f] **SessionStart** フックは撤去された。
しきい値（最小ターン数・最小ツールイベント数・停滞しきい値・resume-flow-on-compact）は config のデフォルト値から来る [test: crates/autoflow/tests/prose_claims.rs::claim_config_defaults]。ゲートはデフォルトで有効である [test: crates/autoflow/tests/prose_claims.rs::claim_config_defaults]。
上書きするには `~/.autoflow/config.toml` を書く [test: crates/autoflow/tests/prose_claims.rs::claim_config_toml_override]:

```toml
enabled = true                # 既定 true [test: crates/autoflow/tests/prose_claims.rs::claim_config_defaults]
min_turns = 2                 # /record を促すまでのターン数 [test: crates/autoflow/tests/prose_claims.rs::claim_idle_record_threshold]
min_tool_events = 3           # /record を促すまでのツールイベント数 [test: crates/autoflow/tests/prose_claims.rs::claim_idle_record_threshold]
stuck_threshold = 3           # 可視エスカレーションまでの無進捗 Stop 回数 [test: crates/autoflow/tests/prose_claims.rs::claim_config_toml_override]
resume_flow_on_compact = true
state_dir = "~/.autoflow/state"
```

パースできない config ファイルは無視される（既定値が使われる） [test: crates/autoflow/tests/prose_claims.rs::claim_unparseable_config_defaults]。

```sh
cargo install --path .
autoflow stop            # Stop フック: record→condukt の状態機械を実行 [test: crates/autoflow/tests/prose_claims.rs::claim_record_then_condukt_sequence]
autoflow pre-compact     # PreCompact フック: このセッションがロックを保持していれば resume マーカーを書く [test: crates/autoflow/tests/precompact_lock.rs::precompact_resumes_flow_only_when_this_session_holds_its_own_project_lock]
autoflow prompt-submit   # UserPromptSubmit フック: マーカーを消費して「中断した /flow の loop を次の一手から再開せよ」を含む指示を一度だけ注入 [test: crates/autoflow/tests/prose_claims.rs::claim_prompt_submit_inject_text]
```

`autoflow stop` は stdin でフック JSON を読み、`block` 判定を出力する（または何も出力しない） [test: crates/autoflow/tests/undetermined_stop.rs::pending_run_state_still_drives_condukt] [test: crates/autoflow/tests/integration.rs::valid_stop_payload_with_no_metrics_is_silent]。
`autoflow pre-compact` と `autoflow prompt-submit` は resume マーカーのゲートが成立しない限り黙る [test: crates/autoflow/src/main.rs::hooks_are_silent_without_marker_or_session] [test: crates/autoflow/src/main.rs::precompact_writes_no_marker_without_a_held_lock] [test: crates/autoflow/tests/prose_claims.rs::claim_precompact_prompt_submit_cli_silent] [test: crates/autoflow/tests/undetermined_liveness.rs::control_precompact_lock_none_is_silent_no_marker]。
例外として、`pre-compact` はゲートを判定できなかった場合に可視の `systemMessage` を出す [test: crates/autoflow/tests/undetermined_liveness.rs::e1_precompact_lock_exit_nonzero_visible] [test: crates/autoflow/tests/undetermined_liveness.rs::e2_precompact_lock_garbage_visible] [test: crates/autoflow/tests/undetermined_liveness.rs::e3_precompact_no_session_id_visible]。
`AUTOFLOW_DISABLE=1` でゲートを無効化できる [test: crates/autoflow/tests/prose_claims.rs::claim_disable_env]。

```sh
cargo test
```

コミットされているのは `bin/autoflow` launcher だけである [test: crates/autoflow/tests/prose_claims.rs::claim_only_launcher_committed]。
これがプラットフォーム別の `bin/autoflow-<os>-<arch>` を exec する [test: crates/autoflow/tests/launcher_missing_binary.rs::control_launcher_still_execs_the_host_binary_when_it_is_present]。
後者は gitignore されている [test: crates/autoflow/tests/prose_claims.rs::claim_only_launcher_committed]。
フックが依存する挙動を変えたときは rollout をやり直すこと。
