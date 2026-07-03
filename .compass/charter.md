## north_star
verify → docker実行 → ship のタスク生涯パイプラインを harness の実行 backbone にする(安全な無人オーケストレーションの土台=phase-6活性化＋phase-7 capstone は proven なので、その上に順番に載せる)。RUN-POLICY(設計の核・graded escalation): 既定は cheap verify(既存の test/build/health)。cheap verify が本番環境との乖離が大きく検証しきれない場合だけ docker でも検証へエスカレート、十分なら docker を飛ばして次へ。最後は必ず ship。エスカレート判断に迷うときは Ask(human gate を残す)。stage は verify→docker→ship の順で実装する。今の ONE(最初の欠けている stage)=docker 実行隔離の mechanism: 現状の worktree 隔離・verifier・condukt verify launch(ビルド/health, ホストで直接 spawn=container 隔離なし)の上に、生成コード/テストを container 内で走らせる真の実行隔離を足す(policy から条件付きで呼ばれる決定論ラッパー)。Devin は yardstick 参照であって目標ではない。build≠validate をシステム段で閉じる。subscription-native・LLM↔決定論分離・never-break-a-turn を崩さない。run-policy ゲート実装・ship(PR/CI)ループ・SWE-bench 計測は docker mechanism ができてから順に parked。

## definition_of_done
- 決定論的な docker 隔離実行ラッパー(新規: condukt verify launch の container バックエンド、または新 crate/サブコマンド)が、与えられた1コマンドを container 内で実行し、構造化 verdict(exit code・stdout/stderr digest・pass/fail)を返すことを e2e で green に実証する。実行はホスト FS/network から隔離(既定 network なし・作業 dir のみ mount)
- 観測可能: benign なコマンド(例 `true`)は container 内で走り verdict=pass、失敗するコマンド(例 `false`)は verdict=fail。exit code と captured 出力が verdict に反映されることを assert
- fail-soft(never-break-a-turn): docker 不在・daemon 不通・spawn 失敗・timeout はいずれも turn を壊さず明示 verdict(docker_unavailable / timeout の note 付き passed:false)に決定論的に縮退する。危険コマンドは spawn 前に既存 blastguard で検証し container にも渡さない
- 実行対象コマンドは untrusted 前提で container 隔離(FS/network 境界)。判定は Rust 決定論側で純機械、LLM を挟まない。fmt と clippy(-D warnings) clean、workspace 全 cargo test green

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(verify→docker→ship pipeline。docker verify は cheap verify が本番乖離で検証しきれない時だけ・迷えばAsk)と現状の最大差分 = docker 実行隔離の mechanism が無い。現状 condukt verify launch はホストで直接 sh -c spawn し container 隔離が無いので、生成コードの実行時挙動を本番相当の隔離下で検証できず ship の前提も欠く。ONE = 1コマンドを docker 隔離(既定 network なし・作業dirのみmount)で実行し構造化 verdict を返す決定論ラッパー(既存 verify launch の container backend が自然)。docker 不在は fail-soft。size m(1〜2ファイル＋e2e)。

## next_action

## parked
- run-policy ゲート本体(変更内容/cheap verify の本番乖離度から『verify のみ / +docker / +ship』を決定論で選び、迷えば Ask。docker mechanism ができてから)
- ship(PR作成→CI失敗→自動修正→green→merge)の閉ループ(docker 実行でCI前ローカル検証してから)
- SWE-bench Verified 計測基盤(docker 隔離を実行基盤として流用)
- code RAG #4 / cross-task 学習 #7 / sandbox 大規模化は上記の後

