## north_star
fail-open を『後から検出する』のをやめ、『そもそも書けない』側へ移す。harness_core の verdict 型は出力側でこれを既に達成している — Clean は private witness でしか作れず、Determination には unwrap_or も ok も Default も無いので、『判定できなかった』を『問題なし』へ潰す最短経路が型として存在しない。同じ技法を入力側（fallible な入力境界）と gate 内部の verdict 経路へ広げる。

## definition_of_done
- DoD1: ワークスペース直下の Cargo.toml が clippy lints を持ち gate crate が継承する。達成済み（6 crate が workspace 継承、非集約の4 crate は理由をコード内に明文化して parked）。
- DoD2: enforce の置き場所は local である（clippy ゲートを required status check にしない）。達成済み（CLAUDE.md 第7節どおり local pre-commit 配線）。
- DoD3: harness_core が fallible な入力境界のラッパを提供し返り値が三値 Determination である。達成済み（boundary モジュール、走査・読み出し・subprocess の3経路）。
- DoD4: gate crate 内の生の走査・読み出し・subprocess 直接呼び出しが機械検出され、検出時に local ゲートが非0で終了する。達成済み（raw-io ratchet を pre-commit 配線、terminal method 検出に再設計済み）。
- DoD5: アンチ空虚の対照実験を記録している。達成済み（ratchet の対照実験 29 tests green）。
- DoD6: 生 IO 呼び出しの baseline が boundary ラッパ経由へ移行され単調減少する — この軸は converged=達成として固定。現況 baseline=60（実測 2026-07-26 HEAD 2742a8c1、count==baseline、floor held）。以後の勾配は raw-IO ではなく DoD9 へ移す。
- DoD7: fail-open reader が三値 Determination へ移行され確定数が単調増加する。既知集合 M1-M4 は 4/4 確定で全て main に landed。
- DoD8: fail-open reader は raw-IO ratchet に現れない意味的 Result 崩壊も含むため、可能な箇所は boundary 経由へ寄せて DoD6 と同時前進させる。
- DoD9: 各 gate crate の verdict 経路が三値（harness_core::verdict）で表現され、silence・空集合・panic・IO・parse・subprocess 失敗を restrictive へ解決する。per-gate 監査で『未監査の verdict 経路 0本』を完了条件とし、監査済み gate crate数/全gate crate数を単調増加の計測単位とする。各確定は利害のない agent が fault injection で RED→GREEN(F→P)。schemaguard と autoflow は本セッションで監査完了・三値化済み（commit 1601b835, cec3a431、docs/audit-schemaguard-verdict-paths.md, docs/autoflow-verdict-audit.md）。既に完了: mutategate floorless-clamp(cde2212c)・condukt silent-verifier(dd3aad81)・blastguard cwd-bypass(ea1355f5)。
- DoD10: 新規 gate crate（taintguard 等）は誕生時点から verdict 三値化の作法で実装される。taintguard は0.1.0で新規作成、0.1.1で3ホール閉鎖、3rd trigger配線済み。enabledPlugins 未登録で inert（backlog e4687aad で追跡）。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト（各確定は利害のない agent の RED oracle を fault injection で先に観測してから GREEN 化して確定＝F→P。build≠validate）

## current_gap
DoD9: schemaguard・autoflow の per-gate 監査は本セッションで完了(commit 1601b835, cec3a431)。残る勾配は verdict::Determination/Verdict 採用が低い gate crate — budgetguard(1 file)・gauge(1 file)・reviewgate(1 file) — の per-gate fail-closed 監査。次の右サイズ一手として budgetguard を先頭候補に選定（人間確認済み）。

## next_action
budgetguard の verdict 経路を per-gate fail-closed 監査する: 予算超過判定・ロック取得・設定読み込みが『判定不能（IO失敗・parse不能・ロック競合・空集合）』を『問題なし（許可）』へ潰していないか確認。利害のない agent が unreadable config・lock 競合・空データを注入して gate が誤って許可することを先に観測(RED)→三値 Determination 経由へ移行し fail-closed(GREEN)で確定(F→P)。cargo test と clippy green 観測、budgetguard version を3ファイル lockstep bump。

## parked

