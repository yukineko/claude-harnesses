## north_star
main上で継続的に赤いままの2 workflow(semver-checks・build & commit plugin binaries)を実際に修正し、scripts/check-ci-red.pyがadvisoryで検知するだけで誰も手当てしない状態を解消する。根拠(2026-07-22実測、gh run viewで実ログ確認済み): (1) semver-checksは17回連続失敗(2026-07-21から)。overwatch v0.1.52がAuditRound.unverified・RoundMetric.unverified・AuditMetrics.cumulative_unverified・ReviewFinding.verdictの4フィールドをexternally-constructible structへ追加したが、0.1.50→0.1.52はminor(第2桁)を跨がないpatchのみの変更として扱われ、cargo-semver-checksのメジャー未満バージョン規約(breaking変更は第2桁を上げる)に違反していた。(2) build & commit plugin binariesは12回連続失敗(同じく2026-07-21から)。本日のfail-open burn-downセッションで touched した9crate中8crate(compass・condukt・ctxrot・deepwiki・harness-status・playbook・runbook・ship)の変更ファイルがcargo fmt --all --checkの整形規則から外れており、smoke workflowのfmtゲートが継続的に落ちていた。pre-pushのchronically-red CI検査は既にこの2件をadvisoryで表示していたが(charter parked参照)、advisory表示は「誰かが見て直す」を前提にしており、今回そのギャップを埋める。

## definition_of_done
- [達成 2026-07-22] overwatchのバージョンを0.1.52から0.2.0へ引き上げ済み(crates/overwatch/Cargo.toml・crates/overwatch/.claude-plugin/plugin.json・.claude-plugin/marketplace.jsonの3ファイル同時)。コード変更なし、既に出荷済みのbreaking変更(pub field追加4件)に対しメジャー未満バージョン規約に沿ってversionを合わせただけ。cargo build --workspaceとcargo clippy -p overwatch --all-targetsがクリーンであることをローカルで確認済み(cargo-semver-checksバイナリ自体はローカル未インストールのため、gh run view --log-failedで実際のCI失敗ログ(constructible_struct_adds_field、1 major and 0 minor checks failed)を読んで原因を特定した)。
- [達成 2026-07-22] cargo fmt --all --checkがワークスペース全体でクリーンになったことをローカルで確認済み(修正前はcompass・condukt・ctxrot・deepwiki・harness-status・playbook・runbook・shipの8crateで整形diffがあった)。
- [達成 2026-07-22] 上記9crate(overwatch含む)すべてを3ファイルversion lockstepでbumpし、python3 scripts/check-plugin-versions.pyとpython3 scripts/check-version-bumped.py --base（リモートmainブランチ基準）が両方OKで終了することを確認済み。scripts/rollout-plugins.sh --plugin <name>で各crateをlive plugin cacheへ反映済み(overwatchはGATE crateのため--canary経由)。python3 scripts/check-plugin-rollout.pyが「39 plugins deployed at their source version」でrollout driftゼロを確認済み。
- 未達: originへのpush後、GitHub Actions上でsemver-checksとbuild & commit plugin binariesの両方が実際にgreenで終了したことをgh run listまたはgh run viewで確認する。pushはユーザー承認が必要なため、この項目はpush実行と事後のCI確認が完了するまで達成としない。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ローカルでのfmt・version・rollout修正はすべて完了し検証済みだが、まだpushしていないためGitHub Actions側での実際のgreen化はまだ観測できていない。push実行がユーザー承認待ち。

## next_action
ユーザーにpushの可否を確認する(AskUserQuestion)。承認後にgit pushし、gh run list --branch main --workflow semver-checks.ymlおよびgh run list --branch main --workflow build-and-commit-plugin-binaries.ymlで新しいrunがgreenで終わることを確認してからDoD4項目目を達成とする。

## parked
- 旧DoD2 GitHub required status checkでmergeをブロックする件: 方針7により機構撤去。CI rulesetはadvisoryへ降格し、enforceはlocal pre-commitへ移管済み。再登録は方針7に反するため意図的に行わない。
- 旧north_star 出荷済み並列衝突ハードニングのvalidate閉環。overwatchはdrift無しでlive。残るのはruntime-conflict merge-hold contended-skipの時間窓集計surfaceとbefore-after deltaのevidence化。
- specguard・stuckguard・overwatch の harness_core::verdict 非適合判定(2026-07-22): 型を無理に統合するのではなく、必要なら harness_core::verdict::Determination<T> 等の別の共有型で個別に判断する。今は着手しない。
- [達成 2026-07-22] fail-openゲート機構の型・enforce・host非依存化(旧north_star)。DoD1は構造適合6crateで完了、check-fail-open --allは0件、enforceはlocal pre-commit。詳細はgit log 43ce376a周辺を参照。
- [達成 2026-07-22] plugin rollout driftの機械的blocking化(旧north_star)。.githooks/pre-pushのrollout-drift検査(scripts/check-plugin-rollout.pyのexit 1)がpushをblockする。詳細はgit log 75b91230周辺を参照。
- chronically-red CIの検知自体をadvisoryからblockingへ格上げするかどうかは、今回のnorth_starのスコープ外として意図的に保留する(今回は「今赤い2件を直す」だけに絞る)。今回のDoDが閉じたあとの次のnorth_star候補になりうる(rollout-drift同様、advisory検知→blocking化のパターン)。

