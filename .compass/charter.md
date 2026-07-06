## north_star
flow→condukt の self-driving loop を安全に自律化する arc。決定論ゲートが routine な人間 Yes/No を置き換え、safety invariant(irreversible・high-risk・pivot・worker blocked は人間に残す / never-break-a-turn / LLM↔決定論分離 / frozen autonomy_invariant audit)は不変に保つ。landed・validated 済み: policy engine(policy decide/answer)・calibrated confidence(fugu-router)・durable escalation channel(condukt escalate)・deterministic circuit-breaker(condukt circuit check)・autonomy-check スイッチ・graded risk×reversibility classifier(blastguard)+force-gate・remove-gate(reversible+low-risk gated の承認レス auto-exec)。arc の核ゲート群は完了。今の ONE = 自律 auto-exec 路の安全面硬化: routine gated が承認レス実行になった今、その state 書き込み面(機微ファイルの 0o600 権限・PID+temp_dir 命名の衝突耐性)と env 由来の path/ref 入力(traversal/メタ文字)を決定論的に検証・封じ、auto-exec が広げた事故面を塞ぐ。ship 閉ループと SWE-bench 計測は parked のまま。build≠validate をシステム段で閉じ、subscription-native を崩さない。Devin は yardstick であって目標ではない。

## definition_of_done
- 機微な状態ファイル(tracekit span・stuckguard state ほか)が 0o600 権限で作成され、作成後の file mode が 0o600 であることを test で assert する
- temp ファイルの PID+temp_dir 命名(証拠 17 箇所)が tempfile のランダム命名に置換され、同一 PID 再利用や並行実行でファイル名が衝突しないことを test で実証する
- env 由来の path/ref(SPECGUARD_NOW は YYYY-MM-DD 形式・SPECGUARD_BASELINE_REF は git rev-parse --verify・CONTEXT_GOVERNOR_STATE_DIR は絶対パス)が検証され、traversal やシェルメタ文字を含む値が拒否されることを test で実証する
- 純加算で既存経路の後方互換を破らない。fmt と clippy(-D warnings) clean、affected crates の cargo test green、触れた各 plugin の version を3正典ファイルで lockstep bump

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(自律 auto-exec 路の安全面が硬化)と現状の最大差分 = auto-exec 化で書き込み頻度が上がった state 面がまだ (1) 既定 umask 作成で world-readable の余地(tracekit span:50-54, stuckguard:181-184 ほか), (2) PID+temp_dir 命名で衝突/PID再利用の余地(17箇所, fugu-router fingerprint:73 ほか), (3) env 由来 path/ref が無検証で traversal/メタ文字混入の余地(specguard main:998-1002・scope:106, context-governor ledger:78-88)。差分を埋める = 機微ファイルを 0o600 作成 + tempfile ランダム命名 + env の date/ref/dir を決定論検証。純加算・後方互換・各 plugin version lockstep。size は disjoint crates 並列で s/m 相当だが GoalTooBig なら 0o600 と env 検証を別 move に分割。backlog 13f38dc6 + d889d391。

## next_action
自律 auto-exec 路の安全面を硬化する: (a) 機微な状態ファイルを 0o600 で作成し PID+temp_dir 命名を tempfile 化(backlog 13f38dc6), (b) env 由来の path/ref 構成要素を検証(backlog d889d391)。両者は disjoint crates を触るので condukt が並列 schedule できる。

## parked
- ship の閉ループ(PR作成→CI失敗→自動修正→green→merge。docker 実行でCI前ローカル検証してから)
- SWE-bench Verified 計測基盤(docker 隔離を実行基盤として流用)
- code RAG / cross-task 学習 / memory-tool API / multi-judge verify
- context-governor 効率(token-reclaim truncate・per-turn 参照 dedup・groom を window-pressure aware)
- PDO discovery 拡張(north-star input metrics・dual-track discovery・stale hypothesis cadence・experiment findings)
- tree-sitter repo map / シンボル単位読み出し(xl)
- run-policy ゲート本体・remove-gate は landed・validated 済み

