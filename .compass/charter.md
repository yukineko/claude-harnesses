## north_star
phase-9 cross-task 学習の第3スライス = capture rate の観測可能化 (epic 計測の instrumentation 前提)。write→retrieve 往復 (slice-1 retrieve + slice-2 capture) は landed 済。今の ONE = cross-project lessons store に対する決定論 read-only 集計コマンド condukt lessons stats を1本通す: store を読み {total, by_kind:{error-pattern,convention}, source_runs(distinct), recent:[task_summary...]} を JSON で emit する。集計は純関数 (AI 呼ばない=determinism-in-code) で、これにより『verified run のうち何件が教訓を capture したか (capture rate = distinct source_runs)』が機械観測でき、epic 全体(類似タスクの修正サイクル削減)の計測の土台になる。空 store・store 不在は fail-soft(空集計・非0終了しない)。純加算・subscription-native(API 無し・bundled bin・lexical/集計のみ)・後方互換。安全 invariant(意味判断は LLM・never-break-a-turn)は不変。epic 全体の fix-cycle 削減計測は後続スライスへ parked。

## definition_of_done
- condukt lessons stats (run 引数不要) が cross-project lessons store(harness_core::lessons, LESSONS_STORE_DIR override 尊重)を読み、集計 JSON を stdout に emit する: {total:int, by_kind:{"error-pattern":int,"convention":int}, source_runs:int(distinct source_run 数), recent:[直近 N 件の task_summary]}。空 store・store 不在は {total:0,by_kind:{},source_runs:0,recent:[]} を出し exit 0(fail-soft・非0終了しない)
- 集計は純関数(例 harness_core::lessons::stats(&[Lesson])->Stats もしくは condukt-local な純関数)で決定論に計算し、AI を呼ばない。unit test で seeded lessons スライスから total/by_kind/distinct source_runs/recent が期待どおり出ることを実証し cargo test green
- capture rate(= distinct source_runs)が機械観測できることを示す: test もしくは実行例で、複数 run 由来の教訓を追加すると source_runs がその distinct 数に一致することを確認する
- 純加算で既存挙動を温存: fmt と clippy(-D warnings) clean、触った crate の cargo test green、condukt を lockstep micro bump(新 subcommand を触るため Cargo.toml + plugin.json + marketplace.json + Cargo.lock。harness_core に純関数を足す場合は build-time lib なので bump 不要)。既存の lessons store / harvest / Phase-1 注入 / Phase-8 capture は untouched(純加算)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(verified run のうち何件が教訓を capture したかが機械観測でき、epic の fix-cycle 削減計測の土台ができる) − 現状 の最大差分: write→retrieve 経路(slice-1 retrieve + slice-2 capture)は landed だが、lessons store の中身を観測する手段が皆無 — capture が実際に起きているか・kind 分布・何 run 分の教訓が溜まったか(capture rate)を見る read-only コマンドが無い(証拠: fugu-router lessons は add/search のみ, condukt lessons は harvest のみで stats/count 無し)。差分を埋める最小 slice = cross-project store を読み {total, by_kind, source_runs(distinct), recent} を JSON で出す決定論 read-only 集計 condukt lessons stats(集計は純関数=AI 非依存, 空 store は fail-soft)。size xs/s。これで capture rate が観測可能になり epic 計測の instrumentation 前提が満たされる。epic 全体の fix-cycle 削減計測は parked。

## next_action

## parked

