## north_star
phase-6=着地済みだが休眠中の autonomy-safety 基盤を実行時に活性化＋実証する(build != validate を土台層へ)。autonomy epic(ideate→implement→verify→replan を人間0介入で完走)は phase 1-5 で達成。休眠コアのうち policy answer(c166700)・editgate PostToolUse hook(13ef48a) は活性化済み。残る休眠コア=checkpoint/rollback: CLI(state checkpoint/rollback)・auto-rollback 経路(main.rs で verified→failed 遷移時に latest_checkpoint へ自動復元, JournalKind::AutoRollback, fail-soft)・e2e(tests/checkpoint_cli.rs) はいずれも着地済み GREEN。だが SKILL.md がライフサイクルのどこでも state checkpoint を呼ばないため、実 run 中に checkpoint が一度も書かれず auto-rollback net は latest_checkpoint=None で常に no-op(=休眠)。phase-6 の次 ONE=SKILL の run ライフサイクル境界に state checkpoint を配線し、既にテスト済みの auto-rollback セーフティネットを実行時に機能させる(可逆な無人続行の土台)。可観測化・実証は Rust 決定論側、判断は LLM。subscription-native・LLM↔決定論分離・never-break-a-turn を崩さない。sandbox #1・code RAG #4・cross-task #7・外部ベンチ #10 は yardstick 参照のみ(parked 維持)。

## definition_of_done
- condukt SKILL.md が run ライフサイクル境界(タスクが verified に遷移した直後=auto-rollback の復元対象が生じる点、および Phase 4.5 ベースライン後の初期 checkpoint)で `condukt state checkpoint --run $RID` を呼ぶよう配線される(grep で SKILL に checkpoint 呼び出しが存在=休眠 write 側の活性化)
- 実 condukt バイナリ駆動の e2e が green で『checkpoint 書き込み後、verified タスクの後続タスク fail→auto-rollback で直前 checkpoint へ run-state 復元＋journal に AutoRollback 記録』を実証(tests/checkpoint_cli.rs の verified_task_that_fails_auto_rolls_back_and_journals が非回帰で pass、必要なら SKILL 配線点を反映する assert を追補)
- never-break-a-turn 不変を保持: checkpoint 書き込み失敗・restore 失敗はいずれも log+skip に縮退し turn を壊さない(restore_checkpoint の fail-soft 経路を assert)
- condukt の fmt と clippy(-D warnings) clean、workspace 全 cargo test 非回帰で pass

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(phase-6: 休眠 autonomy-safety コアの実行時活性化)と現状の最大差分は checkpoint/rollback の write 側配線 1 点。CLI(state checkpoint/rollback)・auto-rollback 経路(main.rs:1421 verified→failed→latest_checkpoint 復元)・e2e(checkpoint_cli.rs 2 tests green) は着地済みだが、SKILL.md が run 中に state checkpoint を一度も呼ばないため checkpoint が書かれず auto-rollback net は latest_checkpoint=None で常に no-op。ONE=condukt SKILL.md の Phase 6(タスク verified 遷移直後)＋Phase 4.5(baseline 後の初期 checkpoint)に `condukt state checkpoint --run $RID` を配線し、既にテスト済みの auto-rollback セーフティネットを実行時に機能させる。editgate と同型(コア+e2e 着地済み→残りは配線のみ)で size は m ではなく s。

## next_action

## parked

