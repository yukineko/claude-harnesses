## north_star
このリポジトリのゲートが「判定不能」を「clean」に写せない状態を、型と決定論ゲートで機械的に保証する。3つの機構のうち2つは実測で達成済み(測定日2026-07-22 測定点470737c5)。(a) enforce側 達成: check-fail-open --all の残存swallowが34から29へ減り、baselineファイル scripts/check-fail-open.baseline を29でpinした--ratchetゲートがdrift(count不一致)で非0終了し単調非増加を強制する。(b) host非依存blocking 達成: fail-open doc-claims test-weakening version-lockstepの4スキャナがpre-commitフック(githooks配下 opt-in)でblockingとしてcommitを止め、--no-verifyもpost-commit post-mergeのledgerとpre-pushで塞がれ、GitHub機構(ruleset)に依存しない。CI ruleset は方針7に従いadvisoryへ降格(実測 gh api rulesets 空配列)。残る1機構が(c) 型契約: harness_core::verdict の型契約を採用したゲートcrateが9crate中5crate(blastguard propguard reviewgate donegate tdd)。tddは測定日2026-07-22 測定点65256061で新規採用を確認(直前のdonegate単独採用から1crate進んだ)。ゴールは、fail-openが型で書けず 決定論ゲートで通れず 残存量が単調減少していると観測できる状態にすること。残差は型契約の9crate中5crateから9crate中9crateへの展開に集約された。

## definition_of_done
- harness_core::verdict の型契約を採用したゲートcrateが 9crate中5crate から 9crate中9crate へ増える。残り4crateは specguard stuckguard mutategate overwatch。検査なしにCleanを構築するコードがtrybuildのcompile-failテストで固定されている(現況 9crate中5crate。blastguard propguard reviewgate donegate tddは採用済み。測定日2026-07-22 測定点65256061。確認コマンド: 各crateのsrc配下を grep -rl して harness_core verdict の利用有無を見る)
- [達成 2026-07-22 測定点470737c5] fail-openのenforceはGitHub required status checkではなくlocal pre-commit(blocking)へ移した(コミット1a59f09a)。4スキャナがpre-commitフックで非0終了しcommitを止めることを使い捨てworktreeで観測。旧DoDの『required4本が赤でmergeを止める』は当時PR21で観測した真の記録だが、方針7(ゲートを外部サービスに預けない)により機構ごと撤去済み(実測 gh api rulesets 空配列)。ただしclassic branch protectionの有無はPAT権限不足で未確認
- [達成 2026-07-22 測定点470737c5] check-fail-open --all の指摘件数が34から29へ減り、baselineファイル scripts/check-fail-open.baseline を29でpinした--ratchetがcount一致を保持し、driftで非0(regression lock-in双方)として単調非増加をゲートする。実測--ratchet終了コード0でfloor held
- [達成 2026-07-22 測定点470737c5] GitHub固有機能に依存しない層で同じ阻止が効く。pre-commitフックはopt-in(既定無効: fresh cloneはcore.hooksPath未設定でフック空、弱体化がそのままcommit成立するのを観測)で、core.hooksPathの向き先を切り替えれば4スキャナがblockingとして効くことを観測。CodeCommit等required-status-check非依存のホストでも同じpre-commitが効く

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
DoD1が9crate中5crate(blastguard propguard reviewgate donegate tdd採用済み、測定点65256061)。残り4crate(specguard stuckguard mutategate overwatch)をharness_core verdictへ移行し、trybuild compile-failでClean偽造不可能性を固定するのが最大レバレッジ。共有三値型の設計判断は既に5crateで実績展開済みのため先行検討不要、直接展開でよい。

## next_action

## parked
- 旧DoD2 GitHub required status checkでmergeをブロックする件: 方針7(ゲートを外部サービスに預けない)により機構撤去。CI rulesetはadvisoryへ降格し、enforceはlocal pre-commitへ移管済み。再登録は方針7に反するため意図的に行わない。
- 旧north_star 出荷済み並列衝突ハードニングのvalidate閉環。overwatchはdrift無しでlive。残るのはruntime-conflict merge-hold contended-skipの時間窓集計surfaceとbefore-after deltaのevidence化。

