## north_star
fail-open を『後から検出する』のをやめ、『そもそも書けない』側へ移す。harness_core の verdict 型は出力側でこれを既に達成している — Clean は private witness でしか作れず、Determination には unwrap_or も ok も Default も無いので、『判定できなかった』を『問題なし』へ潰す最短経路が型として存在しない。同じ技法を入力側（fallible な入力境界）と gate 内部の verdict 経路へ広げる。

## definition_of_done
- DoD1: ワークスペース直下の Cargo.toml が clippy lints を持ち gate crate が継承する。達成済み（6 crate が workspace 継承、非集約の4 crate は理由をコード内に明文化して parked）。
- DoD2: enforce の置き場所は local である（clippy ゲートを required status check にしない）。達成済み（CLAUDE.md 第7節どおり local pre-commit 配線）。
- DoD3: harness_core が fallible な入力境界のラッパを提供し返り値が三値 Determination である。達成済み（boundary モジュール、走査・読み出し・subprocess の3経路）。
- DoD4: gate crate 内の生の走査・読み出し・subprocess 直接呼び出しが機械検出され、検出時に local ゲートが非0で終了する。達成済み（raw-io ratchet を pre-commit 配線、terminal method 検出に再設計済み）。
- DoD5: アンチ空虚の対照実験を記録している。達成済み（ratchet の対照実験 29 tests green）。
- DoD6: 生 IO 呼び出しの baseline が boundary ラッパ経由へ移行され単調減少する — この軸は converged=達成として固定。現況 baseline=60（実測 2026-07-26 HEAD 2742a8c1、`python3 scripts/check-raw-io-ratchet.py` が count==baseline=60、floor held を報告）。remaining sites は Bucket-A（既に fail-closed・機械的）／Bucket-B（documented fail-soft contract・contested）で、clean な raw-IO fail-open は残っていない。以後の勾配は raw-IO ではなく DoD9（意味的 verdict 経路）へ移す。
- DoD7: fail-open reader（Result 崩壊を permissive default へ潰す入力読み取り）が三値 Determination へ移行され確定数が単調増加する。既知集合 M1-M4 は 4/4 確定で全て main に landed。各確定は利害のない agent の RED oracle を fault injection で先に観測してから GREEN 化する（F→P）。新たな fail-open reader を発見したら既知集合に追加する。
- DoD8: fail-open reader は raw-IO ratchet に現れない意味的 Result 崩壊も含むため、可能な箇所は boundary 経由へ寄せて DoD6 と同時前進させる。
- DoD9: 各 gate crate の verdict 経路（block/allow/ask 等の判定を返す関数、およびそれを消費する call site）が三値（harness_core::verdict::Determination / Verdict）で表現され、silence（沈黙した subagent 票）・空集合・panic・IO/parse/subprocess 失敗を restrictive（block/ask/escalate）へ解決する。per-gate 監査で『未監査の verdict 経路 0本』を各 gate crate の完了条件とし、監査済み gate crate 数 / 全 gate crate 数を単調増加の計測単位とする。各確定は利害のない agent が fault injection で RED（gate が誤って pass/allow）を先に観測してから GREEN（restrictive 解決）で確定（F→P）。発見した意味的 fail-open を既知集合に追加する。本セッションで cde2212c（mutategate floorless-clamp）・dd3aad81（condukt silent-verifier）・ea1355f5（blastguard cwd-bypass）が landed 済み。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト（各確定は利害のない agent の RED oracle を fault injection で先に観測してから GREEN 化して確定＝F→P。build≠validate）

## current_gap
DoD6 の raw-IO ratchet は baseline=60 で converged（実測 2026-07-26、count==baseline、floor held）＝この軸は達成固定。DoD7 M1-M4 も 4/4 達成。残る勾配は DoD9: gate-crate 内部の意味的 fail-open（verdict 経路が silence／空集合／panic／IO・parse 失敗を permissive へ潰す class）の per-gate fail-closed 監査。verdict/boundary 未採用は schemaguard(0)・autoflow(0)、低採用は budgetguard／gauge／reviewgate(各1 file)。次の右サイズ一手は未監査 gate 1本の verdict 経路 fail-closed 監査（schemaguard を先頭候補）。

## next_action
schemaguard の verdict 経路を per-gate fail-closed 監査する: schema 検証（decomposition 等）が『schema／入力を読めない・parse 不能・空』を『valid』へ潰していないか、fallible 入力（IO/parse/subprocess）ごとに restrictive 解決を確認。利害のない agent が unreadable/malformed/empty schema を注入して gate が誤って pass することを先に観測(RED)→三値 Determination 経由へ移行し fail-closed(GREEN)で確定(F→P)。cargo test と clippy green 観測、schemaguard version を3ファイル lockstep bump。※実行 handoff（condukt）は d52 の排他ロック解放後。今サイクルは charter 彫りのみ。

## parked

