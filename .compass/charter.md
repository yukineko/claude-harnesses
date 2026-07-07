## north_star
phase-9 cross-task 学習の第4スライス = retrieval 側の観測可能化 (epic 計測の instrumentation を両側そろえる)。write→retrieve→capture観測 (slice-1 retrieve + slice-2 capture + slice-3 capture-rate stats) は landed 済。今の ONE = capture 側 (slice-3 `condukt lessons stats`) と対称に、教訓が実際に retrieve/注入された run を決定論コードで記録し retrieval hit rate を機械観測できるようにする: (1) 決定論 retrieval-event ledger (harness_core, append-only JSONL, run_id で冪等, fail-soft), (2) `condukt lessons record-retrieval` が決定論 search→ledger 冪等記録→注入用 lessons_context を emit, (3) `condukt lessons stats` に retrieval セクションを additive 追加 (hit rate = hits/total が counts から算出可能), (4) SKILL Phase-1 の注入経路を record-retrieval 経由に差し替えて live 注入を観測。集計・記録・冪等・境界は決定論コード (AI 非依存 = determinism-in-code)、意味判断 (教訓文) は不変で LLM。空ストア/バイナリ不在は fail-soft (空 context・非0終了しない・注入形不変=後方互換)。純加算・subscription-native (API 無し・bundled bin・lexical/集計のみ)。安全 invariant (never-break-a-turn・untrusted 注入は境界隔離) は不変。これで capture(書いた) と retrieval(使った) の両側 instrumentation が揃い、epic 全体 (類似タスクの修正サイクル削減) の A/B 計測の前提が満たされる。epic の fix-cycle 削減計測そのものは後続 (データ蓄積後の measure) へ parked。

## definition_of_done
- 決定論的な retrieval-event ledger を追加する (harness_core::lessons 近傍の純粋な append-only JSONL store, run_id で冪等 = 1 run 1 retrieval イベント・再記録は no-op, load は fail-soft で missing→空 Vec・corrupt 行 skip・never panic)。RetrievalEvent{run_id, query_summary, hit:bool, lesson_ids:[String], k, ts} を append/load できる
- condukt lessons record-retrieval --run <RID> --query <q> [--k N] が決定論 lessons::search を走らせ、hit=検索非空 を ledger に冪等記録した上で、注入用 lessons_context JSON (検索ヒット) を stdout に emit する。空ストア/検索ゼロヒットでも fail-soft (空 context を出し exit 0・非0終了しない)。同一 run の再記録は ledger 件数を増やさない (冪等)
- retrieval hit rate が機械観測できる: condukt lessons stats の出力に retrieval:{total:int, hits:int, distinct_runs:int} セクションを additive に加える (既存 total/by_kind/source_runs/recent は不変・後方互換)。集計は純関数 (例 harness_core の retrieval_stats(&[RetrievalEvent])->_) で AI を呼ばず決定論に計算し、unit test で seeded events から total/hits/distinct が期待どおり出ることを実証 (hit rate = hits/total が counts から算出可能) し cargo test green
- SKILL Phase-1 の lessons_context 注入経路を record-retrieval 経由に差し替え、live 注入が ledger に残るようにする (現状 line 206 の bare `fugu-router lessons search` を `condukt lessons record-retrieval` に置換)。condukt/fugu-router 不在時は従来どおり no-op で lessons_context を一切出さない (後方互換・注入形不変・untrusted 境界隔離は維持)
- 純加算で既存挙動を温存: fmt と clippy(-D warnings) clean、触った crate の cargo test green、condukt を lockstep micro bump (SKILL + 新 subcommand を触るため Cargo.toml + plugin.json + marketplace.json + Cargo.lock。0.7.30→0.7.31。harness_core に純関数+store を足す場合は build-time lib なので bump 不要)。既存の lessons store / harvest / stats(capture 側の total/by_kind/source_runs/recent) / Phase-8 capture は untouched(純加算)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(教訓が実際に注入された run が記録され、retrieval hit rate=何割の run が非空 lessons_context を得たかが機械観測でき、capture(slice-3)と対称に epic fix-cycle 計測の両側 instrumentation が揃う) − 現状 の最大差分: retrieve 側は稼働(SKILL Phase-1 が fugu-router lessons search で lessons_context を注入)しているが完全に ephemeral — どの run にどの教訓が注入されたか・全 run の何割がヒットしたか(retrieval hit rate)を見る手段が皆無(証拠: condukt/src に retrieval 記録ゼロ; SKILL Phase-1 line 206 は bare fugu-router lessons search で何も記録しない; condukt lessons は harvest/stats のみ, fugu-router lessons は add/search のみ)。差分を埋める最小 slice = (1)決定論 retrieval-event ledger(run_id 冪等・fail-soft) + (2)condukt lessons record-retrieval(search→冪等記録→lessons_context emit) + (3)condukt lessons stats に retrieval:{total,hits,distinct_runs} を additive 追加 + (4)SKILL Phase-1 を record-retrieval 経由に差し替え。集計/記録/冪等は純関数=AI 非依存、空ストア fail-soft。size m。これで capture(書いた)と retrieval(使った)の両側が観測でき epic 計測の前提が満たされる。fix-cycle 削減計測そのものはデータ蓄積後の後続スライスへ parked。

## next_action

## parked

