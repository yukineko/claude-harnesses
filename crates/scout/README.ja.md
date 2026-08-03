# scout

プロジェクトを多角的に監査して「施策（タスク）」を生成する SOURCE。

## 目的

scout は、プロジェクトを 5 つのレンズで偵察し、実行可能な施策（タスク）を洗い出して backlog に積み、`/flow` へ実行を引き渡す SOURCE である。`/scout` スキルが決定論的なプロジェクト状態を集め、5 レンズ（現在の課題・セキュリティ・業界/他プロジェクトの標準・不足施策・安全性）に read-only な sub-agent を並列展開し、返ってきた施策候補を統合・重複排除・スコアリングしてから、承認された施策だけを backlog に書き出す。

役割分担は明確だ。監査と施策の選別という*判断*は LLM（このスキル＋ sub-agent）が、施策の*保存*は backlog バイナリが、*実行*は flow/condukt が担う。scout 自身は新しい状態を持たず、ファイルも一切編集しない。read-only で課題を*発見*するだけである。

compass が単一ゴールへ向かう勾配（一手に絞り残りは保留）であるのに対し、scout は広域偵察で複数の独立した施策を一度に挙げる相補的な SOURCE だ。両者とも source であり、最終的に backlog → /flow → condukt という同じ executor に合流する。

scout はサブスクリプション完結（**skill のみ、バイナリなし、API キー不要**）である。状態を持たず、書き込みは backlog への追加だけに限られる。

## どうして必要か

「次に何の施策を打つべきか」を広く洗い出す作業を LLM 単体に任せると、見落としや幻覚が混じり、横断的な健全性が静かに劣化していく。scout はこの偵察を構造化された不変条件の上で回し、次の失敗モードを防ぐ。

- **抜け漏れのある場当たり監査。** 「何が足りない？」を一回の問いで済ませると、セキュリティ・安全性・CI・テスト・docs といった横断的な観点が網羅されない。scout は固定の 5 レンズに sub-agent を並列展開し、観点の取りこぼしを防ぐ。
- **証拠のない施策（幻覚）。** 根拠のない「やった方がよさそうなこと」を施策として積むと、backlog がノイズで埋まる。scout はすべての施策に逐語引用・`file:line` 参照・Web ソース URL のいずれかを必須とし、証拠の無い候補は採用しない。
- **仕様/テストを見ない誤判定。** コードの引用は真でも、それが仕様上意図した挙動やテストで既にカバー済みの挙動であれば「課題」ではない。scout は統合フェーズで各候補を `specguard` の spec-map（仕様↔実装↔テストの索引）に照合し、既に意図的/カバー済みと分かる候補を落とす（specguard 未導入なら fail-soft で従来どおり）。
- **無秩序な書き込み。** 監査ツールが勝手にファイルを編集したり全件を積んだりすると制御が効かない。scout の書き込みは `backlog add` のみで、施策の確定には `AskUserQuestion`（HOTL）による明示的な合意を必ず挟む。
- **コストの暴走。** 5 レンズ全開（特に Web を使う L3）は狭いスコープには過剰だ。scout はスコープ規模（追跡ファイル数）に応じてレンズ数を決定論的に縮約し、縮約理由をサマリに明示する（静かな打ち切りはしない）。

compass では拾えない複数の独立した施策が欲しいとき、あるいはセキュリティ・安全性・CI・テスト・docs などの横断的な健全性を点検したいときに scout を使う。

## どう使うか

プラグインマーケットプレイス経由でインストールすれば、`/scout` スキルはすぐに使える。バイナリも配線するフックも無い。施策の永続化には PATH 上の `backlog` バイナリを利用する。`backlog` が無い場合は施策を Markdown で提示し、手動投入を案内する（書き込みはスキップ）。

```
/scout                      # repo 全体・5 レンズすべてで監査
/scout security のみ          # セキュリティレンズに絞る
/scout crates/condukt        # サブツリーだけを監査
/scout --dry-run             # 施策を提示するが backlog には書き込まない
```

`/scout` は次の固定パイプラインを回す。判断は LLM、調査は read-only sub-agent が担う。

| フェーズ | 内容 |
|---|---|
| スコープ受領＋縮約 | 監査スコープを取り、規模（追跡ファイル数）に応じてレンズ数を決定論的に縮約（小 → L1/L5、中 → L1/L2/L4/L5、大 → 全 5）。コスト制御。 |
| 決定論的レビュー | `git log` / `cargo test` / `compass gap` / `backlog list` / `cargo deny` などで事実を read-only 収集し、全 sub-agent に渡す。 |
| 5 レンズ並列調査 | read-only sub-agent が逐語証拠つきの施策候補（JSON）を返す。 |
| 統合 | 重複排除・証拠フィルタの後、`specguard map list` で候補の touched file を spec-map と照合し、`spec_doc`/`test_files` が既にその挙動を意図的・カバー済みとしていれば候補を落とす（specguard 未導入なら fail-soft で省略）。続けてスコアリングは決定論バイナリ `compass score` に委譲（LLM は候補ごとに `goal_proximity` だけを見積もり、`severity`/`effort`/`lens`/`goal_proximity` を渡してランキングさせる。L2/L5 の重み上げはスコアラー側に内蔵済み）。`compass score` が無い／未対応なら `(severity × goal への近さ) ÷ effort` の手計算にフォールバック。`p0/p1/p2` タグ付与。 |
| 合意（HOTL） | 既定は `AskUserQuestion`（multiSelect）で backlog に積む施策をユーザーが選ぶ。autonomy ゲート: `condukt state autonomy-check` が autonomous を返したら選別 Ask を省き、スコア上位 N 件（既定 top 8、`p0`/`p1` 優先）を自動採用する。 |
| 書き出し＋引き渡し | 承認施策を `backlog add`（`--tag scout`、証拠と完了条件を `--notes` に記録）し、`/flow` へ引き渡す。既定は propose-then-confirm、autonomous なら 1 件以上積んだときに `/flow` を自動起動する。 |

施策を積んだら scout は `/flow` へ引き渡してから退く。既定では `/flow` を**提案**（propose-then-confirm）し、autonomy（`condukt state autonomy-check`）が有効なら `/flow` を自動起動する。実行ループと backlog ロックは flow の責務であり、scout は併走しない。`--dry-run` のときは autonomy でも合意フェーズで停止し、提示だけで終了する。

不変条件は次のとおり。監査は read-only（scout も sub-agent もファイルを編集しない）、証拠の無い施策は採用しない、書き込みは `backlog add` のみ、施策の確定には明示的なユーザー合意を要する（ただし autonomy 有効時は top-N を自動採用し、その内容をサマリで明示する）。
