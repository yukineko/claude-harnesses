## north_star
慢性的に赤い CI の検知を advisory から blocking へ格上げし、scripts/check-ci-red.py の判定が push を実際に止めるようにする。根拠(2026-07-22 実測): 直前の north_star は「advisory 表示は『誰かが見て直す』を前提にしており、そのギャップを埋める」として 2 workflow を人手で修理し、gh run list で green 化を確認して閉じた(outcome #39 Forward)。しかし修理したのは症状であって機構ではない — .githooks/pre-push は今も check-ci-red の exit code を読んだ上で最後に必ず exit 0 し、慢性赤を一行の警告として出すだけである。同じ advisory-only の欠陥は既に rollout-drift で一度顕在化し、scripts/check-plugin-rollout.py を pre-push の blocking 検査へ格上げすることで解消された(git log 75b91230 周辺)。今回はそれと同じパターンを chronically-red CI に適用し、17 連続失敗が誰にも止められないまま積み上がる状態を機構として不可能にする。

## definition_of_done
- check-ci-red が exit 1(慢性赤を確定) を返したとき、pre-push フックが非0で終了し push が実際に止まることを、使い捨てリポジトリまたは checker をスタブした状態で観測している。観測前の RED(格上げ前は同じ入力で push が通っていたこと)も併せて記録する。
- exit 3(判定不能) は意図的に advisory のまま残す。これは CLAUDE.md の第3節『判定不能は制限側へ』に対する明示的な carve-out であり、第7節『ゲートを外部サービスに預けない』を優先した結果である — exit 3 で push を止めると GitHub API の可用性が push 権限を握ることになり、オフラインで push できなくなる。この carve-out の理由が pre-push フックのコメントと charter の両方に明文で残っており、散文が実挙動と一致している。
- exit 0 以外のすべての未知 exit code が exit 1 と同じ制限側に写ることを観測している。checker がクラッシュしたり将来 exit code を増やしたりしたときに、それが黙って all-clear として通る経路が存在しない。
- 格上げ後も既存の逃げ道の性質が変わっていないことを確認する。PREPUSH_SKIP_CI_RED を 1 にした場合はネットワーク呼び出しごと skip される(従来どおり)。この環境変数が恒常的な迂回として使われていないことを、その旨をコメントで明記して残す。
- blocking 化した pre-push が既存の他検査(rollout-drift・bypass ledger)と共存し、既存の pre-push テストスイートが green である。慢性赤が無い通常状態では push が従来どおり通ることを観測している(全部止める gate は判定していないのと同じ)。

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
機構は既にほぼ揃っており、残差は一箇所に集約されている。scripts/check-ci-red.py は既に三値の exit 契約(0=判定済み慢性赤なし、1=慢性赤、3=判定不能)を持ち、クラッシュや不正な閾値を 1 ではなく 3 に写す配慮まで実装済み。.githooks/pre-push もその exit code で分岐し(テキスト一致ではなく exit code で分岐する旨のコメントあり)、未知 exit code を『NOT an all-clear』と明記している。欠けているのは最後の一手だけ — 分岐したあと結局すべての枝が同じ exit 0 に合流している。したがって次の一手は『exit 1 と未知 exit code を非0終了へ写し、exit 3 だけを carve-out として advisory に残す』という pre-push の終了経路の書き換えと、その挙動を固定するテストである。規模は小さいが、格上げ前に push が通っていたことを先に観測しないとテストが何も証明しないので、RED の観測が必須。

## next_action
condukt へ渡す。pre-push の終了経路を書き換える前に、慢性赤を模した入力で push が現状は通ることを観測して RED として記録し、その後 blocking 化して同じ入力で止まることを確認する。

## parked
- 旧 north_star(2026-07-22 達成): 慢性的に赤い 2 workflow(semver-checks・build & commit plugin binaries)の実修理。overwatch を 0.2.0 へ引き上げて semver 規約に合わせ、8 crate の整形崩れを直した。gh run list で両 workflow の green 化を確認済み(outcome #39 Forward)。
- 旧 DoD2 GitHub required status check で merge をブロックする件: 第7節により機構撤去。CI ruleset は advisory へ降格し、enforce は local pre-commit へ移管済み。再登録は第7節に反するため意図的に行わない。
- 旧 north_star 出荷済み並列衝突ハードニングの validate 閉環。overwatch は drift 無しで live。残るのは runtime-conflict merge-hold contended-skip の時間窓集計 surface と before-after delta の evidence 化。
- specguard・stuckguard・overwatch の harness_core verdict 非適合判定(2026-07-22): 型を無理に統合するのではなく、必要なら別の共有型で個別に判断する。今は着手しない。
- [達成 2026-07-22] fail-open ゲート機構の型・enforce・host 非依存化(旧 north_star)。DoD1 は構造適合 6 crate で完了、check-fail-open --all は 0 件、enforce は local pre-commit。詳細は git log 43ce376a 周辺を参照。
- [達成 2026-07-22] plugin rollout drift の機械的 blocking 化(旧 north_star)。pre-push の rollout-drift 検査が push を block する。詳細は git log 75b91230 周辺を参照。今回の north_star はこれと同じパターンの適用である。
- donegate の harness_core verdict 移行(6d4312c5)は出荷済みだが condukt run-20260722-072334 は 0 of 4 verified。独立 verifier 未実行、trybuild compile-fail タスク未着手。ただし DoD1 自体は別セッションが構造適合 6 crate で閉じており、主筋ではないため保留。
- backlog 6267bfbe(p1): bypass ledger が git commit --amend を未検証コミットとして誤記録する。誤検知側なので危険ではないが、amend は日常操作なのでゲートの狼少年化を招く。単発 fix として backlog に残す。

