## north_star
Continuous-Audit の収束シグナルを正直にする: audit-metrics の closure-rate と converging フラグが、実際に landed した回帰テスト(fix 側)を反映するようにする。現状 audit_round は finding 時に regression_tests_added を記録する(必然的に 0=まだ fix が無い)うえ ledger は append-only なので、確認済み findings を締めた 17 以上の回帰テストが実在しても closure はゼロ・converging は false のまま=ループが自分の核心的問い『fleet は硬化しているか?』に答えられない。fix 側を ledger にフィードバックして収束シグナルを実測に一致させる。

## definition_of_done
- closure フィードバック経路を追加する: overwatch の audit-round に、既存ラウンドの regression_tests_added を round-id で更新する決定論サブコマンド(close。--round と --tests を取る)を追加する。store の read-modify-write(該当ラウンドの record を読み、tests を加算または設定し、書き戻す純関数)で実装し、未知の round-id は明示エラーか no-op で fail-soft にする。観測可能条件: あるラウンドを tests ゼロで record したのち close で N に更新すると、audit-metrics がそのラウンドの closure-rate を N わる confirmed として表示する
- 決定論テストを追加する(RED から GREEN): tests ゼロで append したラウンドを close で N に更新すると per-round と cumulative の closure-rate が反映されることを隔離ストアで assert する。冪等性(同一 close の再適用の扱い)・未知 round-id の fail-soft・既存の append と metrics の非回帰も別ケースで固定する
- 駆動 SKILL に closure ステップを明記する: continuous-audit の SKILL に、CONFIRMED findings を回帰テストで締めた後にそのラウンドへ closure を記録する手順(発見から修正、そして closure までの動線)を追記する
- 実測バックフィル: 既存 ledger の実ラウンドを実際の fix 側で締める。round 1(confirmed 5、本 gate 群を締めた回帰テスト)と round 2026W28(confirmed 11、本スレッドで blastguard・specguard・stuckguard・propguard・mutategate を締めた回帰テスト)を close し、audit-metrics が正のゼロ超 closure-rate と実態に即した converging を表示することを観測する
- overwatch を micro で lockstep bump する(3つの version 正典を同時に)。version lockstep チェッカと bump-on-change チェッカが green、cargo fmt と cargo clippy(overwatch, all-targets)が warnings 拒否で clean、cargo test の overwatch が green。fail-soft と never-break-a-turn と後方互換を維持する
- 検証(build は validate ではない): 別モデルで独立に closure 更新(round-id 一致・冪等・未知 round-id の fail-soft)と audit-metrics の closure と converging が実測に一致することを再確認し、観測した closure の数値を証拠として compass outcome に記録する

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(収束シグナルが fix 側を反映し closure と converging が実測に一致) から 現状 の最大差分: audit_round は finding 時に regression_tests_added を記録し(必然的に 0)、ledger は append-only なので、17 以上の回帰テストが確認済み findings を締めた後も closure はゼロ・converging は false のまま=ループが『硬化しているか』に答えられない構造的盲点。最小 validate スライス(size は s から m の純加算) = overwatch の audit-round に round-id 越しの closure 更新サブコマンド(store の read-modify-write と決定論テスト)を足し、駆動 SKILL に closure ステップを明記し、既存の 2 ラウンド(round1 は confirmed 5、round 2026W28 は confirmed 11)を実 fix 側でバックフィルして audit-metrics が honest な正の closure-rate と converging を出すことを観測、overwatch を micro で lockstep bump する。

## next_action
overwatch の audit-round に close サブコマンド(--round と --tests を取り、該当ラウンドの regression_tests_added を read-modify-write で更新する純関数)を実装し、決定論テスト(RED から GREEN)を追加し、continuous-audit の SKILL に closure ステップを明記し、round1 と round 2026W28 をバックフィルして audit-metrics が honest な正の closure-rate と正しい converging を表示することを観測し、overwatch を 3 正典 lockstep で micro bump し、gates を green にして、compass outcome に観測値を記録する。

## parked

