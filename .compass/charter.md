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
- DoD9: 各 gate crate の verdict 経路が三値（harness_core::verdict）で表現され、silence・空集合・panic・IO・parse・subprocess 失敗を restrictive へ解決する。per-gate 監査で『未監査の verdict 経路 0本』を crate ごとの完了条件とする。【計測単位（2026-07-31 に定義。C3 未達を解消）】分母 ＝ harness_core::verdict を参照するクレート集合。実測 22（再測定: grep -rln で harness_core::verdict:: を crate の src 配下から数え、crate 名で uniq）。分子 ＝ per-gate 監査ドキュメントが存在するクレート数。実測 3（schemaguard, autoflow, budgetguard）。現況 3 of 22。【点の修正は監査に数えない】mutategate floorless-clamp(cde2212c)・condukt silent-verifier(dd3aad81)・blastguard cwd-bypass(ea1355f5) は個別欠陥の修正であって『全 verdict 経路を列挙し監査した』ではない。監査済みと数えるのは逐語引用つきの監査ドキュメントがある crate だけ（docs/audit-schemaguard-verdict-paths.md, docs/autoflow-verdict-audit.md, docs/audit-budgetguard-verdict-paths.md）。各確定は利害のない agent が fault injection で RED→GREEN(F→P)。
- DoD10: 新規 gate crate（taintguard 等）は誕生時点から verdict 三値化の作法で実装される。taintguard は0.1.0で新規作成、0.1.1で3ホール閉鎖、3rd trigger配線済み。enabledPlugins 未登録で inert（backlog e4687aad で追跡）。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト（各確定は利害のない agent の RED oracle を fault injection で先に観測してから GREEN 化して確定＝F→P。build≠validate）

## current_gap
DoD9 は 3 of 22（budgetguard の監査が 2026-07-31 に landed し分子が 2→3）。ゴールとの最大の差は『verdict 経路を持つと分かっているのに、一度も列挙されていない crate が 19 本ある』こと。分母は 2026-07-31 に定義済み（harness_core::verdict 参照クレート、実測 22）で単調増加が計算できる。勾配は DoD9 のままであり、次の一手は 19 本の未監査 crate の先頭を1本削ること。GATE crate（blastguard / propguard / specguard / stuckguard / mutategate / overwatch）は1本も監査されていないので、そこから先に取る。なお 2026-08-02 に specforge の出口（ratified spec の requirement を backlog に積む、merge bc022557）が landed したが、これは source→executor の導線であって DoD9 の分子ではない — 監査ドキュメントの無い crate は監査済みと数えない。

## next_action
blastguard の verdict 経路を per-gate 監査する（未監査の verdict 経路 0本 が完了条件）。blastguard を先に取る理由: CLAUDE.md 第3節が三値の正典例として crates/blastguard/src/model.rs:5『Three answers, not two.』を名指ししており、『二値型そのものが原因』という主張の当の実装が一度も全経路を列挙されていない。さらに blastguard は繰り返し mirror-gap（片側の構文だけ塞がれる）が見つかっている crate なので、点の修正ではなく列挙が効く。手順は budgetguard 監査と同型: (1) 全 verdict 経路を逐語引用つきで列挙し permissive な既知集合を明示、(2) 利害のない agent が fault injection で RED を先に観測、(3) 三値化して GREEN、(4) 意図的な permissive 仕様は壊さない、(5) 散文と実挙動の食い違いは同一コミットで是正、(6) docs/audit-blastguard-verdict-paths.md を追加（これが分子を動かす完了条件）。

## parked

