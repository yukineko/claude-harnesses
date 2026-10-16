- `ToolResultGroomer` は要約文を書かない：予算超過のツール結果の先頭と末尾を逐語のまま残し、中間だけを省略マーカー `…[context-governor: elided N chars of TOTAL]…` に置き換える [test: crates/context-governor/tests/prose_claims.rs::claim_groomer_is_elision_not_summary]。
- `Pinned` / `Verbatim` のアイテムからは `Evictable` トークンを生成できず、生成できるのは `Lane::Evictable` のアイテムだけである [test: crates/context-governor/tests/context_governor_hooks.rs::evictable_token_only_from_evictable_lane]。
- `Stop` と `SubagentStop` はそれぞれ厳密に `{}` を出力して exit 0 する [test: crates/context-governor/tests/prose_claims.rs::claim_subagent_stop_never_blocks]。

hook-dispatch バイナリは stdin でフックペイロードを受け取り、JSON エンベロープを stdout に書く [test: crates/context-governor/tests/context_governor_coexistence_with_ctxrot.rs::post_tool_use_emits_valid_envelope_and_exits_zero]。
`hooks/hooks.json` はこの 1 つのバイナリを `PostToolUse`（マッチャ `Read|Bash|Grep|Glob|WebFetch|BashOutput|NotebookRead|Task`）・`UserPromptSubmit`・`SessionStart`・`PreCompact`・`Stop`・`SubagentStop` に登録している [test: crates/context-governor/tests/prose_claims.rs::claim_hooks_json_registration]。
有効なペイロードに対し、`PostToolUse` / `UserPromptSubmit` / `SessionStart` / `Stop` / `SubagentStop` はいずれも stdout に JSON オブジェクトを出して exit 0 する [test: crates/context-governor/tests/prose_claims.rs::claim_non_compact_events_exit_zero_with_envelope]。

- `ToolResultGroomer` は、トークン予算を超えたツール結果を、省略マーカー付きの厳密により小さい本文に置き換える [test: crates/context-governor/src/defaults/groomer.rs::over_budget_trims_strictly_smaller]。
- `ToolResultGroomer` は、トークン予算内のツール結果には手を加えない [test: crates/context-governor/src/defaults/groomer.rs::under_budget_passes_through]。
- `ContextInjector` は `CONTEXT_GOVERNOR_REFERENCE_DOC` が指す参照ドキュメントを読み、見出しに一致するプロンプトに対して `additionalContext` エンベロープを返す [test: crates/context-governor/tests/context_governor_hooks.rs::injector_reads_reference_doc_from_env_and_injects]。
- `StateRehydrator` は保存済みスナップショットの pinned セクションを常駐コンテンツとして再注入する [test: crates/context-governor/src/defaults/rehydrator.rs::pinned_and_verbatim_sections_reinjected_as_resident]。
- 同梱の `SessionStart` マッチャは `compact` を含み [test: crates/context-governor/tests/backlog_23869ff7.rs::session_start_matcher_covers_compact]、そのマッチャ経由の `compact` SessionStart はピンを再注入する [test: crates/context-governor/tests/backlog_565fb2a8.rs::compact_session_start_through_shipped_matcher_rehydrates_the_pin]。
- `Checkpointer` は、トークン使用量が閾値を超える実在のトランスクリプトを与えられたときスナップショットを記録する [test: crates/context-governor/src/defaults/checkpointer.rs::checkpointer_emits_snapshotted_row_iff_real_snapshot]。

`ToolResultGroomer`・`ContextInjector`・`CompactionGuard` はそれぞれ台帳に 1 行の JSONL を追記し、各行は数値の `saved_tokens` と `resident_tokens` を持つ [test: crates/context-governor/tests/context_governor_hooks.rs::action_ledger_records_groom_inject_snapshot_and_rollup_sums]。

- 台帳ファイルは `<state_dir>/<project_key>/<session>/ledger.jsonl` である（`<state_dir>` は `CONTEXT_GOVERNOR_STATE_DIR`、`<session>` は `CLAUDE_CODE_SESSION_ID` から導かれる） [test: crates/context-governor/tests/prose_claims.rs::claim_ledger_path_under_state_dir]。
- groom の行の `event` は `groomed`、inject の行は `injected`、snapshot の行は `snapshotted` である [test: crates/context-governor/tests/context_governor_hooks.rs::action_ledger_records_groom_inject_snapshot_and_rollup_sums]。
- `context-governor rollup` は台帳から `rows`・`total_saved_tokens`・イベント別件数を出力し、2 回実行しても出力は同一で、台帳ファイルを変更しない [test: crates/context-governor/tests/prose_claims.rs::claim_rollup_read_only_deterministic]。
