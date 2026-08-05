## north_star
fail-open を『後から検出する』のをやめ、『そもそも書けない』側へ移す。ゴールは『判定不能を permissive な既定へ潰す最短経路が、レビューではなくコンパイラによって拒否される』こと。射程は 2026-08-05 に訂正した — 前版は『出力側（harness_core の verdict 型）では既に達成している。Determination には unwrap_or も ok も Default も無い』と書いていたが実測で偽。Determination::require が std::Result を返し、Result::unwrap_or は E 側に境界を持てないため、boundary::read_dir_entries(p).require().unwrap_or_default() が実 harness-core に対してビルド成功する（unwrap_or に空を渡す形と is_ok で真偽へ潰す形も同様に成功）。塞げるのは E 側ではなく T 側であり、boundary の payload が Vec と Option すなわち Default を持ち呼び出し側で自作可能なことが erasure を成立させている。したがって射程は入力側と gate 内部だけでなく harness_core 自身を含む。

## definition_of_done
- DoD1: ワークスペース直下の Cargo.toml が clippy lints を持ち gate crate が継承する。達成済み（6 crate が workspace 継承、非集約の 4 crate は理由をコード内に明文化して parked）。
- DoD2: enforce の置き場所は local である（clippy ゲートを required status check にしない）。達成済み（CLAUDE.md 第7節どおり local pre-commit 配線）。
- DoD3: harness_core が fallible な入力境界のラッパを提供し、返り値が三値である。【2026-08-05 に達成済みから部分達成へ格下げ】boundary モジュールは存在し走査・読み出し・subprocess の 3 経路を覆うが、boundary 自身の内部に 2026-07-24 に導入された fail-open が残る — 子プロセス出力の読み取り失敗と受信タイムアウトが空文字へ潰れ、その空文字が known な CommandOutput として下流へ渡る。完了条件は boundary 内部に判定不能を permissive へ潰す経路が 0 本であること。
- DoD4: gate crate 内の生の走査・読み出し・subprocess 直接呼び出しが機械検出され、検出時に local ゲートが非 0 で終了する。達成済み（raw-IO ratchet を pre-commit 配線、terminal method 検出に再設計済み）。
- DoD5: アンチ空虚の対照実験を記録している。達成済み（ratchet の対照実験 29 tests green）。
- DoD6: 生 IO 呼び出しの baseline が boundary ラッパ経由へ移行される。【2026-08-05 に converged 固定を撤回】前版は count==baseline を converged=達成として固定していたが、それは『減少が止まった』という観測であって達成ではない。現況 baseline=60（実測 2026-07-31 HEAD 30f6d7b5）。完了条件は残 60 サイトを 3 分類（既に fail-closed で機械的なもの、文書化された fail-soft 契約を持つもの、未判定）し、未判定を 0 本にすること。
- DoD7: fail-open reader が三値へ移行され確定数が単調増加する。既知集合 M1 から M4 は 4 件すべて確定して main に landed。
- DoD8: fail-open reader は raw-IO ratchet に現れない意味的 Result 崩壊も含むため、可能な箇所は boundary 経由へ寄せて DoD6 と同時前進させる。
- DoD9: 各 gate crate の verdict 経路が三値で表現され、silence・空集合・panic・IO・parse・subprocess 失敗を restrictive へ解決する。【計測単位を 2026-08-05 に改訂】分母 ＝ harness_core の verdict を参照するクレート集合、実測 22。第一分子 ＝ per-gate 監査ドキュメントが存在するクレート数、実測 7（schemaguard, autoflow, budgetguard, mutategate, propguard, stuckguard, reviewgate）。第二分子 ＝ それらの監査が発見した permissive 経路のうち三値化されて landed した件数。旧定義は第一分子だけを DoD9 の進捗としており、その結果 reviewgate の監査は permissive 経路を 8 件見つけて 1 件も是正しないまま分子を動かせた（propguard も F-2 と F-3 を未是正のまま数えている）。以後は両分子を併記し、DoD9 の達成条件は第二分子が第一分子の全 finding を覆うこと。各確定は利害のない agent が fault injection で RED を先に観測してから GREEN 化する。
- DoD10: 新規 gate crate（taintguard 等）は誕生時点から verdict 三値化の作法で実装される。taintguard は 0.1.0 で新規作成、0.1.1 で 3 ホール閉鎖、3rd trigger 配線済み。enabledPlugins 未登録で inert（backlog e4687aad で追跡）。
- DoD11: 【2026-08-05 追加】判定不能を permissive な既定へ潰す erasure が、レビューではなくコンパイルエラーとして拒否される。完了条件は、封印前に実測でビルド成功していた 3 形（require のあとに unwrap_or_default を呼ぶ形、unwrap_or に空の既定を渡す形、is_ok で真偽へ潰す形）が trybuild の compile-fail 固定として先に RED を観測され、封印後に GREEN へ移り、同時に正常系の positive control がビルドし続けること。既存の erasure 実地（stuckguard の config 読み出しと condukt の gate 実行）がコンパイルエラーとして露出することを観測で確認する。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト（各確定は利害のない agent の RED oracle を fault injection で先に観測してから GREEN 化して確定＝F→P。build≠validate）。【2026-08-05 追加】測定棒自体も検査対象とする — 分子や達成条件の定義を変えて数字を動かした場合は、変更日と旧定義での値を charter に併記する。定義変更による前進は前進として数えない。

## current_gap
北極星の前提節が実測で反証され、DoD 群が『出力側は達成済み』という偽の土台の上に建っていた。最大の差は harness_core 自身にある — Determination::require が std::Result を返すため、判定不能を permissive へ潰す 3 形が今ビルド成功する。塞ぐのは E 側ではなく T 側（boundary の payload 封印）で、封印が E0451 と E0277 を出すことは実測済み。並行して測定棒 2 本（DoD6 の converged 固定、DoD9 の監査ドキュメント分子）を是正済みだが、DoD9 の第二分子は現況 0 件で未計測。

## next_action
harness_core の型封印。Determination::require の返り値と boundary の payload を封印し、判定不能を permissive な既定へ潰す 3 形を E0451 と E0277 として拒否させる。手順: (1) 封印前の 3 形が現在ビルド成功することを trybuild の compile-fail 固定で RED として先に観測する、(2) payload を封印する newtype を導入して T 側を塞ぐ、(3) RED が GREEN へ移ることと positive control が通り続けることを観測する、(4) コンパイルエラーとして露出した既存の erasure 実地（stuckguard の config 読み出しと condukt の gate 実行）を三値のまま消費する形へ是正する、(5) 散文と実挙動の食い違い（verdict モジュールの docstring が unwrap_or は無いと書いている箇所）を同一コミットで是正する。

## parked
- blastguard の per-gate verdict 経路監査（census 実測 131 サイト、2026-08-04、測定点 1f0cf124）。CLAUDE.md 第3節が三値の正典例として名指ししている当の実装が一度も全経路列挙されていない。
- 『取得失敗が空集合に潰れて allow になる』横断パターンの一括是正。reviewgate の P1 と propguard の F-2 で 2 crate 確認済み、監査済み 7 crate を横断で当たれば追加で出る見込み。
- 残る未監査 GATE crate: specguard と overwatch。

