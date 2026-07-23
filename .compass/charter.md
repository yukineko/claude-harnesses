## north_star
fail-open を『後から検出する』のをやめ、『そもそも書けない』側へ移す。harness_core の verdict 型は出力側でこれを既に達成している — Clean は private witness でしか作れず、Determination には unwrap_or も ok も Default も無いので、『判定できなかった』を『問題なし』へ潰す最短経路が型として存在しない。同じ技法を入力側へ広げる。

## definition_of_done
- ワークスペース直下の Cargo.toml が clippy の lints セクションを持ち、gate crate 側の Cargo.toml がそれを継承する。達成済み (blastguard・propguard・specguard・stuckguard・mutategate・overwatch の6crateが [lints] workspace = true を持つ、budgetguard・donegate・reviewgate・schemaguardの4個は集約しない理由がコード側コメントに明文で残るparked)。
- enforce の置き場所は local である。clippy ゲートを GitHub の required status check として登録しない。達成済み (CLAUDE.md 第7節どおりlocal pre-commit配線)。
- harness_core が fallible な入力境界のラッパを提供し、その返り値が既存の三値型 Determination である。達成済み (crates/harness-core/src/boundary.rs、ディレクトリ走査・ファイル読み出し・subprocess実行の3経路)。
- GATE_CRATES 内で標準ライブラリの生のディレクトリ走査・ファイル読み出し・subprocess 実行を直接呼んでいる箇所が機械的に検出され、検出時に local のゲートが非0で終了する。達成済み (scripts/check-raw-io-ratchet.py、.githooks/pre-commit に配線済み、commit adec7ae3。regexのboundary::修飾漏れをcommit 8df18f50で修正済み)。
- アンチ空虚の対照実験を記録している。達成済み (scripts/test_check_raw_io_ratchet.py の anti-vacuity control experiment、24 tests green)。
- baseline の内訳 (現在72件・specguard 37・overwatch 30・propguard 2・その他) の生IO呼び出しが、右サイズに分解した単位でharness_core内のboundaryラッパ経由へ実際に移行され、baselineが単調減少する。各移行はboundary移行前後でcargo testとclippyがgreenであることを観測してから確定する。mutategate(1)・stuckguard(4)・propguard(5のうち3)は移行済み(77→72、commit 74384163/ac4af8ef/470fc018、condukt run-20260723-161953-13614)。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
DoD1-5達成済み(outcome #28/#29)。DoD6は72件まで進捗(77から-5)。残る既知の未解決: (a) propguard gate.rs:871・git.rs:34の2箇所はharness_core::boundary::runがtimeout/プロセスグループkillを持たないため未移行 — 新primitive設計待ち(backlog 0e0f5249、human decision 2026-07-23)。(b) check-raw-io-ratchet.pyのCommand::new(検出はconstruction行ベースの文字列一致のため、boundary::run移行後もCommand::new(...)自体は残り続け、baselineに反映されない構造的欠陥(backlog 422508c3、stuckguard anchor.rs 3箇所は実質移行済みだが未反映のままcountされている)。(c) specguard(37箇所)・overwatch(30箇所)はサイズxlで未着手・要再分解。

## next_action
backlog 422508c3 (check-raw-io-ratchet.pyのCommand::new検出を.output()/.spawn()/.status()の直接呼び出しベースへ再設計) を先に解決する。理由: これを直さない限り、subprocess系のbaseline減少がoverwatch/specguard移行後も観測不能になり、DoD6の『単調減少を観測する』契約が測れなくなる。再設計後、正しいcountを再測定してbaselineをre-pinし、その後overwatch(30)・specguard(37)の再分解(size l、ファイル単位でさらに分解)に進む。

## parked
- propguard gate.rs:871・git.rs:34の2箇所の生Command実行をharness_core::boundaryのtimeout対応primitive経由へ移行する (backlog 0e0f5249、新primitive設計が前提)。
- overwatch(30箇所)の生IO呼び出しをharness_core内のboundaryラッパ経由へ移行する (size l — ファイル単位でさらに分解が要る。対象は主に aggregate.rs と bridge.rs)。
- specguard(37箇所)の生IO呼び出しをharness_core内のboundaryラッパ経由へ移行する (size l — ファイル単位でさらに分解が要る)。
- 手貼りのdeny(clippy::panic)残り5個 (budgetguard・donegate・reviewgate・schemaguard) の集約。各crateをworkspace lintsにopt-inさせるとunwrap_used・expect_usedのdenyも同時に効くため、それら4crateのproductionのunwrap・expectを潰すかexpectで正当化する作業が伴う。
- condukt テストのPATH env race修正 (backlog b0db2bff、本north_starと無関係の別スレッド)。
- /mnt/c/tmp/orphan-worktree-quarantine/smb-share-hardening の最終処分判断 (backlog 7d71e1bd、削除でなくquarantine移動のみ、本north_starと無関係)。

