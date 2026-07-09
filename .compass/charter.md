## north_star
cross-task 学習層(lessons store の write→retrieve)が実際に修正サイクルを減らすかを『計測可能』にし、hypothesis 7100ac7c を証拠付きで validate/reject して閉じられる状態にする。build(学習層)は既に landed 済み。残る差分は計測の glue: replan 回数(condukt {run_id}.replan-log.jsonl の ReplanLogRecord.replan_count)と lessons-retrieval hit(harness-core retrieval.jsonl の RetrievalEvent{run_id,hit})は両方すでに run_id キーで永続化されているのに、両者を join して mean_replan_reduction_ratio = 1 - (mean_replan_of_hit_runs / mean_replan_of_nonhit_runs) を算出する層が存在しない。この決定論の集計層(新 learning_signal.rs + `condukt learning-signal` CLI)を純加算で足せば、データ蓄積とともに 7100ac7c が measure step で閉じられる。

## definition_of_done
- condukt に learning-signal 集計層を追加する: 新モジュール crates/condukt/src/learning_signal.rs が、各 run について {run_id}.replan-log.jsonl を load_replan_records で読み replan_count を run 単位で合計し(per_run_replan_total)、harness-core の retrieval ledger(retrieval::load)から run_id ごとの hit フラグを取り、共通 run_id キーで join して hit 群/miss 群に分ける純関数を持つ。決定論・fail-soft(store 欠如/空/破損は空集合に縮退)
- mean_replan_reduction_ratio = 1 - (mean_replan_of_hit_runs / mean_replan_of_nonhit_runs) を算出する: hit 群と miss 群それぞれの mean replan を出し ratio を返す。片側 sample が 0 件・分母 0・全体空などの縁ケースは panic せず {ratio:null, hit_sample_size, miss_sample_size} のように安全に表現する(0 除算しない)
- `condukt learning-signal` CLI サブコマンドを追加し、上記純関数の結果を JSON で emit する: {metric:'mean_replan_reduction_ratio', ratio, numerator_mean(hit), denominator_mean(miss), hit_sample_size, miss_sample_size}。grep で main.rs に learning-signal アーム と mod learning_signal 宣言が確認できる
- 決定論テストを追加する: 合成した replan-log JSONL + retrieval ledger(既知の hit/miss × 既知の replan 分布)を隔離 store に書いて食わせ、期待 ratio を assert する。縁ケース(空 store / hit だけ or miss だけ / 単一 run / 分母0)も別テストで検証。既存の replan/retrieval/lessons テストは非回帰
- 純加算・後方互換: condukt を micro lockstep bump(0.7.36→0.7.37)で4正典(Cargo.toml / .claude-plugin/plugin.json / .claude-plugin/marketplace.json の condukt エントリ / Cargo.lock)同時。既存 replan 記録・retrieval ledger・lessons ロジックは untouched。python3 scripts/check-plugin-versions.py と python3 scripts/check-version-bumped.py が green、cargo fmt と cargo clippy -p condukt --all-targets が -D warnings で clean、cargo test -p condukt green
- 検証(build≠validate): 別モデルで独立に、learning_signal.rs の join キー(run_id)・hit/miss 平均・ratio 式・縁ケースの安全性・後方互換(既存ロジック不変)・全 gate green を再確認する。注意: この ONE は 7100ac7c を『計測可能にする』ところまで。実際の validate/reject は lessons が十分蓄積し hit/miss 両群が揃ってからの measure step で行う(このスコープ外)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(cross-task学習の効果が計測可能で 7100ac7c を証拠付きで閉じられる) − 現状 の最大差分: 学習層(lessons write→retrieve)は landed 済み・両計測信号も既に run_id キーで永続化されている(replan_count は condukt の {run_id}.replan-log.jsonl / lessons-hit は harness-core の retrieval.jsonl)が、両者を join して mean_replan_reduction_ratio を出す集計層が皆無で、7100ac7c は「データが無い」のではなく「計測手段が無い」状態。差分を埋める最小 slice(size m・純加算) = condukt に新 learning_signal.rs(run 単位で replan_count 合計 × retrieval hit を run_id で join し hit/miss 群の mean replan から ratio 算出、縁ケース fail-soft)と learning-signal CLI サブコマンドを追加、決定論テストで既知分布から期待 ratio を assert、condukt を micro lockstep bump。harness-core/tracekit/hypothesis のコア改修は不要(既存 CLI と ledger をそのまま読むだけ)。これで蓄積が進めば measure step が 7100ac7c を閉じられる。

## next_action

## parked

