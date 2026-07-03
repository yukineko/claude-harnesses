## north_star
verify → docker → ship のタスク生涯パイプラインを harness の実行 backbone にする。docker 実行隔離 mechanism(condukt verify launch --docker・--network=none・blastguard 前段・fail-soft docker_unavailable)は landed・validated(outcome forward 済み)。今の ONE = RUN-POLICY ゲート本体: 変更内容と cheap verify(既存 test・build・health)の『本番乖離度』から、そのタスクを verify のみ・+docker で再検証・+ship のどれに回すかを決定論で選ぶゲート。既定は cheap verify。cheap verify が本番乖離で検証しきれない時だけ docker verify へエスカレート、十分なら docker を飛ばす。迷えば Ask(human gate を残す)。判定は Rust 決定論・LLM を挟まない。ship の閉ループと SWE-bench 計測は run-policy ができてから順に parked。Devin は yardstick 参照であって目標ではない。build≠validate をシステム段で閉じる。subscription-native・LLM↔決定論分離・never-break-a-turn を崩さない。

## definition_of_done
- 決定論的な run-policy 純関数(例 decide_run_policy)が、入力(変更規模と種別・cheap verify 結果・本番乖離シグナル)から verdict(VerifyOnly | EscalateDocker | EscalateShip | AskHuman)を返すことを unit で実証。同一入力は同一 verdict(決定論)で LLM を挟まないことをテストで固定する
- 観測可能な分岐: cheap verify pass かつ乖離低の入力は VerifyOnly を返し docker を飛ばす。cheap verify では本番乖離が大きい入力は EscalateDocker を返す。閾値境界で曖昧な入力は AskHuman を返す。各分岐を最低1テストで assert する
- condukt の verify 経路に配線され、EscalateDocker verdict のときだけ既存 launch --docker container 実行が呼ばれる。docker 不在は既存 fail-soft に縮退しゲートは turn を壊さない。選ばれた verdict は JSONL に記録され後から可観測になる
- 純加算で既存 verify 経路の後方互換を破らない。fmt と clippy(-D warnings) clean、workspace 全 cargo test green

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
docker 実行隔離 mechanism は landed・validated だが『いつ docker を呼ぶか』を決める run-policy ゲートが無く、--docker は今は手動指定でしか発火しない。ゴール(変更×cheap-verify結果から verify のみ・+docker・+ship を決定論で選び迷えば Ask)との最大差分 = decide_run_policy 純関数(4-way verdict)＋condukt verify 経路への配線(EscalateDocker のときだけ launch --docker)＋分岐テスト。docker 不在は既存 fail-soft に縮退。size m(1〜2ファイル＋テスト)。

## next_action

## parked
- ship の閉ループ(PR作成→CI失敗→自動修正→green→merge。docker 実行でCI前ローカル検証してから)
- SWE-bench Verified 計測基盤(docker 隔離を実行基盤として流用)
- code RAG #4 / cross-task 学習 #7 / sandbox 大規模化は上記の後
- (運用 backlog・非戦略) daily-report を /plugin で install / condukt 0.7.0 の /plugin update→rebuild→sync ロールアウト

