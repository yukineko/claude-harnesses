## north_star
fail-open を『後から検出する』のをやめ、『そもそも書けない』側へ移す。ゴールは『判定不能を permissive な既定へ潰す最短経路が、レビューではなくコンパイラによって拒否される』こと。射程は 2026-08-05 に訂正した — 前版は『出力側（harness_core の verdict 型）では既に達成している。Determination には unwrap_or も ok も Default も無い』と書いていたが実測で偽。Determination::require が std::Result を返し、Result::unwrap_or は E 側に境界を持てないため、boundary::read_dir_entries(p).require().unwrap_or_default() が実 harness-core に対してビルド成功する（unwrap_or に空を渡す形と is_ok で真偽へ潰す形も同様に成功）。塞げるのは E 側ではなく T 側であり、boundary の payload が Vec と Option すなわち Default を持ち呼び出し側で自作可能なことが erasure を成立させている。したがって射程は入力側と gate 内部だけでなく harness_core 自身を含む。【2026-08-11 に射程を liveness 軸へ拡張した — 人間裁定】セッション/worker の生死不明を、別スレッドではなく本 north_star の一事例として扱う。理由は shape が同型であることが実測できたため: condukt state probe は 5 日前に死んだ run の task を verdict=progressing と判定するが、その根拠に挙がる git-head は当該 task の worktree HEAD ではなく repo 全体の HEAD であり、実測ではこのセッションが 93 秒前に main へ積んだ commit だった（crates/condukt/src/state.rs:1134 逐語「git HEAD is a run-level (repo-wide) signal shared by every task」）。CLAUDE.md 第8節が「別セッションは常に存在する」を保証する以上 HEAD が止まることは無く、凍結した task は永久に stalled へ収束しない — probe_run 自身の docstring はその収束を主張しているので散文と実挙動も食い違う。これは「判定不能が permissive/誤った側へ潰れる」の liveness 軸での現れであり、既存の測定棒（利害のない agent の RED 先行 + anti-vacuity 対照）と DoD の作法がそのまま適用できる。ただし**この軸では制限側の向きが反転する**: ゲートでは判定不能は「ユーザーを block する側」だが、GC では「削除しない側」が制限側である（削除だけが不可逆なので生死不明は保全側へ倒す）。測定点 2f9f8b67。【2026-08-21 に射程を shell 層へ拡張した — 人間裁定】『判定不能が、もっともらしい既定へ黙って潰れる』という同型の欠陥が shell スクリプトでも実測されたため、scripts/ 配下のゲートスクリプトを本 north_star の射程に含める（DoD13）。ただし**この軸では機構が変わる**: Rust 側の機構はコンパイルエラーでの拒否（DoD11）だが、shell にコンパイラは無いので、拒否は pre-commit の字句ゲートが担う。根拠となった実測は f9bba02a（nullglob で消えた glob → 引数 0 個の `ls` が `$PWD` を列挙して成功し、破壊的な `cp -f` の書き込み先になった）と 90a358d3（構文ゲート自身が非 0 を返せない）の 2 件。測定点 58c779af。

## definition_of_done
- DoD1: ワークスペース直下の Cargo.toml が clippy lints を持ち gate crate が継承する。達成済み（6 crate が workspace 継承、非集約の 4 crate は理由をコード内に明文化して parked）。
- DoD2: enforce の置き場所は local である（clippy ゲートを required status check にしない）。達成済み（CLAUDE.md 第7節どおり local pre-commit 配線）。
- DoD3: harness_core が fallible な入力境界のラッパを提供し、返り値が三値である。【2026-08-11 に部分達成から達成済みへ戻した】2026-08-05 の格下げ理由だった boundary 内部の fail-open（子プロセス出力の読み取り失敗と受信タイムアウトが空文字へ潰れ、その空文字が known な CommandOutput として下流へ渡る）は是正済み。実測 2026-08-11、測定点 2f9f8b67: read_pipe_bounded の返り値が Determination であり、旧挙動の `let _ = p.read_to_string(..)` と `unwrap_or_default()` は過去形の docstring にしか現れない（`grep -n 'let _ = p.read_to_string\|recv_timeout(timeout).unwrap_or_default()' crates/harness-core/src/boundary.rs` はコード行に 0 ヒット）。この是正は condukt run run-20260806-062115-17609 の t1 として landed していたが、run の state が 5 日間 running のまま閉じられておらず charter からは未達に見えていた（backlog 8b871bb1）。完了条件は変えない: boundary 内部に判定不能を permissive へ潰す経路が 0 本であること。
- DoD4: gate crate 内の生の走査・読み出し・subprocess 直接呼び出しが機械検出され、検出時に local ゲートが非 0 で終了する。達成済み（raw-IO ratchet を pre-commit 配線、terminal method 検出に再設計済み）。
- DoD5: アンチ空虚の対照実験を記録している。達成済み（ratchet の対照実験 29 tests green）。
- DoD6: 生 IO 呼び出しの baseline が boundary ラッパ経由へ移行される。【2026-08-05 に converged 固定を撤回】前版は count==baseline を converged=達成として固定していたが、それは『減少が止まった』という観測であって達成ではない。【2026-08-11 に数字を測り直した — 60 は継承値であり実測と食い違っていた】現況 baseline=48（測定コマンド `cat scripts/check-raw-io-ratchet.baseline` および `python3 scripts/check-raw-io-ratchet.py`、測定日 2026-08-11、測定点 2f9f8b67。ゲート自身も `raw-io-ratchet: count == baseline (48)` と報告する）。前版は baseline=60（実測 2026-07-31 HEAD 30f6d7b5）と書いていたが、その後 d093c478 で overwatch の『contested fail-soft contract』束が丸ごと落ちて 60 から 48 へ動いており、charter だけが 60 を主張していた。完了条件は残 48 サイトを 3 分類（既に fail-closed で機械的なもの、文書化された fail-soft 契約を持つもの、未判定）し、未判定を 0 本にすること。**この数字は継承せず毎回上記コマンドで測り直すこと。** 【2026-08-21 に DoD8 を吸収 — 人間裁定】旧 DoD8（『raw-IO ratchet に現れない意味的 Result 崩壊も含むため、可能な箇所は boundary 経由へ寄せて DoD6 と同時前進させる』）は 観測可能な合否判定 を持たなかった（『可能な箇所』『同時前進』はどちらも閾値ではない）ため DoD 一覧から撤去し、本項へ吸収した。したがって DoD6 の 3 分類の対象には、ratchet が 字句として拾う生 IO 呼び出しだけでなく **意味的 Result 崩壊**（Determination を Result へ落としてから unwrap_or / ok / is_ok で真偽へ潰す形）も含める。完了条件は変わらない: 未判定を 0 本にすること。再測 2026-08-21、測定点 58c779af: baseline=48 で不変、ゲート自身も `raw-io-ratchet: count == baseline (48); no new raw-IO call, floor held.` と報告する。
- DoD7: fail-open reader が三値へ移行され確定数が単調増加する。既知集合 M1 から M4 は 4 件すべて確定して main に landed。
- DoD9: 各 gate crate の verdict 経路が三値（harness_core::verdict）で表現され、silence・空集合・panic・IO・parse・subprocess 失敗を restrictive へ解決する。**完了条件はコードの性質であり、監査ドキュメントの存在ではない** — crate ごとの完了条件は『未監査の verdict 経路 0本、かつその監査が挙げた permissive 項目が 0 件または全件クローズ済み』とする。【計測単位（2026-07-31 に定義、2026-08-06 に改定）】分母 ＝ harness_core::verdict を参照するクレート集合。実測 22（測定コマンド・測定点・測定日は current_gap に記載）。分子は 2 値で併記する — (i) 監査済み ＝ 逐語引用つきの per-gate 監査 doc があり、監査者自身が『全 verdict 経路を列挙した』と宣言している crate 数。実測 7（schemaguard, autoflow, budgetguard, mutategate, propguard, stuckguard, reviewgate）。(ii) 是正完了 ＝ (i) に加えて、その監査が permissive として挙げた verdict 経路の項目が 0 件または全件クローズ済みの crate 数。実測 3（budgetguard, mutategate, stuckguard）。**DoD9 の完了条件は (ii) の是正完了だけであり、(i) の監査済みは完了条件ではなく途中経過の指標である。** 現況 是正完了 3 of 22（監査済みは 7 of 22）。【2026-08-11: この数字は測り直した結果であって継承ではない】下の【2026-08-07】節は測定手順が再現しないと記録していたが、**2026-08-11 に再測して再現した**ので撤回する（詳細は同節）。【2026-08-06 の改定理由】旧定義は『分子は監査ドキュメントの存在で数える＝是正の landed とは別軸』と明記し、reviewgate が permissive 経路を 8 件見つけて 1 件も是正しないまま分子に入っていた（commit b41d8674 が旧定義のまま 6 of 22 を 7 of 22 へ動かした）。当の監査 doc 自身は末尾で『本監査は是正を伴わないため fixed 側には計上しない』と書いており、charter だけが是正なき前進を主張していた。この定義が立つ限り per-gate 監査は『doc を書けば分子が動く』へ縮退するので、完了条件を (ii) へ戻す。【是正が未完了で (ii) に入らない 4 crate】reviewgate は P1-P8 の 8 件が未是正（backlog 3357c2e2 と 277440b1）。propguard は F-2 と F-3 が未是正（backlog 1217fa4f と 3ca750b9）。schemaguard は §5-5 が未是正。**この理由は 2026-08-06 に是正した** — 旧版は『read-only 監査で 1 行も変更しておらず silent-skip 2 件が未解決』と書いていたが、現行コードに対して誤りである。監査 doc 自体は read-only だったものの crate はその後 0.1.10 まで書き換わり、§5-6/§5-7/§5-8 は既にクローズしている（crates/schemaguard/src/schema.rs:132 の `Report` が violations・undetermined・waived の3フィールドをいずれも private で持ち、crates/schemaguard/src/main.rs は valid:true でも not_checked を出す。測定日 2026-08-06、測定点 7bc09647）。(ii) に入らない実際の理由は §5-5 ＝ **消費側**の silent-skip が残っていること: crates/condukt/src/main.rs:4859-4864 の `schema_precheck` が schema 未登録も JSON パース不能もどちらも `return Ok(())` に潰す（`schema_precheck_each` :4873 も同型。backlog 3e7d5df8）。答えは合っていて理由が嘘という、この DoD が止めようとしている当のもの（CLAUDE.md 第4節）だった。【(ii) のスコープは消費経路を含む】判定の消去は判定を産む関数ではなく **call site** で起きるため、監査 doc が消費側 crate の経路を対象に含めているなら、その未是正項目は (ii) を満たさない。schemaguard の §5-5 が項目そのものは condukt の src にある実例。autoflow は 8 件のうち 6 件を 0.1.17 でクローズしたが、監査が P-7 として挙げた now_secs の判定不能フォールバック（時刻取得失敗が 0 に潰れ、中断タスクの復帰判定が常に偽になる）が今も残り、監査がスコープ外に置いた charter_freshness の判定不能も後から実在の fail-open として起票された（backlog 2ca03efc）。4 crate とも (i) には数え、(ii) には数えない。【carve-out はクローズではない】budgetguard は install 経路の 2 件（backlog a1cc21f2 と 1f922949）を『verdict 経路ではなく設置経路なので本監査の完了条件の外』と自己宣言して残している。(ii) は verdict 経路の項目で数えるため budgetguard は (ii) に入るが、この carve-out 自体が同じ自己許可の小型版なので id を明記して可視化する。**監査が自分でスコープ外と宣言した項目でも、それが verdict 経路なら (ii) には入れない** — autoflow の charter_freshness がその実例である。【点の修正は監査に数えない】mutategate floorless-clamp(cde2212c)・condukt silent-verifier(dd3aad81)・blastguard cwd-bypass(ea1355f5) は個別欠陥の修正であって『全 verdict 経路を列挙し監査した』ではない。【部分監査も (i) に数えない】blastguard には監査 doc が既に存在するが、監査者自身が『機械列挙は完了、分類は部分的』として分子を動かさないと宣言しているため (i) にも数えない（ファイル名は current_gap に記載）。各確定は利害のない agent が fault injection で RED→GREEN(F→P)。 【2026-08-07: この DoD の測定手順は再現しない — 数値を継承しないこと】上に書いた在庫の測定コマンド `backlog list --project <旧チェックアウトの project キー> --status pending`（2026-08-07 時点でこのマシンにもう存在しないパスが project キーだったため、2026-08-17 に check_dod_refs 誤検知回避でパス文字列を除去。文意は不変）に、この DoD が名指しした 9 件（3357c2e2 / 277440b1 / 1217fa4f / 3ca750b9 / 2ca03efc / 3e7d5df8 / a1cc21f2 / 1f922949、および b4baf3d7）は**1 件も現れない**。実測 2026-08-07、測定点 93a524d5: repo ストア（repo 側 backlog ストア）の pending は 14 件、9 件はいずれも ホーム側の backlog ストア（668 件）側にある。つまり **是正完了 3 of 22 は今日この手順では再現できない**。前版の 3 を継承数値として使ってはならない（CLAUDE.md の測定値の節）。 【2026-08-11 に撤回 — 測定手順は再現する】測定日 2026-08-11、測定点 412c7656。同じ手順（repo ストアに対する pending の在庫）で 9 件すべての所在が確定した: 8 件（3357c2e2 / 277440b1 / 1217fa4f / 3ca750b9 / 2ca03efc / 3e7d5df8 / a1cc21f2 / 1f922949）はいずれも pending として現れ、残る b4baf3d7 は status=done だが、**これは作業完了ではない** — チケット本文が逐語で「CLOSED AS DUPLICATE（2026-08-06、ユーザー裁定）… この close は作業完了を意味しない。実装は b0cacd15 側で行う」と書いている。b0cacd15 は現在も pending。したがって DoD11 の残余（`match { Required::Blocked(_) => Vec::new() }` の手書き形を字句ゲートで検出する）は**未達のまま**であり、実際 pre-commit にそのゲートは配線されていない（実測 2026-08-11: `.githooks/pre-commit` に該当チェックの呼び出しが 0 件、`scripts/` に `Required::Blocked` を探す実装も 0 件。`scripts/census-verdict-terminals.py` は存在するが自身の usage 例以外から呼ばれていない）。**status=done を達成の証拠として読まないこと** — この DoD が止めようとしている「答えは合っていて理由が嘘」の在庫版である。repo ストアの pending 総数は 206 件。2026-08-07 に 14 件しか見えなかったのは 5ba13c3e（ストア分岐）が未是正だったためで、その完了条件 2（旧ストアに項目があるのに新ストアが空/少ない状態を検出して警告する）が landed した結果、在庫が repo ストア側から見えるようになった。したがって **是正完了は再び測定可能であり、独立に測り直した値も 3 of 22 である**（是正未完了は reviewgate（3357c2e2 と 277440b1）、propguard（1217fa4f と 3ca750b9）、autoflow（2ca03efc）、schemaguard（3e7d5df8）の 4 crate。budgetguard の a1cc21f2 と 1f922949 は本 DoD が明示する install 経路の carve-out なので (ii) からは外さない）。**この 3 は継承値ではなく再測値である** — 次に読む者も同じ手順で測り直すこと。ストアの統合はこの DoD とは別の一手として切り出す（backlog 5ba13c3e。ユーザー決定により現時点では可視化のみで移行しない）。**測れないという事実自体をここに残すのが正しく、測れないまま数字だけ据え置くのが誤りである。**
- DoD10: 新規 gate crate は誕生時点から verdict 三値化の作法で実装される。完了条件は不変（新設 gate crate が最初のコミットから三値で判定を返し、判定不能を permissive へ潰す経路を持たないこと）。**この DoD の唯一の実例だった taintguard は 2026-08-24 のユーザー裁定でリポジトリから撤去された**（判定が厳しすぎて実害が出たため）ので、現時点でこの DoD には生きた実例が無い。次に新設される gate crate が最初の観測対象になる。
- DoD11: 【2026-08-05 追加】判定不能を permissive な既定へ潰す erasure が、レビューではなくコンパイルエラーとして拒否される。完了条件は、封印前に実測でビルド成功していた 3 形（require のあとに unwrap_or_default を呼ぶ形、unwrap_or に空の既定を渡す形、is_ok で真偽へ潰す形）が trybuild の compile-fail 固定として先に RED を観測され、封印後に GREEN へ移り、同時に正常系の positive control がビルドし続けること。既存の erasure 実地（stuckguard の config 読み出しと condukt の gate 実行）がコンパイルエラーとして露出することを観測で確認する。 【2026-08-07 に達成】compile-fail 固定は `crates/harness-core/tests/ui/verdict/` に存在し、`cargo test -p harness-core --test verdict_compile_fail` は 6 fixture すべて green（実測 2026-08-07、測定点 93a524d5）。3 形は require の erasure fixture に原文のまま残っており、fixture 自身の doc が 「committed as a KNOWN-RED negative control」と RED 先行を記録している。`require()` は `harness_core::verdict::Required` を返し、`unwrap_or`/`unwrap_or_default`/`unwrap_or_else`/`ok`/`is_ok` のいずれも持たないので 3 形とも E0599。docstring も実挙動へ更新済み（verdict モジュールの doc 460-463 行が旧 Result 経由の穴とその封印を明記）。手順 (4) の既存 erasure 実地は `cargo build --workspace` が通ることで是正済みと確認した（未是正なら封印はコンパイルエラーとして露出する）。残余は fixture 自身が明記する `match { Required::Blocked(_) => Vec::new() }` の手書き形で、これは型ではなく字句ゲートの担当。【2026-08-11 に追跡先を訂正】backlog b4baf3d7 は status=done だが、その close は「重複としてのクローズであり作業完了を意味しない」とチケット自身が逐語で述べており、実装は **b0cacd15（現在も pending）** 側にある。字句ゲートは pre-commit に未配線（実測 2026-08-11、測定点 412c7656）。DoD11 の型封印は達成だが、この第二層は未達である。
- DoD12: 【2026-08-11 追加。north_star の liveness 軸への拡張に伴う】worker とセッションの生死が、無関係な活動に汚染されない task 固有のシグナルで判定され、生死不明が構造的に known へ収束する。完了条件は 4 つすべて: (a) 進捗判定が当該 task に閉じたシグナル（その task の worktree HEAD、worktree 内ファイルの更新、その task 固有の transcript）で行われ、repo 全体の HEAD を生存証拠に使う経路が 0 本であること。(b) 凍結した task が、repo が活動中でも stalled へ収束することを behavioural test で観測し、RED を先に観測していること。(c) アンチ空虚の対照として、本当に進捗している task が progressing のままであることを同時に観測していること。(d) GC 側は向きが反転するため、判定不能を「削除可」へ写す経路が 0 本であり、未コミット作業（tracked の変更と untracked の両方）を持つ worktree は dead 判定でも保全してからでなければ削除できないこと。現況は未達 — 実測 2026-08-11 測定点 2f9f8b67 で probe が repo 全体の HEAD を見ており（north_star 参照）、判定不能が不可逆削除へ直結する経路が 2 本ある（condukt の CLI 側にある abandon --all-stuck が liveness も dirty も見ずに worktree と branch のポインタを捨てる形と、worktree モジュールの orphans が判定不能をそのまま削除対象へ積む形。行番号は backlog e99f61e4 に記載）。backlog e99f61e4 と 8b871bb1 と 2d3cbda2 と 5daf0b60 と b637936f。 【2026-08-17 に再測 — この charter は 2026-08-11 以降 79 commit 分未更新だったため再確認した。数字は継承せず measured】(a) は達成: f906094c で probe が task 自身の worktree HEAD へ切替済み、639813db で reap gate も同様、c57c3f20・4f9a4735・ef56295d・399128cf で run-scope の RED-first behavioural test 群が landed（8b871bb1 は status=done、compass outcome forward が 69651a93 に記録 — ただし outcomes ストア（gitignore 対象、per-worktree ローカルファイル）はこの worktree からは再読できない。in-repo の記録は commit メッセージのみ）。(d) の骨格（reconcile による live・dead・undetermined 三値化と削除のゲート限定）も 74e3a879・b896ae0c で landed し e99f61e4 に merge 済み、resume 側 ((a) の e99f61e4 版) も 0.7.138 で landed・独立検証済み（7/7 kill）。**ただし e99f61e4 は現在も status=claimed** — 実装直後の独立検証で reconcile 自体の欠陥が見つかったため（fe84d350: condukt run state が claim していない worktree — CLAUDE.md 第8節が常態とする flow スキル由来の session-* worktree が該当 — を無条件に dead と判定していた）。2026-08-12 の人間裁定で「claim 無し＝Undetermined」、2026-08-14 の人間裁定で「Dead ⇐ 活動停滞 ∧ progress-window 経過 ∧ transcript 証拠なし（3点連言）」が確定し、RED テストは 375587a2 に委託済みで 2026-08-14 に裁定との整合を読み直し確認済み（fe84d350 と 5aaecbf6 の notes 参照）。**残っている実装**: (i) progress window を注入可能にする、(ii) 3点連言で Dead+removable に到達することを固定する anti-vacuity テスト（375587a2 で dead_clean_worktree_is_removable を削除した際に失った対照の作り直し）。2d3cbda2（dead session の worktree と branch を回収する GC 自体が無い）と 5daf0b60（abandon --all-stuck が生存 worker を dirty・liveness 未確認で切り離す）と b637936f（stuck_task_ids の三値化）は reconcile とは別コードパス（condukt state abandon）の話で、いずれも未着手のまま残っている。
- DoD13: 【2026-08-21 追加。north_star の shell 層への射程拡張に伴う — 人間裁定】shell スクリプトにおいて『解決の失敗』が『もっともらしい既定』へ黙って化ける形が機械検出され、検出時に local ゲート（pre-commit）が非 0 で終了する。**機構はコンパイラではなく字句ゲート**であり、DoD11 の第二層（未配線のまま残っている字句ゲート）と同じ層に属する — shell にコンパイラは無いので north_star の『そもそも書けない側へ移す』はこの層で実装する。射程に入れた根拠は仮説ではなく本日の実測 2 件である: (a) f9bba02a — f9bba02a が直した 1 行（逐語は current_gap の【DoD13 の根拠となった逐語】に記載）が `shopt -s nullglob` の下でマッチ 0 のとき語ごと消え、引数 0 個の `ls` が失敗せず `$PWD` を列挙して **成功する**。`head -n1` が C locale 順の先頭 `CLAUDE.md` を返し、直後の `cp -f` が repo 直下の CLAUDE.md を plugin の ELF で 38 回上書きした（37678 bytes の markdown → 704304 bytes の ELF、`git checkout -- CLAUDE.md` で復旧）。`2>/dev/null` も `|| true` も何も守っていなかった。(b) 90a358d3 — `scripts/tests/check-shell-syntax.sh` の負の対照 4 件がすべて『落ちるべき場面で exit 0』であり、この構文ゲート自体が非 0 を返せない疑いがある（原因は未読なので判定不能として起票済み）。現況の分母（実測 2026-08-21、測定点 58c779af）: `ls scripts/*.sh scripts/*.py | wc -l` → **65 本**、うち失敗握り潰しの語法を含む shell は `grep -rlE '\|\| true|2>/dev/null \| head|\.ok\(\)' scripts/*.sh | wc -l` → **5 本**。完了条件は『65 本を 3 分類（fail-closed で機械的 / 文書化された fail-soft 契約を持つ / 未判定）し、未判定を 0 本にする』こと（DoD6 と同じ作法）。**アンチ空虚の対照が必須**: 字句ゲートを入れるなら、上記 (a) の逐語形が RED として落ちることを先に観測してから GREEN にする。**この DoD は今サイクルの ONE ではない**（下記 next_action と parked を参照）。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト（各確定は利害のない agent の RED oracle を fault injection で先に観測してから GREEN 化して確定＝F→P。build≠validate）。【2026-08-05 追加】測定棒自体も検査対象とする — 分子や達成条件の定義を変えて数字を動かした場合は、変更日と旧定義での値を charter に併記する。定義変更による前進は前進として数えない。

## current_gap
【2026-08-21 に再オリエンテーション — 31 commit 分の drift を再測して書き直した。前版は 2026-08-17 版】

ゴールとの最大の差は 3 つ。(1) と (2) は前版から軸が変わっていないが**数字が動いた**。(3) は今回追加。

(1) **liveness 軸の ONE は 4 日間 未着手のまま残り、その間に在庫が倍近く増えた。**
    2026-08-17 に選んだ ONE（fe84d350 + 5aaecbf6、e99f61e4 のブロッカー）は **3 件すべて実測で今も
    `status = "pending"`**（測定コマンド `grep -A9 '^id = "<id>"' .backlog/tasks.toml`、測定日 2026-08-21、
    測定点 58c779af）。つまり前サイクルの一手は消化されていない。
    一方でその間に **session-* worktree は 35 → 66 へ増えた**（測定コマンド
    `ls -d /mnt/c/Users/hiroyuki_nakayama/src/.harness-worktrees/session-* | wc -l`、測定日 2026-08-21、
    測定点 58c779af。前回は 35、測定日 2026-08-11、測定点 023305c8）。10 日で +31。
    これは 2d3cbda2（dead session の worktree/branch を回収する GC が無い）の在庫が実測で増え続けている
    ということであり、**この軸を後回しにするコストが観測された**。CLAUDE.md 第8節が worktree を義務化した
    以上、GC の不在は単調に積む。

(2) **在庫は測れるが、DoD9 の分子（是正完了）は 10 日間 動いていない。**
    (i) 監査済み **7 of 22**、(ii) 是正完了 **3 of 22** — 2026-08-21 に再測して再現（継承値ではない）。
    再測手順: `ls docs/audit-*-verdict-paths.md | wc -l` → 7。ただし blastguard の監査 doc は
    冒頭で自ら **逐語「この監査は DoD9 の分子を動かさない」「分類は部分的」「charter の分子は 3 of 22 の
    まま据え置く」** と宣言しているので監査済みに数えない（7 − blastguard + docs/autoflow-verdict-audit.md
    = 7）。分母 22 は `grep -rln 'harness_core::verdict::' crates/*/src/ | cut -d/ -f2 | sort -u | wc -l`
    で再測して不変。**部分的な監査を『監査済み』と数えないのがこの節の要点**であり、その規律は
    blastguard 監査 doc 自身が守っている。

(3) **north_star の射程に入った shell 層は、分類がまだ 1 本も無い。**（今回追加、DoD13）
    ゲートスクリプト **65 本**のうち失敗握り潰しの語法を含む shell が **5 本**（測定コマンドは DoD13 に記載、
    測定日 2026-08-21、測定点 58c779af）。3 分類は未着手＝**未判定 65 本**。
    Rust 側（DoD6 の 48 サイト）と違い、こちらは baseline すら無い。

【この節の数字の測定（毎回測り直すこと。継承した数字は測定点が付いていても腐る）】
- raw-IO ratchet baseline = **48**（不変）— `cat scripts/check-raw-io-ratchet.baseline` および
  `python3 scripts/check-raw-io-ratchet.py`。測定日 2026-08-21、測定点 58c779af。
  ゲート自身の出力は `raw-io-ratchet: count == baseline (48); no new raw-IO call, floor held.`
- DoD9 の分母 = **22**（不変）、(i) 監査済み **7**、(ii) 是正完了 **3**。測定日 2026-08-21、測定点 58c779af。
- repo ストア `.backlog/tasks.toml` = **682 件**（pending **341** / done 325 / failed 8 / cancelled 6 /
  claimed 2）— `grep -c '^\[\[task\]\]' .backlog/tasks.toml` および
  `grep -oP '^status = "\K[^"]+' .backlog/tasks.toml | sort | uniq -c`。測定日 2026-08-21、測定点 58c779af。
  前回 pending は 206（2026-08-11、測定点 412c7656）なので **+135**。
- machine-local ストア `~/.backlog/tasks.toml` = **859 件**、mtime **2026-08-21 13:02**（＝今日も書かれている）
  — `grep -c '^\[\[task\]\]' /home/hiroyuki_nakayama/.backlog/tasks.toml` および
  `ls -la /home/hiroyuki_nakayama/.backlog/`。測定日 2026-08-21、測定点 58c779af。
  **ストア分裂は未解消**。ただし parked に書いてあった「~/.backlog 668 件 と repo .backlog 14 件」という
  数字は stale であり、3e3a9a48（pending 54 件を repo へ移設）と 89feaddb（キューのスコープを project 単位へ）
  が landed した後の実態は 859 / 682 である。**主張（分裂している）は成立、数字は誤り**だったので訂正した。
- session-* worktree = **66**（前回 35）。測定コマンドは (1) に記載。測定日 2026-08-21、測定点 58c779af。
- shell ゲートスクリプト **65 本** / 失敗握り潰し語法を含む shell **5 本**。測定コマンドは DoD13 に記載。
  測定日 2026-08-21、測定点 58c779af。

【status=done を達成の証拠にしない】この規律は前版から維持する。b4baf3d7 は status=done だが
チケット本文が逐語で「この close は作業完了を意味しない」と述べており、実装は b0cacd15（pending）側にある。
DoD11 の第二層（字句ゲート）は未配線のまま — そして DoD13 はその同じ第二層に属する仕事である。
在庫の状態と成果物の状態は別軸である。

【今サイクルで観測した、north_star に直接効く実測 2 件】e2a22f3b（SessionStart banner の 4 source が
hook 経路で全滅していたのは、bare 名 spawn が `~/.cargo/bin` の stale copy に暗黙依存していたため。
`plugin_bin` で cache 優先・三値の resolver へ移行）と f9bba02a（DoD13 の根拠 (a)）。
前者は本日の SessionStart で **実 hook 経路から 4 source すべての描画を初めて目視確認**した（前サイクルまでは
deploy 済みバイナリの直叩きでしか観測できていなかった）。

【DoD13 の根拠となった逐語（DoD 本文から移設 — check_dod_refs が引用中の glob を
「存在しないパスへの参照」と誤検出するため。ゲートを黙らせたのではなく、逐語の置き場所を変えた）】
f9bba02a が直した 1 行は次のとおり:

    repofile=$(ls "$REPO"/crates/*/bin/"$base" 2>/dev/null | head -n1 || true)

このループは `shopt -s nullglob` の下で回る。staged な `<crate>/bin/<name>-linux-x86_64` を持つ crate は
39 のうち 2 本だけ（condukt と overwatch）なので、残り 37 では glob が何にもマッチせず**語ごと消える**。
結果 `ls` は引数 0 個で呼ばれ、**失敗せずに `$PWD`（repo root）を列挙して成功する**。`head -n1` が
C locale 順の先頭 `CLAUDE.md` を返し、直後の `cp -f "$src" "$repofile"` が
`cp -f target/release/<plugin> CLAUDE.md` になった。`2>/dev/null` は何も隠していない（ls は成功している）し、
`|| true` も無効である。つまり**解決の失敗**が**もっともらしい答え**へ黙って書き換えられ、
その答えが破壊的な書き込み先として使われた。

## next_action
【2026-08-21 更新 — ONE は前版から変えない。理由を明記する】
前版（2026-08-17）の ONE である **fe84d350 + 5aaecbf6** を維持する。変えない理由は 2 つあり、どちらも実測:
(a) 3 件（e99f61e4 / fe84d350 / 5aaecbf6）は今も pending で、**前サイクルの一手が消化されていない**。
(b) その間に session-* worktree が 35 → 66 へ増え、**後回しのコストが観測された**（current_gap (1)）。
消化していない ONE を放置して新しい糸（DoD13）へ乗り換えるのは、焦点保護の逆である。

手順は前版から変わらない: (1) `HARNESS_PROGRESS_WINDOW_SECS` のような環境変数で progress window を
注入可能にし、テストが実時間の経過を待たずに occupancy を観測できるようにする。(2) 5aaecbf6 が要求する
「何が worktree の DEATH を積極的に立証するか」の規則を reconcile に実装する（生死不明は **削除しない側**
へ倒す — この軸では制限側の向きが反転することを north_star が明記している）。(3) その上で fe84d350
（condukt run state が claim しない worktree を無条件に dead と扱う欠陥）を閉じる。
両方とも人間裁定済み・RED テスト委託済みなので、これから書くのは判断ではなく実装。
**利害のない agent に RED を先に書かせる**こと（CLAUDE.md 2.(a)）— 前サイクルの記録では
この条件が満たせなかったことが明示されているので、今回は着手前にそこを確保する。
right_size: m（condukt run 1 本、reconcile 内の occupancy 判定 + 2, 3 本のテスト）。

【この ONE の後に続く候補（今回は commit しない）】
- 2d3cbda2（dead session の worktree/branch を回収する GC 自体が無い）— current_gap (1) の 66 という
  実測が指している当の欠陥。fe84d350/5aaecbf6 が DEATH の規則を確定させた**後**に着手するのが順序として正しい
  （削除の判定基準が無いまま GC を書くのは、不可逆な側へ倒す実装になる）。
- 5daf0b60（abandon --all-stuck が生存 worker を liveness/dirty 未確認で切り離す）、b637936f（stuck_task_ids の三値化）。
- **DoD13 の字句ゲート**（shell 層）。今サイクルで north_star の射程に入れたが、ONE にはしない。
  65 本の 3 分類から始まる仕事で、上の liveness ONE とは別モジュール・別機構。parked へ回す。

## parked
- 【2026-08-21 追加】DoD13 の実装 — shell 層の erasure を検出する pre-commit 字句ゲート。今サイクルで north_star の射程に入れた（人間裁定）が、ONE は liveness 側を維持したため parked。着手時の第一歩は 65 本の 3 分類。アンチ空虚の対照として f9bba02a の逐語形（nullglob で消えた glob → 引数 0 個の `ls`）が RED として落ちることを先に観測する。
- blastguard の per-gate verdict 経路監査の**完了**（census 実測 196 production サイト、機械列挙は完了・分類は部分的）。監査 doc は docs/audit-blastguard-verdict-paths.md に存在し、自ら「分子を動かさない」と宣言している。未分類の範囲は同 doc §5 に列挙済み。
- 『取得失敗が空集合に潰れて allow になる』横断パターンの一括是正。reviewgate の P1 と propguard の F-2 で 2 crate 確認済み、監査済み 7 crate を横断で当たれば追加で出る見込み。DoD9 の (ii) を実際に動かす唯一の候補。
- 残る未監査 GATE crate: specguard と overwatch。
- backlog ストアの分裂。**2026-08-21 に数字を訂正**: machine-local `~/.backlog` = 859 件（今日も書かれている、mtime 2026-08-21 13:02）/ repo `.backlog` = 682 件。前版の「668 件 と 14 件」は 3e3a9a48・89feaddb が landed する前の値で stale だった（主張は成立、数字は誤り）。backlog 5ba13c3e / e840c8b2。ユーザー決定により現時点では可視化のみで、全面移行はしない。
- blastguard の残機会 2 件 — `"$D/bin/tool"` のように参照へテキストが貼り付いた head 9 件（backlog 2fb05132）と、行外定義の shell 関数という残存 fail-open（backlog 5700ba25）。実測 2026-08-07。
- 【2026-08-21 追加】abba6f0d — hook から harness バイナリを bare 名で spawn する残り 10 箇所を `plugin_bin::resolve` へ移す。e2a22f3b で overwatch の 1 箇所だけを実測に基づいて直し、残りへは外挿しなかった。

