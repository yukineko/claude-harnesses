## north_star
このリポジトリのゲートが「判定不能」を「clean」に写せない状態を、型と CI で機械的に保証する。現状は展開が途上: harness_core::verdict の型契約は 9 ゲート crate 中 3 件(propguard, reviewgate, blastguard)のみ、check-fail-open の指摘は 34 件のまま(かつスキャナ自身が exit 0 で単調性を強制していない=検出はするが gate していない)。CI 側は 4 本(fail-open-scan, doc-claims-scan, test-weakening-scan, version-lockstep-check)が required status check として稼働し、赤いとき merge が実際に止まることを観測済み。ただしこれは GitHub 上の main を守るだけで、local main は依然として無防備(条件は push 時にしか評価されない)。ゴールは、fail-open が型で書けず・CI で通れず・残存量が単調減少していると観測できる状態にすること。

## definition_of_done
- harness_core::verdict の型契約を採用したゲート crate が 3 件から 9 件へ増える。対象は blastguard, propguard, specguard, stuckguard, mutategate, overwatch, donegate, reviewgate, tdd の 9 crate。検査なしに Clean を構築するコードが trybuild の compile-fail テストで固定されている
- [達成 2026-07-22] workflows 配下の 4 本が required status check に登録され、いずれかが赤いとき merge が実際にブロックされることを観測した。証拠: PR #21 で doc-claims-scan のみ FAILURE のとき mergeable=MERGEABLE かつ state=BLOCKED、偽の引用を消して 4 本 SUCCESS にすると state=UNSTABLE(merge 可)へ反転した
- check-fail-open スキャナを --all で走らせたときの指摘件数が 34 件から減少し、その残数が CI で単調非増加としてゲートされている(新規追加が赤くなる)
- GitHub 固有機能に依存しない層でも同じ阻止が効く。ホスト非依存の blocking gate を opt-in(既定無効)で用意し、CodeCommit など required-status-check の無いホストでも fail-open を止められることを観測する

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
DoD2 は閉じた(required 4 本が稼働し、赤で merge が止まることを観測)。残る差分は 3 つ: (1) 型契約の採用が 3/9 で止まっている、(2) check-fail-open が 34 件を検出しながら exit 0 で通す=検出はするが enforce しない fail-open が残っている、(3) 守られているのは GitHub の main だけで、成果物が最初に着地する local main は無防備(condukt は worktree から local main へ直接 FF merge する)。ローカル側の pre-commit/pre-push は常に exit 0 の advisory で、core.hooksPath 未設定なら動かず --no-verify で外せるため、ゲートとして数えられない。最大レバレッジは (2): baseline pin + 増加時 nonzero の単調 gate に変えれば、34 件が減る方向にしか動かなくなる。

## next_action

## parked
- 旧 north_star: 出荷済み並列衝突ハードニングの validate 閉環。DoD1(canary rollout で live 化)は実測上ほぼ達成済み(overwatch は drift 無しで live)。残るのは overwatch に runtime-conflict と merge-hold と contended-skip の時間窓集計 surface を足すことと、before/after delta の evidence 化。

