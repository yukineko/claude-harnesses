## north_star
harness の LLM 判断面のうち、実は機械判定可能（fixed rule / parse / 集合演算 / 数値閾値）な残渣を決定論バイナリに落とし、非決定性を排除する arc。判断=LLM / 決定論=バイナリの分離をさらに前進させ、build≠validate を検証段でより締める。安全 invariant（意味的正しさ・分解・novel design は LLM に残す / never-break-a-turn / 後方互換）は不変。監査で7候補を特定・ランク済み（#1 baseline set-diff, #2 構造化 checks, #3 confidence-from-facts, #4 scout score, #5 C3 語彙screen, #6 outcome 既定, #7 class overlap）。今の ONE = condukt Phase-6 検証の非決定性を排除する2スライス: (1) baseline-failure 除外を LLM 目視でなく current_failing−baseline の集合差分で決定論化(#1), (2) verifier confidence を fact 由来(checks/repro が実行され exit0 かつ回帰ゼロ→high / 未実行・free-text 論拠→low)に(#3)。どちらも condukt verify.rs 内・純関数・明確な F→P。残る候補(#2 #4 #5 #6 #7)は parked。subscription-native を崩さない。

## definition_of_done
- condukt に、パース済みテスト名の集合差分で回帰を出す決定論関数がある。regressions = current_failing − baseline_failing。既存 red がありかつ新規回帰なしなら passed=true、新規回帰ありなら passed=false を返す。この振る舞いを unit test で red→green 実証する
- condukt に、verifier confidence を観測事実から導く決定論関数がある。checks/repro が実際に実行され exit0 かつ回帰ゼロなら high、検証が未実行または free-text 論拠に依存するなら low を返す。真偽表を unit test で実証する
- 純加算で既存 verify 経路と LLM verifier を温存（決定論層を前段・補助として足すのみ、既存の出力・パスは不変）。fmt と clippy(-D warnings) clean、cargo test -p condukt green、condukt の version を3正典ファイル(Cargo.toml・plugin.json・marketplace.json)で lockstep bump

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(condukt Phase-6 検証の非決定性が排除された)と現状の最大差分 = 現状の verifier は (1) baseline-failure 除外を LLM が『実装前から壊れてた失敗リスト』と『今の失敗』を目視で突き合わせている(condukt-verifier.md, SKILL.md:375)→長い transcript 越しの目視で誤帰属(既存 red を回帰と誤認=誤fail / 新規 break を見逃し=誤pass)、(2) confidence(high/med/low)を LLM が自己申告(condukt-verifier.md:45-57)→当てずっぽうが opus-tier 再検証を無駄起動。差分を埋める = verify.rs に純関数を2本足す: (a) regressions=current_failing−baseline のテスト名集合差分で passed を決定論判定、(b) checks/repro が実行され exit0 かつ回帰ゼロ→high / 未実行・free-text 論拠→low の confidence 導出。既存 parse_test_result_failed/distill_failure を再利用。純加算・後方互換(既存 LLM verifier 経路は温存し前段・補助として足す)・condukt version 3正典 lockstep bump。size s×2、両者 verify.rs 内で近接するが独立関数=condukt schedule に委譲。#2 構造化checks/#4 scout score/#5 C3 screen/#6 outcome 既定/#7 class overlap は parked。

## next_action

## parked

