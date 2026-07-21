# required status check が「赤いとき」に merge を止めることの観測

このファイルは **一時的な観測用**であり、この PR の中で削除される。

## 何を確かめているか

charter の DoD2 は次を要求している:

> 4 本が branch protection の required status check に登録され、いずれかが赤いとき
> main への merge が**実際にブロックされることを 1 度観測している**

登録済みであることは実測済み（ruleset 19438193、`enforcement: active`、
contexts が 4 本とも `89d54d56` の check-run 名と一致）。

一度ブロックは起きているが、それは「チェックが**まだ報告されていない**」ときの
ブロックだった（typo `test-weakning-scan` を required にしていた間、main への直接
push が `4 of 4 required status checks are expected` で拒否された）。DoD が要求して
いるのは「チェックが**赤い**とき」なので、まだ充足していない。

## 仕込んだ赤

下の行は存在しない行番号を引用しており、`scripts/check-doc-claims.py` が
`line-out-of-range` として exit 1 を返す。よって `doc-claims-scan` が赤くなる。

三値の verdict 型は `crates/harness-core/src/verdict.rs:99999` に定義されている。

（この主張は**意図的に偽**である。当該ファイルにその行は存在しない。ゲートが
これを検出できなければ、ゲートの側が壊れている。）

## 手順

1. この状態で PR を出し、`doc-claims-scan` が赤く、merge がブロックされることを観測する
2. 偽の引用を削除し、4 本すべて緑になって merge 可能になることを観測する
3. この観測ファイルごと削除して merge する
