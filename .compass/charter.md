## north_star
このリポジトリのゲートが「判定不能」を「clean」に写せない状態を、型と CI で機械的に保証する。現状は道具が作られただけで展開されていない: harness_core::verdict の型契約は 9 ゲート crate 中 2 件(propguard/reviewgate)しか採用しておらず、check-fail-open.py --all は 34 件を指摘したまま、17 本ある CI workflow は 1 本も required status check になっていない(赤くても merge できる)。ゴールは、fail-open が型で書けず・CI で通れず・残存量が単調減少していると観測できる状態にすること。

## definition_of_done
- harness_core::verdict の型契約を採用したゲート crate が 2 件から 9 件(blastguard/propguard/specguard/stuckguard/mutategate/overwatch/donegate/reviewgate/tdd)へ増え、検査なしに Clean を構築するコードが trybuild の compile-fail テストで固定されている
- fail-open.yml / doc-claims.yml / test-weakening.yml / version-lockstep.yml が branch protection の required status check に登録され、いずれかが赤いとき main への merge が実際にブロックされることを 1 度観測している
- python3 scripts/check-fail-open.py --all の指摘件数が 34 件から減少し、その残数が CI で単調非増加としてゲートされている(新規追加が赤くなる)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
型契約(harness_core::verdict)も CI ゲート(17 workflow)も存在するのに、どちらも実効していない: 採用 2/9 crate、required status check 0 本、check-fail-open の指摘 34 件が横ばい。最大かつ右サイズの差分は「1 本目の required status check を実際に効かせること」。理由は、型契約の展開(9 crate 移行)は l 級で複数モジュール横断だが、required 登録は既存の緑の workflow を 1 本 required にするだけで、それ以降のすべての fail-open 修正が「赤ければ merge できない」という強制力を得る=最小コストで最大の擁護可能性を生む。ただし branch protection API は現行 PAT で 403 のため、トークン調達か Web UI 操作という人間側の一手が前提になる。それが解けない場合の代替は、型契約を 1 ゲート crate(blastguard か donegate)へ移行して 2/9 を 3/9 にし、compile-fail テストで固定する s〜m 級の一手。

## next_action

## parked
- 旧 north_star: 出荷済み並列衝突ハードニングの validate 閉環。DoD1(canary rollout で live 化)は実測上ほぼ達成済み(overwatch は drift 無しで live、condukt の drift は cfg(test) のみの 0.7.84)。残るのは overwatch に runtime-conflict / merge-hold / contended-skip の時間窓集計 surface を足すことと、before/after delta の evidence 化。

