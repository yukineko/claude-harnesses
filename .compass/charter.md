## north_star
review-redesign の最後の未接続=Continuous-Audit の finder→verifier→record→review-queue ループを実接続する。現状 scripts/continuous-audit.sh は決定論レコーダ止まりで、CONFIRMED findings は人間が手で --finding 指定しないと review-queue に載らない(自動供給ゼロ)。これを『駆動 SKILL が1ラウンド回すと gate crates への敵対的 finder→refute-verifier が CONFIRMED subset を出し、それが自動で review-queue に溜まる』状態にする。反復ラウンドで同一 finding が重複しない冪等性(dedup)まで含めて初めてループが実用になる(build≠validate)。

## definition_of_done
- 駆動 SKILL を新設する: crates/overwatch/skills/continuous-audit/SKILL.md が、gate crates(blastguard/propguard/specguard/stuckguard/mutategate)に対し1ラウンド(敵対的 finder→refute ベース verifier→CONFIRMED subset 抽出)を回し、各 CONFIRMED を scripts/continuous-audit.sh --finding 経由で record し audit-round も記録する手順を規定する。crates/overwatch/.claude-plugin/plugin.json の skills に登録され、.claude-plugin/marketplace.json の overwatch エントリにも反映される(grep で SKILL.md 存在 + 両 manifest 登録を確認)
- record-finding を finding-id で冪等化する: 同一 finding-id を複数ラウンドで record しても overwatch review-queue には最新1行のみ出る。dedup は review_queue.rs の build_queue(または store 読み戻し)で finding-id キーに最新 ts を残す純関数として実装し、systemic/rollback ストリームには影響しない
- 決定論テストを追加する: 同一 finding-id を2回 record→review-queue が1行に畳まれることを隔離ストアで assert。異なる id は畳まれないことも別ケースで検証。既存 continuous_audit_script.rs / review_queue テストは非回帰。fail-soft(ストア欠如/空/破損→空集合)不変条件を維持
- overwatch を micro lockstep bump(現行→次)で3正典(crates/overwatch/Cargo.toml / .claude-plugin/plugin.json / .claude-plugin/marketplace.json の overwatch エントリ)同時。python3 scripts/check-plugin-versions.py と python3 scripts/check-version-bumped.py が green、cargo fmt と cargo clippy -p overwatch --all-targets が -D warnings で clean、cargo test -p overwatch green
- 検証(build≠validate): 別モデルで独立に、dedup 不変条件(同一 id→1行・別 id→畳まない)・SKILL の finder→verifier→record 契約が scaffold の --finding/--round 引数と整合すること・fail-soft 後方互換(既存 systemic/rollback ストリーム不変)・全 gate green を再確認する。注意: この ONE は『ループを実接続し自動供給を成立させる』ところまで。converge(round 越しの new-findings 減少)の longitudinal 実証はデータ蓄積後の measure step(スコープ外)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(Continuous-Audit ループが実接続され CONFIRMED findings が review-queue に自動供給される) − 現状 の最大差分: 決定論レコーダ(scripts/continuous-audit.sh)・record-finding store・review-queue 統合・audit-round ledger は全て landed 済みだが、(1)それらを回す駆動 SKILL が無く findings は手動 --finding 指定のみ=自動供給ゼロ、(2)review_queue.rs の build_queue に finding-id dedup が無く反復ラウンドで同一 finding が重複してループが実用にならない。最小 validate スライス(size m・純加算) = crates/overwatch/skills/continuous-audit/SKILL.md(gate crates への finder→refute-verifier→CONFIRMED→scaffold record を規定)を新設し両 manifest 登録、record-finding を finding-id で冪等化(build_queue で最新 ts を残す純関数)し、同一 id 2回→review-queue 1行を決定論テストで固定、overwatch を micro lockstep bump。これで自動供給が成立し converge の longitudinal 実証(measure step)へ橋渡しできる。

## next_action

## parked

