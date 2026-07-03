## north_star
phase-7=無人 run で autonomy-safety 土台が統合動作することを実証する(build != validate の総仕上げ)。phase-6 で checkpoint/rollback(a7e0d40)・editgate(13ef48a)・policy answer(c166700) を個別に活性化し各々の e2e は green だが、これらが1本の run で同時に効き互いに干渉しないことは未実証。High 権威『devin-gap の能力追加はこの土台の上に載せる』の前提=土台が本当に無人で信頼できる、を固める capstone。実 condukt バイナリを隔離 HOME で駆動し、autonomy=on の1 run 内で policy auto-agree→checkpoint→editgate block→verified→後続 fail で auto-rollback 復元→replan directive が同一 run-state/journal を共有して連鎖することを assert。可観測化・実証は Rust 決定論側、判断は LLM。subscription-native・LLM↔決定論分離・never-break-a-turn を崩さない。sandbox #1・code RAG #4・cross-task #7・外部ベンチ #10 は yardstick 参照のみ(parked 維持、capstone 実証後に土台の上へ載せる候補)。

## definition_of_done
- 実 condukt バイナリを隔離 HOME で駆動する新規 capstone e2e fixture(既存の個別 e2e とは別ファイル)が green で、1 run 内で安全スタックが統合動作することを実証する: (a)autonomy-check=on で human gate が auto へ縮退、(b)checkpoint が書かれ auto-rollback の復元対象が生じる、(c)editgate が compile 破壊 stdin payload を block する(decision=block)、(d)verified タスクの後続 fail(verified->failed)で auto-rollback が直前 checkpoint へ run-state 復元し journal に AutoRollback を残す、の各段が同一 run-state/journal を共有し互いに干渉しないことを assert
- never-break-a-turn 不変を run 全体で保持: 各段の IO/spawn 失敗はいずれも fail-soft に縮退し turn を壊さない(hook 系は exit 0・panic 非伝播)ことを assert
- capstone は既存の個別 e2e(edit_gate_hook/checkpoint_cli/replan_recovery/policy)を置き換えず追加で共存し、workspace 全 test 非回帰
- condukt fmt と clippy(-D warnings) clean、workspace 全 cargo test green

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(phase-7: 無人 run で安全スタックの統合動作を実証)と現状の最大差分は capstone e2e が存在しないこと。個別 e2e(edit_gate_hook/checkpoint_cli/replan_recovery/policy)は各機構を単独で緑にするが、autonomy=on の1 run 内で policy-auto→checkpoint→editgate block→verified→後続 fail で auto-rollback 復元→replan directive が同一 run-state/journal を共有して連鎖・互いに干渉しないことは誰も assert していない。ONE=実 condukt バイナリを隔離 HOME で駆動する新規 capstone e2e(tests/foundation_capstone_e2e.rs 等)を書き、統合動作を1本で実証する。既存 harness(env! CARGO_BIN_EXE_condukt・unique_dir・隔離 HOME)を流用。size m(新規 e2e 1ファイル・複数サブコマンド連鎖)。

## next_action

## parked

