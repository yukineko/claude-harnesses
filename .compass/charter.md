## north_star
specguard.toml の area 監査範囲を主要GATEクレート(blastguard・propguard・stuckguard・mutategate・overwatch)まで拡張し、42クレート中3クレートしか監査対象でない spec↔impl drift 監査の機構的ギャップを解消する

## definition_of_done
- specguard.toml に blastguard・propguard・stuckguard・mutategate・overwatch の5クレート分の area エントリが追加され、各エントリの canon が各クレートの README.md を指す
- specguard scope の出力に新規5エリア(blastguard・propguard・stuckguard・mutategate・overwatch)が現れ、既存の3エリア(condukt・specguard・harness-core)の出力に回帰がない
- python3 scripts/check-plugin-versions.py が引き続き exit 0 (specguard.toml は version-lockstep 対象外なのでバージョンbumpは不要)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
specguard.tomlは condukt・specguard・harness-core の3クレートのみを[[area]]として登録しており、42クレート中これら3クレート以外(blastguard・propguard・stuckguard・mutategate・overwatchを含む)はspec↔impl drift監査の対象外になっている。各GATEクレートにはREADME.md/README.ja.mdが既に存在しcanonとして使えるので、最小right-sizeな一手はspecguard.tomlに5クレート分の[[area]]エントリ(globs=crates/<name>/src/**、canon=crates/<name>/README.md)を追加し、specguard scopeの出力に新規エリアが現れ既存3エリアの動作に回帰がないことを確認すること。

## next_action

## parked

