## north_star
phase-6=着地済みだが休眠中の autonomy-safety 基盤を実行時に活性化＋実証する(build ≠ validate を土台層へ)。autonomy epic(ideate→implement→verify→replan を人間0介入で完走)は phase 1-5 で達成。直近の pull で並列・無人の信頼性土台(checkpoint/rollback・editgate・policy/gatelog)が CLI コア＋unit test として着地したが、editgate は hooks.json に PostToolUse 登録が無く実行時に一度も発火せず、checkpoint は SKILL から呼ばれず休眠している(policy answer のみ c166700 で配線済み=active)。phase-6 は最大レバレッジの休眠コアを実行時経路へ配線し、発火＋保護を e2e で実証して計測ループを閉じる。ONE=editgate PostToolUse hook 配線(worker のコンパイル破壊編集を同一ターンで block)。可観測化・実証は Rust 決定論側、判断は LLM。subscription-native・LLM↔決定論分離・never-break-a-turn を崩さない。sandbox #1・code RAG #4・cross-task #7・外部ベンチ #10 は yardstick 参照のみ(parked 維持)。

## definition_of_done
- hooks.json に PostToolUse(matcher: Edit|Write|MultiEdit)→`${CLAUDE_PLUGIN_ROOT}/bin/condukt editgate` が登録され、worker が live condukt worktree 内の Rust ファイルをコンパイル破壊状態に編集したとき hook が stdout に {"decision":"block","reason":<cargo check 診断>} を出す経路が実バイナリで動作する
- 実 condukt バイナリ駆動の e2e fixture(既存 unit test の再掲でなく tests/ 層)が green で以下を実証: (a)コンパイル破壊編集→block 発火(reason に診断行)、(b)正常編集→無反応(clean build は silent)、(c)worktree 外/非 Rust/空 or 不正 stdin→無反応(fallback=allow, 決して block しない)
- never-break-a-turn 不変を保持: editgate は如何なる IO/spawn 失敗でも fallback(allow)へ縮退し turn を壊さず、run_hook 契約により exit 0・panic 非伝播であることを assert
- condukt の fmt と clippy(-D warnings)が clean、workspace 全 cargo test 非回帰で pass

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
裏取り結果、ONE は size xs に縮小: (1)editgate サブコマンド(Command::Editgate, run_editgate)は main.rs に配線済み・GREEN、(2)実バイナリ駆動 e2e は pull で入った crates/condukt/tests/edit_gate_hook.rs が既にカバー(broken→block / clean→silent / 非Rust・worktree外・空stdin→allow を CARGO_BIN_EXE_condukt+隔離HOMEで検証済み=DoD#2 充足)。唯一の未実装=hooks.json の PostToolUse 登録(現状 SessionStart restore と Stop record-run の2本のみ)。したがって ONE=crates/condukt/hooks/hooks.json に PostToolUse(matcher: Edit|Write|MultiEdit)→${CLAUDE_PLUGIN_ROOT}/bin/condukt editgate を1エントリ追加し、休眠していた edit-gate を実行時に live 化する。完了確認=既存 edit_gate_hook.rs が引き続き green(fail-soft 含む)・fmt/clippy clean・workspace 非回帰。配線後 rebuild-plugins.sh --no-clean で installed 側 hooks.json/バイナリも同期。checkpoint/rollback の SKILL 配線は次点(parked)。

## next_action
editgate PostToolUse hook を hooks.json に登録し、editgate が実行時に発火して worker のコンパイル破壊編集を同一ターンで block する経路を通す + e2e で発火/沈黙/fallback/fail-soft を実証する。

## parked
- #7 cross-task 学習: fugu-router episode store をタスク層へ拡張 (backlog 8086b5d0)
- #4 code RAG(埋め込み無し版): playbook 検索を構造スコアリング・deepwiki接地・シンボル索引で強化 (backlog 32739700)
- #1 サンドボックス実行 (backlog 3cd5ed15): Docker・VM は思想と乖離・blastguard+worktree が回答——無人ホスト損傷が現実化したら再訪
- #10 外部ベンチハーネス (backlog de758e5d): フレームワーク機能でなく計測プロジェクト——公開検証フェーズで
- foundation-hygiene: rebuild-plugins.sh が hooks と config も同期 (backlog 4a499fb9)
- checkpoint/rollback を run ライフサイクルへ配線(休眠中の次点コア): phase-6 ONE(editgate)完了後の候補

