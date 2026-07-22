## north_star
fail-open を『後から検出する』のをやめ、『そもそも書けない』側へ移す。harness_core の verdict 型は出力側でこれを既に達成している — Clean は private witness でしか作れず、Determination には unwrap_or も ok も Default も無いので、『判定できなかった』を『問題なし』へ潰す最短経路が型として存在しない。同じ技法を入力側へ広げる。根拠 (測定日 2026-07-22 / 測定点 56872974): (1) ルート Cargo.toml にも各 crate の Cargo.toml にも lints セクションが1つも無く、clippy の unwrap_used/expect_used は既定 off なので gate crate でも素通りしていた。代わりに deny(clippy::panic) が15ファイルに手貼り。(2) GATE_CRATES の src 配下に生の fs::read_dir / fs::read_to_string / Command::new が97箇所。harness_core は verdict.rs を持つが fs/proc の境界ラッパを持たない。(3) 既にある機械ゲート check-fail-open.py は行単位のテキスト走査で、自身の docstring が『a line-level scan cannot tell whether the caller treats None as fail-open』と限界を明記している。検出器は本質的に上限を持つので、その外側を型で閉じる。

## definition_of_done
- ワークスペース直下の Cargo.toml が clippy の lints セクションを持ち、gate crate 側の Cargo.toml がそれを継承する。unwrap_used / expect_used / panic が gate crate で deny に なっていることを、違反を1件わざと書いて clippy が非0で終了するのを観測してから消す (RED を先に見る) ことで確認している。手貼りの deny(clippy::panic) のうち gate crate 側の 10個がワークスペース設定へ集約され重複が残っていない。残り5個 (budgetguard / donegate / reviewgate / schemaguard) は集約せず、集約しない理由 (その crate を workspace lints に opt-in させると unwrap_used / expect_used の deny も同時に効くため、別スコープの一手になる) が コード側のコメントに明文で残っている。この5個の集約は parked。
- enforce の置き場所は local である。clippy ゲートを GitHub の required status check として 登録しない。これは backlog 7ecf3797 の完了条件に書かれていた『clippy ジョブが required status check として列挙される』を意図的に採用しないという判断であり、理由は CLAUDE.md 第7節 (ブロックと許可を決める権限を外部サービスに預けない)。不採用の理由が backlog 項目と charter の 両方に明文で残っており、散文が実挙動と一致している。
- harness_core が fallible な入力境界のラッパを提供し、その返り値が既存の三値型 Determination である。少なくともディレクトリ走査・ファイル読み出し・subprocess 実行の3経路を覆う。新しい三値型を 作らず既存のものを再利用していること、および Result を bool へ潰す近道を型として提供していない こと (Default も From<bool> も unwrap_or も生えていないこと) をコンパイル失敗テストで固定している。
- GATE_CRATES 内で標準ライブラリの生のディレクトリ走査・ファイル読み出し・subprocess 実行を 直接呼んでいる箇所が機械的に検出され、検出時に local のゲートが非0で終了する。段階移行の ための許可リストを持ってよいが、その件数は baseline として固定され、増える方向の編集が ゲートで止まる (ratchet)。baseline の初期値は測定コマンドと測定点つきで記録されている。
- アンチ空虚の対照実験を記録している。上記の型とゲートが実在する fail-open を実際に捕まえる ことを、既知の未修正インスタンス少なくとも1件に対して観測している (導入前は緑、導入後は赤、修正後にまた緑)。何も検出しない検出器は常に緑なので、これが無ければ他の4項目は『通ること』しか証明しない。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
型は用意したが、まだ誰も使う義務を負っていない。boundary.rs は Determination で3つの入力境界を包んだが (未コミット)、GATE_CRATES には生の fs::read_dir / fs::read_to_string / Command::new が97箇所 (測定 2026-07-22 / 56872974) 残っており、新しく生の呼び出しを書き足すことを止めるものが何も無い。DoD4 の検出器と ratchet が無い限り boundary.rs は『使ってもよい代替』でしかなく、fail-open の入口は開いたままである。加えて DoD5 の対照実験がまだ無いため、仮に検出器を作っても『実在する fail-open を捕まえる』ことは未証明のまま。

## next_action

## parked

