## north_star
このリポジトリのゲートが「判定不能」を「clean」に写せない状態を、型と決定論ゲートで機械的に保証する。3 つの機構のうち 2 つは実測で達成済み(測定日 2026-07-22 / 測定点 470737c5): (a)【enforce 側・達成】check-fail-open --all の残存 swallow が 34→29 へ減り、scripts/check-fail-open.baseline=29 を pin した --ratchet ゲートが drift(count!=baseline)で非0終了し単調非増加を強制する。(b)【host 非依存 blocking・達成】fail-open/doc-claims/test-weakening/version-lockstep の 4 スキャナが .githooks/pre-commit で blocking として commit を止め、--no-verify も post-commit/post-merge の ledger + pre-push で塞がれ、GitHub 機構(ruleset)に依存しない。CI/ruleset は §7 に従い advisory へ降格(実測 gh api rulesets=[])。残る 1 機構=(c)【型契約・未達 3/9】harness_core::verdict の型契約を採用したゲート crate が 9 件中 3 件(blastguard, propguard, reviewgate)のみ。ゴールは、fail-open が(a)型で書けず・(b)決定論ゲートで通れず・(c)残存量が単調減少していると観測できる状態にすること。残差は(c)型契約の 3/9→9/9 展開に集約された。

## definition_of_done
- harness_core::verdict の型契約を採用したゲート crate が 3 件から 9 件へ増える。残り 6 crate は specguard, stuckguard, mutategate, overwatch, donegate, tdd。検査なしに Clean を構築するコードが trybuild の compile-fail テストで固定されている(現況 3/9: blastguard, propguard, reviewgate は採用済み)
- [達成 2026-07-22 / 470737c5] fail-open の enforce は GitHub required status check ではなく local pre-commit(blocking)へ移した(1a59f09a)。4 スキャナが .githooks/pre-commit で非0終了し commit を止めることを使い捨て worktree で観測。旧 DoD『required 4 本が赤で merge を止める』は当時 PR #21 で観測した真の記録だが、§7(ゲートを外部サービスに預けない)により機構ごと撤去済み(実測 gh api rulesets=[])。ただし classic branch protection の有無は PAT 403 で未確認
- [達成 2026-07-22 / 470737c5] check-fail-open --all の指摘件数が 34→29 へ減り、scripts/check-fail-open.baseline=29 を pin した --ratchet が count==baseline を保持・drift で非0(regression/lock-in 双方)として単調非増加をゲートする。実測 --ratchet exit 0 で floor held
- [達成 2026-07-22 / 470737c5] GitHub 固有機能に依存しない層で同じ阻止が効く。.githooks は opt-in(既定無効: fresh clone は core.hooksPath 未設定で .git/hooks 空、弱体化がそのまま commit 成立するのを観測)で、core.hooksPath=.githooks を打てば 4 スキャナが blocking として効くことを観測。CodeCommit 等 required-status-check 非依存のホストでも同じ pre-commit が効く

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
DoD2/3/4 は閉じた(測定日 2026-07-22 / 測定点 470737c5): enforce は local pre-commit の blocking へ移り、fail-open 残存は 29 で --ratchet が単調非増加を強制し、opt-in の .githooks が host 非依存で効くことを観測済み。残る唯一の実質差分は DoD1: harness_core::verdict の型契約の採用が 3/9 で止まっている(未採用は specguard, stuckguard, mutategate, overwatch, donegate, tdd の 6 crate)。最大レバレッジはこの 6 crate を verdict 型へ移行し、検査なしの Clean 構築が trybuild compile-fail で固定される状態にすること。ただし backlog 42b7c9af が『任意の T を包む共有三値型はまだ存在しない・各 crate が三値を個別に再発明している・型による強制は未実装』と記録しており、9/9 展開の前提として共有三値型の設計判断(harness-core に据えるか)が先行しうる — 型契約の対象 crate ごとに verdict 経路の形が異なるため、まず 1 crate(例: donegate か overwatch)を移行して型の当てはめ可否を観測してから残りへ展開する。

## next_action

## parked
- 旧 DoD2『GitHub required status check で merge をブロック』: §7(ゲートを外部サービスに預けない)により機構撤去。CI/ruleset は advisory へ降格し、enforce は local pre-commit へ移管済み。再登録は §7 に反するため意図的に行わない。
- 旧 north_star: 出荷済み並列衝突ハードニングの validate 閉環。overwatch は drift 無しで live。残るのは runtime-conflict/merge-hold/contended-skip の時間窓集計 surface と before/after delta の evidence 化。

