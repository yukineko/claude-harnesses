## north_star
scripts/rollout-plugins.sh の GATE_CRATES に overwatch が追加済み(commit 558f864)なのに、pre-push フックの GATE crate 判定パターン・CLAUDE.md の2箇所(36,62行目)・scripts/tests/canary-gate-crates.sh の case4 が追従しておらず、canary-gate-crates.sh が実際に FAIL している状態を解消し、GATE_CRATES 定義を全箇所で一致させる。

## definition_of_done
- pre-push フック(.githooks配下)の GATE crate 判定パターンに overwatch が含まれるよう修正する
- CLAUDE.md の GATE_CRATES 記述が overwatch を含む
- scripts/tests/canary-gate-crates.sh が全caseで exit 0 になる(case4修正込み)
- crates/overwatch/tests/rollout_gate_crates.rs を含め cargo test -p overwatch が green
- 修正はコミットのみ(push・PR作成はしない)

## measuring_stick
擁護可能性 × ゴールへの接近距離 ÷ コスト

## current_gap

## next_action

## parked

