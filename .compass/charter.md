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
- DoD9: 各 gate crate の verdict 経路が三値（harness_core::verdict）で表現され、silence・空集合・panic・IO・parse・subprocess 失敗を restrictive へ解決する。**完了条件はコードの性質であり、監査ドキュメントの存在ではない** — crate ごとの完了条件は『未監査の verdict 経路 0本、かつその監査が挙げた permissive 項目が 0 件または全件クローズ済み』とする。【計測単位（2026-07-31 に定義、2026-08-06 に改定）】分母 ＝ harness_core::verdict を参照するクレート集合。実測 22（測定コマンド・測定点・測定日は current_gap に記載）。分子は 2 値で併記する — (i) 監査済み ＝ 逐語引用つきの per-gate 監査 doc があり、監査者自身が『全 verdict 経路を列挙した』と宣言している crate 数。実測 7（schemaguard, autoflow, budgetguard, mutategate, propguard, stuckguard, reviewgate）。(ii) 是正完了 ＝ (i) に加えて、その監査が permissive として挙げた verdict 経路の項目が 0 件または全件クローズ済みの crate 数。実測 3（budgetguard, mutategate, stuckguard）。**DoD9 の完了条件は (ii) の是正完了だけであり、(i) の監査済みは完了条件ではなく途中経過の指標である。** 現況 是正完了 3 of 22（監査済みは 7 of 22）。【2026-08-06 の改定理由】旧定義は『分子は監査ドキュメントの存在で数える＝是正の landed とは別軸』と明記し、reviewgate が permissive 経路を 8 件見つけて 1 件も是正しないまま分子に入っていた（commit b41d8674 が旧定義のまま 6 of 22 を 7 of 22 へ動かした）。当の監査 doc 自身は末尾で『本監査は是正を伴わないため fixed 側には計上しない』と書いており、charter だけが是正なき前進を主張していた。この定義が立つ限り per-gate 監査は『doc を書けば分子が動く』へ縮退するので、完了条件を (ii) へ戻す。【是正が未完了で (ii) に入らない 4 crate】reviewgate は P1-P8 の 8 件が未是正（backlog 3357c2e2 と 277440b1）。propguard は F-2 と F-3 が未是正（backlog 1217fa4f と 3ca750b9）。schemaguard は §5-5 が未是正。**この理由は 2026-08-06 に是正した** — 旧版は『read-only 監査で 1 行も変更しておらず silent-skip 2 件が未解決』と書いていたが、現行コードに対して誤りである。監査 doc 自体は read-only だったものの crate はその後 0.1.10 まで書き換わり、§5-6/§5-7/§5-8 は既にクローズしている（crates/schemaguard/src/schema.rs:132 の `Report` が violations・undetermined・waived の3フィールドをいずれも private で持ち、crates/schemaguard/src/main.rs は valid:true でも not_checked を出す。測定日 2026-08-06、測定点 7bc09647）。(ii) に入らない実際の理由は §5-5 ＝ **消費側**の silent-skip が残っていること: crates/condukt/src/main.rs:4859-4864 の `schema_precheck` が schema 未登録も JSON パース不能もどちらも `return Ok(())` に潰す（`schema_precheck_each` :4873 も同型。backlog 3e7d5df8）。答えは合っていて理由が嘘という、この DoD が止めようとしている当のもの（CLAUDE.md 第4節）だった。【(ii) のスコープは消費経路を含む】判定の消去は判定を産む関数ではなく **call site** で起きるため、監査 doc が消費側 crate の経路を対象に含めているなら、その未是正項目は (ii) を満たさない。schemaguard の §5-5 が項目そのものは condukt の src にある実例。autoflow は 8 件のうち 6 件を 0.1.17 でクローズしたが、監査が P-7 として挙げた now_secs の判定不能フォールバック（時刻取得失敗が 0 に潰れ、中断タスクの復帰判定が常に偽になる）が今も残り、監査がスコープ外に置いた charter_freshness の判定不能も後から実在の fail-open として起票された（backlog 2ca03efc）。4 crate とも (i) には数え、(ii) には数えない。【carve-out はクローズではない】budgetguard は install 経路の 2 件（backlog a1cc21f2 と 1f922949）を『verdict 経路ではなく設置経路なので本監査の完了条件の外』と自己宣言して残している。(ii) は verdict 経路の項目で数えるため budgetguard は (ii) に入るが、この carve-out 自体が同じ自己許可の小型版なので id を明記して可視化する。**監査が自分でスコープ外と宣言した項目でも、それが verdict 経路なら (ii) には入れない** — autoflow の charter_freshness がその実例である。【点の修正は監査に数えない】mutategate floorless-clamp(cde2212c)・condukt silent-verifier(dd3aad81)・blastguard cwd-bypass(ea1355f5) は個別欠陥の修正であって『全 verdict 経路を列挙し監査した』ではない。【部分監査も (i) に数えない】blastguard には監査 doc が既に存在するが、監査者自身が『機械列挙は完了、分類は部分的』として分子を動かさないと宣言しているため (i) にも数えない（ファイル名は current_gap に記載）。各確定は利害のない agent が fault injection で RED→GREEN(F→P)。
- DoD10: 新規 gate crate（taintguard 等）は誕生時点から verdict 三値化の作法で実装される。taintguard は0.1.0で新規作成、0.1.1で3ホール閉鎖、3rd trigger配線済み。enabledPlugins 未登録で inert（backlog e4687aad で追跡）。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト（各確定は利害のない agent の RED oracle を fault injection で先に観測してから GREEN 化して確定＝F→P。build≠validate）

## current_gap
DoD9 は **是正完了 3 of 22（監査済み 7 of 22）**。完了条件は是正完了の側だけである（DoD9 参照）。ゴールとの最大の差は 2 つになった: (1) verdict 経路を持つと分かっているのに一度も列挙されていない crate が 15 本ある、(2) **列挙は済んだのに permissive 項目が未是正のまま残っている crate が 4 本ある**（reviewgate 8 件・propguard 2 件・schemaguard は消費側 §5-5（condukt の schema_precheck）が未是正・autoflow は 8 件中 6 件クローズで P-7 が残存）。

【この節の数字の測定（前版から継承せず、毎回測り直すこと）】
- 分母 22 — `grep -rln 'harness_core::verdict::' crates/*/src/ | cut -d/ -f2 | sort -u | wc -l`。測定日 2026-08-06、測定点 690619c6。2026-07-31 の定義以来 22 で不変（2026-08-04・2026-08-06 に再測定）。
- 監査 doc は 8 枚 — `ls docs/audit-*-verdict-paths.md docs/autoflow-verdict-audit.md | wc -l`。測定日 2026-08-06、測定点 690619c6。うち blastguard は監査者自身が『分子を動かさない（分類は部分的）』と宣言しているため監査済み 7 に入らない。**doc の枚数は分子ではない** — これが 2026-08-06 の改定の要点。
- 未是正項目の在庫 — `backlog list --project /Users/yuki/src/harness --status pending --json`。測定日 2026-08-06。reviewgate 3357c2e2 / 277440b1、propguard 1217fa4f / 3ca750b9、autoflow 2ca03efc、budgetguard の carve-out a1cc21f2 / 1f922949 がいずれも pending。
- autoflow P-7 の残存 — 監査 §4.7 が挙げた `now_secs` の `.unwrap_or_default()` は `crates/autoflow/src/condukt.rs:207-211` に今も残る（コード実測 2026-08-06、測定点 690619c6）。0.1.17 の修正 commit `cec3a431` は本文で §4.1-4.6 の 6 件だけを列挙しており、P-7 は含まれていない。**監査 doc の doc-claim-exempt マーカーは『0.1.17 で修正済み』と一律に主張しているが、P-7 についてはコード実測と食い違う** — 監査 doc の自己申告ではなくコードを見て数えること。

【2026-08-06 の測定棒の改定】旧定義（分子＝監査 doc の存在）のもとで commit b41d8674 が 6 of 22 を 7 of 22 へ動かしていた。改定後も監査済みの軸では 7 のままだが、**完了条件の軸（是正完了）では 3** である。この改定を起票した backlog a75ad752 は「reviewgate を外して 6」を見込んでいたが、同じ基準を propguard（F-2 / F-3 未是正）・schemaguard（消費側 §5-5 が未是正）・autoflow（P-7 残存）にも適用した実測は 3 だった。**見込みではなく実測を書く。**

5→6 は stuckguard の監査（2026-08-04 landed、当時のセッション）。35 サイトを未分類 0 で分類し fail-open を 1 件発見・修正した — progress advisory が『測れなかった第三 signal』を測定値 0.0 として平均に入れており、score の上界が 2/3 ＝ 既定閾値 0.75 未満に固定されていた。つまり error digest を持たない全ループ（Read/Grep/Edit の繰り返し＝この advisory が捕まえるべき形そのもの）で数学的に発火不能だった。docstring は正しい仕様を書いており実装だけが違った＝ CLAUDE.md 第4節の事案。

6→7（commit b41d8674）は reviewgate の監査で、**別セッション**が 2026-08-04 に landed させたもの（当時のセッションはそれを分子へ反映しただけで、監査自体は行っていない）。2026-08-06 の改定後、この 6→7 は **監査済みの軸の前進であって完了条件の軸の前進ではない** と読み直す — reviewgate は 8 件すべて未是正なので是正完了には入らない。当の監査 doc 自身が『本監査は是正を伴わないため fixed 側には計上しない』と書いており、charter だけが是正なき前進を主張していた。この監査は手法として同時期の 2 本（propguard・stuckguard）より強い: 判定を産む関数と消費 call site を全列挙したうえで、疑わしい経路をリリースビルドしたバイナリへの**ブラックボックス fault injection 8 本**で観測している（PATH 先頭の fake git で内容取得だけを失敗させる、reviewer に非 UTF-8 を出力させる、glob を 1 個だけ壊す 等）。判断ではなく観測に落とす手本として、以降の per-gate 監査はこの水準を既定にする。

**ただし reviewgate は 8 件すべて未是正**であり、しかもその P1（diff 取得失敗が『空 diff ＝ 変更なし』に潰れ無診断で allow）は propguard の F-2（backlog 1217fa4f）と**同一の shape** である。同じ欠陥が別 crate に独立して存在することが観測で確認された＝これは per-crate の個別欠陥ではなく**横断パターン**。監査を 1 crate ずつ進める現在の進め方は、このパターンを crate ごとに再発見し続けることになる。

次の一手の候補は 2 つあり、どちらを取るかは人間の判断に返す:
(a) blastguard の監査の**完遂**（従来の予定。census 実測 131 サイト — scripts/census-verdict-terminals.py blastguard、2026-08-04、測定点 1f0cf124。charter の前版は 120 と書いていたが再現しなかったので置き換えた）。docs/audit-blastguard-verdict-paths.md は 2026-08-02 に landed 済みだが、監査者自身が『機械列挙は完了・分類は部分的』として分子を動かさないと宣言しているので、残りは doc の追加ではなく未分類の解消である。カテゴリ棄却が実在の欠陥を隠した当事者であり、個別分類の価値は高い。
(b) 『diff/scan 取得失敗が空集合に潰れて allow になる』横断パターンの一括是正。reviewgate P1・propguard F-2 で 2 crate 確認済みで、監査済み 7 crate を横断で当たれば更に出る見込み。**改定後の完了条件（是正完了）を実際に動かすのはこちらの側**であり、(a) は監査済みの軸しか動かさない。

残る未監査 GATE crate は specguard・overwatch。blastguard は部分監査済み（分類が未完のため監査済み 7 に入れていない）。

## next_action
blastguard の verdict 経路を per-gate 監査する（完了条件は DoD9 の (ii) 是正完了 ＝ 挙げた permissive 項目が全件クローズ。『未監査の verdict 経路 0本』は (i) 監査済みの条件であって完了条件ではない）。blastguard を先に取る理由: CLAUDE.md 第3節が三値の正典例として crates/blastguard/src/model.rs:5『Three answers, not two.』を名指ししており、『二値型そのものが原因』という主張の当の実装が一度も全経路を列挙されていない。さらに blastguard は繰り返し mirror-gap（片側の構文だけ塞がれる）が見つかっている crate なので、点の修正ではなく列挙が効く。手順は budgetguard 監査と同型: (1) 全 verdict 経路を逐語引用つきで列挙し permissive な既知集合を明示、(2) 利害のない agent が fault injection で RED を先に観測、(3) 三値化して GREEN、(4) 意図的な permissive 仕様は壊さない、(5) 散文と実挙動の食い違いは同一コミットで是正、(6) docs/audit-blastguard-verdict-paths.md（2026-08-02 landed・分類が部分的）の未分類サイトを 0 にする。**doc の追加は完了条件ではない** — 監査済みの軸に入るのは未分類 0 になったとき、完了条件（是正完了）に入るのは挙げた permissive 項目が全件クローズしたときである。

## parked

