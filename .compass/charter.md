## north_star
fail-open を『後から検出する』のをやめ、『そもそも書けない』側へ移す。harness_core の verdict 型は出力側でこれを既に達成している — Clean は private witness でしか作れず、Determination には unwrap_or も ok も Default も無いので、『判定できなかった』を『問題なし』へ潰す最短経路が型として存在しない。同じ技法を入力側へ広げる。

## definition_of_done
- ワークスペース直下の Cargo.toml が clippy の lints セクションを持ち、gate crate 側の Cargo.toml がそれを継承する。達成済み (blastguard・propguard・specguard・stuckguard・mutategate・overwatch の6crateが [lints] workspace = true を持つ、budgetguard・donegate・reviewgate・schemaguardの4個は集約しない理由がコード側コメントに明文で残るparked)。
- enforce の置き場所は local である。clippy ゲートを GitHub の required status check として登録しない。達成済み (CLAUDE.md 第7節どおりlocal pre-commit配線)。
- harness_core が fallible な入力境界のラッパを提供し、その返り値が既存の三値型 Determination である。達成済み (crates/harness-core/src/boundary.rs、ディレクトリ走査・ファイル読み出し・subprocess実行の3経路)。
- GATE_CRATES 内で標準ライブラリの生のディレクトリ走査・ファイル読み出し・subprocess 実行を直接呼んでいる箇所が機械的に検出され、検出時に local のゲートが非0で終了する。達成済み (scripts/check-raw-io-ratchet.py、baseline=77、.githooks/pre-commit に配線済み、commit adec7ae3)。
- アンチ空虚の対照実験を記録している。達成済み (scripts/test_check_raw_io_ratchet.py の anti-vacuity control experiment、20 tests green)。
- baseline=77 の内訳 (specguard 37・overwatch 30・propguard 5・stuckguard 4・mutategate 1) の生IO呼び出しが、右サイズに分解した単位でharness_core内のboundaryラッパ経由へ実際に移行され、baselineが77から単調減少する。各移行はboundary移行前後でcargo testとclippyがgreenであることを観測してから確定する。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
DoD1-5は達成済み(2026-07-23確認、outcome #28)。残る唯一のDoDは、baseline=77の生IO呼び出し(specguard 37・overwatch 30・propguard 5・stuckguard 4・mutategate 1)をharness_core内のboundaryラッパ経由へ移行しbaselineを単調減少させること。mutategate(1)+stuckguard(4)+propguard(5)=10箇所は合計しても小さく1タスクでs〜mサイズに収まるが、overwatch(30)とspecguard(37)はそれぞれ単体でxl相当のためさらに再分解が要る。

## next_action
mutategate(1箇所)・stuckguard(4箇所)・propguard(5箇所)、計10箇所の生IO呼び出しをharness_core内のboundaryラッパ経由へ移行する (size s)。移行前にcargo test -p mutategate -p stuckguard -p propguardがgreenであることを確認し、移行後も同じテストがgreenであること、およびscripts/check-raw-io-ratchet.pyのcountが77から67へ減ることを観測する。baselineファイルは67に再pinする。overwatch・specguardはそれぞれ別タスクとしてこの後に回す(park)。

## parked
- overwatch(30箇所)の生IO呼び出しをharness_core内のboundaryラッパ経由へ移行する (size l — ファイル単位でさらに分解が要る。対象は主に aggregate.rs と bridge.rs)。
- specguard(37箇所)の生IO呼び出しをharness_core内のboundaryラッパ経由へ移行する (size l — ファイル単位でさらに分解が要る)。
- 手貼りのdeny(clippy::panic)残り5個 (budgetguard・donegate・reviewgate・schemaguard) の集約。各crateをworkspace lintsにopt-inさせるとunwrap_used・expect_usedのdenyも同時に効くため、それら4crateのproductionのunwrap・expectを潰すかexpectで正当化する作業が伴う。
- condukt v0.7.99の未コミット変更 (plugin.json/Cargo.toml/marketplace.json/state.rs) をコミットする (backlog 3c2d5384、本north_starと無関係の別スレッド)。
- condukt テストのPATH env race修正 (backlog b0db2bff、本north_starと無関係の別スレッド、今セッションで発見)。

