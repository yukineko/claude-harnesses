## north_star
flow→condukt の self-driving loop を安全に自律化する arc。決定論ゲートが routine な人間 Yes/No を置き換え、safety invariant(irreversible・high-risk・pivot・worker blocked は人間に残す / never-break-a-turn / LLM↔決定論分離 / frozen autonomy_invariant audit)は不変に保つ。landed・validated 済み: policy engine(policy decide + policy answer shim)・calibrated confidence(fugu-router)・durable escalation channel(condukt escalate)・deterministic circuit-breaker(condukt circuit check)・autonomy-check スイッチ・graded risk×reversibility classifier(blastguard)+force-gate。今の ONE = 最後の remove-gate: 現状は全 class:gated タスクが自律モードでも人間承認を要するが、reversible かつ low-risk と分類された gated タスクだけを policy=auto で承認レス実行(実行前 checkpoint + 決定を journal 記録で可観測)に開く。irreversible または high-risk gated は従来どおり escalate。ship 閉ループと SWE-bench 計測は parked のまま。build≠validate をシステム段で閉じ、subscription-native を崩さない。Devin は yardstick であって目標ではない。

## definition_of_done
- reversible かつ low-risk と分類された gated タスクが policy=auto のとき、人間プロンプト無しで実行されるが、実行前に checkpoint が取られ決定が append-only JSONL journal に記録されることを test で実証する
- irreversible または high-risk と分類された gated タスクは従来どおり escalate し、人間の停止が保持されることを test で実証する
- 分類から auto か escalate かの判定は決定論(同一分類は同一判定、LLM を挟まない)で、frozen autonomy_invariant audit が green のまま
- 純加算で既存 verify と policy と gate 経路の後方互換を破らない。fmt と clippy(-D warnings) clean、workspace 全 cargo test green

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
全 class:gated タスクが自律モードでも人間承認を要する(condukt SKILL の gated carve-out ＝ Phase 3/8 で必ず人間へ)。remove-gate の deps は全て landed 済み: graded risk×reversibility classifier(blastguard)・policy engine(policy decide/answer)・実行前 checkpoint rollback・circuit-breaker。ゴール(reversible かつ low-risk と分類された gated タスクだけ policy=auto で承認レス実行し、irreversible/high-risk は escalate 維持)との最大差分 = 分類を gated 判定に配線 + reversible+low-risk のみ checkpoint と journal 付きで auto 実行する分岐 + escalate 経路の保持 + 分岐テスト(auto 実行/escalate 保持を各 assert)。純加算・frozen autonomy_invariant 不変。size m。backlog 1059481c。

## next_action

## parked
- ship の閉ループ(PR作成→CI失敗→自動修正→green→merge。docker 実行でCI前ローカル検証してから)
- SWE-bench Verified 計測基盤(docker 隔離を実行基盤として流用)
- code RAG / cross-task 学習 / memory-tool API / multi-judge verify
- (security backlog) state-file 0o600・env path と ref 検証・tempfile 命名 は autonomy remove-gate の後
- run-policy ゲート本体は landed・validated 済み(outcome #21 forward)

