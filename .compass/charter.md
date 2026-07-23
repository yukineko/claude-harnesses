## north_star
fail-open を『後から検出する』のをやめ、『そもそも書けない』側へ移す。harness_core の verdict 型は出力側でこれを既に達成している — Clean は private witness でしか作れず、Determination には unwrap_or も ok も Default も無いので、『判定できなかった』を『問題なし』へ潰す最短経路が型として存在しない。同じ技法を入力側（fallible な入力境界）へ広げる。

## definition_of_done
- DoD1: ワークスペース直下の Cargo.toml が clippy lints を持ち gate crate が継承する。達成済み（6 crate が workspace 継承、非集約の4 crate は理由をコード内に明文化して parked）。
- DoD2: enforce の置き場所は local である（clippy ゲートを required status check にしない）。達成済み（CLAUDE.md 第7節どおり local pre-commit 配線）。
- DoD3: harness_core が fallible な入力境界のラッパを提供し返り値が三値 Determination である。達成済み（boundary モジュール、走査・読み出し・subprocess の3経路）。
- DoD4: gate crate 内の生の走査・読み出し・subprocess 直接呼び出しが機械検出され、検出時に local ゲートが非0で終了する。達成済み（raw-io ratchet を pre-commit 配線、terminal method 検出に再設計済み）。
- DoD5: アンチ空虚の対照実験を記録している。達成済み（ratchet の対照実験 29 tests green）。
- DoD6: 生 IO 呼び出しの baseline が boundary ラッパ経由へ移行され単調減少する。各移行は前後で cargo test と clippy が green を観測して確定。現況 baseline=67（specguard 37・overwatch 28・propguard 2、他 gate crate は 0）。実測 2026-07-24、HEAD b20aa4a5。
- DoD7: fail-open reader（Result 崩壊を permissive default へ潰す入力読み取り）が三値 Determination へ移行され確定数が単調増加する。既知集合 M1-M4 は 4/4 確定で全て main に landed（M1 overwatch read_violations を fail-closed scan 化、M2 specguard sentinel_pending 三値化、M3 overwatch lock の pid_alive 三値化、M4 harness-core load_json 三値化と stuckguard consumer 移行）。各確定は利害のない agent の RED oracle を fault injection で先に観測してから GREEN 化する（F→P）。新たな fail-open reader を発見したら既知集合に追加する。
- DoD8: fail-open reader は raw-IO ratchet に現れない意味的 Result 崩壊も含むため、可能な箇所は boundary 経由へ寄せて DoD6 と同時前進させる（一つの移行で ratchet baseline を下げつつ三値化を確定する）。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト（各移行は利害のない agent の RED oracle を先に観測してから確定）

## current_gap
DoD1-5 と DoD7(fail-open reader M1-M4=4/4) は達成済みで全て main(b20aa4a5) に landed。残る勾配は DoD6 の raw-IO ratchet=67 を boundary 経由へ移して単調減少させること。内訳は specguard 37・overwatch 28・propguard 2 で、最大バケットは specguard。次の右サイズの一手は specguard の report-state 読み取り（sentinel を present-but-unreadable のとき空扱いして gate を素通しさせる fail-open）を三値 boundary::read_to_string へ移す s スライス。DoD8 どおり ratchet 減少と三値化を同時に達成できる。

## next_action
specguard の sentinel 読み取り 2 箇所（report 描画パス main.rs:1193 と ack パス main.rs:1285、いずれも読み取り失敗を空扱いして drift gate を素通しさせる dangerous-direction の fail-open）を harness_core::boundary::read_to_string(三値 Determination) へ移行し present-but-unreadable を absent と区別する。利害のない agent が unreadable sentinel を注入して gate が誤通過することを先に観測(RED)→移行後 GREEN の F→P で確定。cargo test と clippy を green 観測。ratchet 67→65。

## parked

