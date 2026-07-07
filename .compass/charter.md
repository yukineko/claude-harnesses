## north_star
harness の LLM 判断面のうち実は機械判定可能な残渣を決定論バイナリへ落とす determinism-sweep arc の最終スライス。監査7候補のうち #1 baseline set-diff / #2 構造化checks / #3 confidence-from-facts / #5 C3語彙screen / #6 outcome既定 / #7 class overlap は landed 済み。残る唯一の候補 #4 = scout の施策スコアリングを LLM 手計算から決定論スコアラーへ落とし、arc を締める。今の ONE = scout Phase-3 の優先スコア `(severity × goal近接) ÷ effort`(L2/L5 重み付き)を LLM の目分量から harness-core の純関数へ移し、scout skill はその決定論スコアラーを呼んで順位付けする。LLM は evidence 判定・重複排除・意味判断に専念し、順位付け(算術)は binary。scout は skill-only(bin 無し)なのでスコアラーは harness-core に置き、scout が叩ける既存 bin 経由で露出する(host bin は condukt 分解で確定)。純加算・後方互換(既存 Phase-3 出力形は不変)・subscription-native を崩さない。安全 invariant(意味的正しさ・分解・novel design は LLM に残す / never-break-a-turn)は不変。これで determinism-sweep arc(7/7)を閉じる。

## definition_of_done
- harness-core に、施策候補(severity[high|medium|low]・effort[xs..xl]・goal近接シグナル)から優先スコアを返す決定論純関数がある。同一入力→同一スコアで、スコア = severity重み × goal近接 ÷ effort係数、かつ L2(security)/L5(safety) レンズは重みを上げる、を固定ルールで算出する。順序と重み付けの真偽表を unit test で実証する(cargo test green)
- scout SKILL.md の Phase-3『スコアリング』手順が、LLM の手計算でなくこの決定論スコアラー(harness-core 関数を露出する PATH 上の bin 経由)を呼んで候補を順位付けするよう書き換わる。LLM は evidence フィルタ・重複排除・優先度タグ付けの意味判断のみ担い、算術順位付けは binary に委譲する。既存 Phase-3 の出力形(backlog add へ渡す施策リスト)は温存する(純加算・後方互換)
- 純加算で既存挙動を温存: fmt と clippy(-D warnings) clean、対象 crate の cargo test green、触った plugin を正典ファイルで lockstep micro bump(scout は skill-only なので plugin.json + marketplace.json の2ファイル、host bin を持つ plugin があれば Cargo.toml も含め3ファイル。harness-core は build-time lib なので bump 不要)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap
ゴール(scout の施策スコアリングが決定論スコアラー経由で LLM 目分量を排除)と現状の最大差分 = scout SKILL Phase-3 が優先スコア (severity × goal近接) ÷ effort を LLM の手計算で付けている(scout/SKILL.md Phase 3『スコアリング — 自分(main の LLM)で』) → 同一候補でも run ごとにブレる・L2/L5 重み付けが暗黙。差分を埋める = harness-core に severity/effort/goal近接→スコアの固定ルール純関数を1本足し(L2/L5 重み固定・真偽表 unit test)、scout skill はそれを露出する bin を呼んで順位付けする(LLM は evidence/重複排除/優先度タグの意味判断のみ)。純加算・後方互換・Phase-3 出力形不変。size s。determinism-sweep arc の最終(7/7)スライス。

## next_action

## parked

