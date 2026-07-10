# Continuous-Audit レビュー機能 — 価値・適用範囲・"無駄にしない"規律

このノートは「AI 敵対レビュー（`/continuous-audit`：finder→verifier→CONFIRMED→回帰テスト→修正）を
**なぜ持つのか / どこに効くのか / どうすれば形骸化しないのか**」の運用上の見解を固定するもの。
`docs/review-redesign-implementation-items.md`「継続運用の原則」の実運用側の補足であり、
1 度の実データ（2026-07-11 のラウンド1）で検証した結論を残す。

## 実証された価値（2026-07-11 round 1）

GATE crates（blastguard/propguard/specguard/stuckguard/mutategate）に 1 ラウンド回した結果、
**shipping 済みの高リスク fail-open を 5 件**発見・修正した。いずれも「防御ゲート自身が危険を安全と
誤判定する」種類で、既存テストと人間レビューをすり抜けて main に載っていた：

- blastguard: `rm -rf *.toml` が Allow（config glob への self-match）／後段セグメントの truncating
  redirect を見逃す（最初の `>` しか検査しない）。
- propguard: FAIL 判定した diff を再検査せず通す（already-verified バイパス）／別 property の PASS
  説明文で FAIL property を satisfied 誤カウント。
- specguard: 主語スワップによる認可反転が Precedented 自動批准（polarity gate が前方のみ束縛）。

**この機能が「diff だけ・人力では追えない non-local な正しさ」を実際に捕まえた**というのが、
持つべき理由の一次証拠。加えて：

- **篩いが効いた**：finder 6 候補のうち stuckguard の 1 件を verifier が正しく REFUTED（ノイズを確信に
  混ぜない）。
- **独立系統が裏付けた**：overwatch の fleet-systemic 検知が blastguard の 2 件を別経路（3 回／11 回
  再発）で corroborate。単一手法の思い込みではない。

## 無駄に転ぶ条件（ここを外すと形骸化する）

1. **記録だけして直さない** → review theater。CONFIRMED を放置すると review-queue は単なる nag に
   なる。**必ず修正まで回して初めて元が取れる**（今回は condukt で 5 件すべて RED→GREEN 化した）。
2. **無差別適用** → コスト（finder＋verifier＋worker のマルチエージェント）が価値に見合わない。
   価値は gate/セキュリティクリティカルな面に集中する。日常の低リスクコードに全面適用しない。
3. **収束を測らない** → ラウンドを重ねて new-findings が減っているか（`overwatch audit-metrics`）を
   見ないと、「決定論コアの構造的欠陥」シグナル（減らない crate）を見落とす。

## 適用範囲と規律（これを守る限り儀式ではなく実利）

- **対象**：GATE crates（`scripts/rollout-plugins.sh` の GATE_CRATES と同期）に限定。
- **頻度**：gate 関連ファイルの変更時＋定期（`.githooks/pre-push` の advisory、cron 雛形は
  `scripts/continuous-audit.cron.example`）。
- **モデル多様性（MUST）**：finder と verifier は必ず別モデル（生成と検証の盲点共有を防ぐ）。
- **決定性への固定化（MUST）**：CONFIRMED は**必ず**その場で回帰テスト化して閉じる。これにより
  次ラウンドの探索は「まだテスト化されていない未知」に絞られ、new-findings は単調非増加に向かう。
- **収束の監視**：`overwatch audit-metrics` の new-findings 時系列を追う。減らない crate は決定論
  コアの設計を疑う優先対象。

## この機能を継続可能にするための必要改修（実使用で判明した gap）

ラウンド1を実際に回して見えた、**放置すると動線が途切れる**具体的な改修。優先度順：

1. **review-queue → backlog 自動ブリッジ（最優先）**：現在 CONFIRMED は `overwatch review-queue` に
   だけ載り、fix queue（`backlog` クレート）へは**手動でしか流れない**。`/flow` は backlog を drain
   するので、ここが自動化されて初めて「発見→修正」が人手を挟まず動線として閉じる。
2. **`scripts/continuous-audit.sh` の空 `FINDINGS[@]` unbound バグ**：`--finding` を渡さない dry-run
   が bash 3.2（macOS 既定）で `unbound variable` で落ちる（`set -u` + 空配列展開）。tool 自身の
   移植性バグ。→ `[[ backlog: harness 同梱シェル資産の bash4 依存横断 ]]` と同根。
3. **`audit-round --round` の契約不一致**：SKILL の例は `2026W28`（文字列）だが overwatch バイナリは
   **数値**を要求し弾く。SKILL 例を数値に直すか、バイナリを文字列許容にするか、どちらかへ統一。

これらが埋まるまでは「手動ブリッジ＋数値 round-id」で回避運用する（今回はそうした）。
