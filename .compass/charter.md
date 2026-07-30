## north_star
fail-open を『後から検出する』のをやめ、『そもそも書けない』側へ移す。harness_core の verdict 型は出力側でこれを既に達成している — Clean は private witness でしか作れず、Determination には unwrap_or も ok も Default も無いので、『判定できなかった』を『問題なし』へ潰す最短経路が型として存在しない。同じ技法を入力側（fallible な入力境界）と gate 内部の verdict 経路へ広げる。

## definition_of_done
- DoD1: ワークスペース直下の Cargo.toml が clippy lints を持ち gate crate が継承する。達成済み（6 crate が workspace 継承、非集約の4 crate は理由をコード内に明文化して parked）。
- DoD2: enforce の置き場所は local である（clippy ゲートを required status check にしない）。達成済み（CLAUDE.md 第7節どおり local pre-commit 配線）。
- DoD3: harness_core が fallible な入力境界のラッパを提供し返り値が三値 Determination である。達成済み（boundary モジュール、走査・読み出し・subprocess の3経路）。
- DoD4: gate crate 内の生の走査・読み出し・subprocess 直接呼び出しが機械検出され、検出時に local ゲートが非0で終了する。達成済み（raw-io ratchet を pre-commit 配線、terminal method 検出に再設計済み）。
- DoD5: アンチ空虚の対照実験を記録している。達成済み（ratchet の対照実験 29 tests green）。
- DoD6: 生 IO 呼び出しの baseline が boundary ラッパ経由へ移行され単調減少する — この軸は converged=達成として固定。現況 baseline=60（実測 2026-07-31 HEAD 30f6d7b5、count==baseline、floor held）。以後の勾配は raw-IO ではなく DoD9 へ移す。
- DoD7: fail-open reader が三値 Determination へ移行され確定数が単調増加する。既知集合 M1-M4 は 4/4 確定で全て main に landed。
- DoD8: fail-open reader は raw-IO ratchet に現れない意味的 Result 崩壊も含むため、可能な箇所は boundary 経由へ寄せて DoD6 と同時前進させる。
- DoD9: 各 gate crate の verdict 経路が三値（harness_core::verdict）で表現され、silence・空集合・panic・IO・parse・subprocess 失敗を restrictive へ解決する。per-gate 監査で『未監査の verdict 経路 0本』を crate ごとの完了条件とする。【計測単位（2026-07-31 に定義。C3 未達を解消）】分母 ＝ harness_core::verdict を参照するクレート集合。実測 22（再測定: grep -rln で harness_core::verdict:: を crate の src 配下から数え、crate 名で uniq）。分子 ＝ per-gate 監査ドキュメントが存在するクレート数。実測 2（schemaguard, autoflow）。現況 2 of 22。【点の修正は監査に数えない】mutategate floorless-clamp(cde2212c)・condukt silent-verifier(dd3aad81)・blastguard cwd-bypass(ea1355f5) は個別欠陥の修正であって『全 verdict 経路を列挙し監査した』ではない。監査済みと数えるのは逐語引用つきの監査ドキュメントがある crate だけ（docs/audit-schemaguard-verdict-paths.md, docs/autoflow-verdict-audit.md）。各確定は利害のない agent が fault injection で RED→GREEN(F→P)。
- DoD10: 新規 gate crate（taintguard 等）は誕生時点から verdict 三値化の作法で実装される。taintguard は0.1.0で新規作成、0.1.1で3ホール閉鎖、3rd trigger配線済み。enabledPlugins 未登録で inert（backlog e4687aad で追跡）。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト（各確定は利害のない agent の RED oracle を fault injection で先に観測してから GREEN 化して確定＝F→P。build≠validate）

## current_gap
DoD9 は 2 of 22。ゴールとの最大の差は『verdict 経路を持つと分かっているのに、一度も列挙されていない crate が 20 本ある』こと。分母は 2026-07-31 に定義済みで（harness_core::verdict 参照クレート）、単調増加が計算できるようになった。直近 22 コミットのうち後半3件（shared-crate bump gate・record-freshness audit）は『ゲート自身の出力が腐らないこと』という別軸を前進させたが、2026-07-31 に人間が裁定して『north_star は不変・必要だった寄り道であり DoD には昇格させない』と確定した。したがって勾配は DoD9 のままであり、次の一手は 20 本の未監査 crate の先頭を1本削ること。

## next_action
budgetguard の verdict 経路を per-gate 監査する（未監査 20 本の先頭。前サイクルで人間確認済み）。予算超過判定・ロック取得・設定読み込みが『判定不能（IO失敗・parse不能・ロック競合・空集合）』を『問題なし（許可）』へ潰していないかを、全 verdict 経路の列挙 + 逐語引用で確認する。利害のない agent が unreadable config・lock 競合・空データを注入して gate が誤って許可することを先に観測（RED）→ 三値 Determination 経由へ移行し fail-closed（GREEN）で確定（F→P）。完了の定義は『budgetguard の未監査 verdict 経路 0本』であり、点の修正では数えない — 逐語引用つきの監査ドキュメントを docs 配下に残して初めて分子が 2 から 3 になる。cargo test と clippy green を観測し、budgetguard version を3ファイル lockstep bump して rollout する。

## parked

