# loop engineering の観点での claude-harnesses 評価(2026-07-13)

「loop engineering」を「エージェントの context→plan→act→verify→repeat サイクルを、プロンプトでは
なく決定論的なハーネスで型にはめる設計論」だと捉えたときに、この repo の39クレートがその実践と
してどう位置づくかを評価したメモ。

## 軸ごとの担当クレート

| 軸 | 担当クレート |
|---|---|
| Context | context-governor, ctxrot, playbook, runbook |
| Plan/source | compass, backlog, scout, hypothesis → flow が統合 |
| Act/executor | condukt(interpreter/worker/verifier), fugu-router |
| Verify/Stop gate | donegate, propguard, reviewgate, tdd, precommit-audit, mutategate, specguard, schemaguard, trajectoryeval |
| Stall検知 | stuckguard |
| Safety rail | blastguard, budgetguard |
| Observability | tracekit, gauge, session-insights, overwatch, difflog |
| HOTL | taskprog, harness-status, beacon, autoflow |

この repo は「loop engineering」の主要な軸をほぼ網羅している。

## 取り入れる価値がある3パターン(このrepo固有実装を超えて汎用的)

### 1. propguard — fail-closed な意味的不変条件ゲート

done_criteria から3〜5個の property(error-path / determinism / idempotence 等)を導出し、
`(diff, properties)` をハッシュして未検証diffは必ず1回ブロックする設計([crates/propguard/README.ja.md](../crates/propguard/README.ja.md))。
「テストが緑」と「意味的に正しい」のギャップを埋める補完ゲートで、tdd だけでは拾えない
「テストは通るが不変条件は壊れている」を機械的に潰す。壊れたチェッカーはバイパスにならない
(クラッシュ時はブロックが安全側)設計が丁寧。

### 2. stuckguard — 決定論的スタック検出

Jaccard近似一致による反復検出 + oscillation(編集の往復)検出 + progress-score(多様性・状態
ハッシュ安定性・エラー再発の3信号)という多層検知([crates/stuckguard/README.ja.md](../crates/stuckguard/README.ja.md))。
ほとんどの agent harness は max-turns か人間の目視に頼っており、「モデル自身は堂々巡りに
気づかない」という失敗モードをブロックせず助言だけに留める(誤検知コストを1行に抑える)設計は
他プロジェクトにそのまま輸出できる。

### 3. condukt — F→Pオラクル(再現性オラクル)

「fix/feature が verified 昇格するには、実際に Fail→Pass の遷移を再現できたことを要求する」
仕組み(`condukt state check-oracle`)。エージェントが「直した」と自己申告するだけで完了扱いに
なる典型的な失敗を構造的に防ぐ。

## 気になったギャップ

verify系(propguard/reviewgate/specguard等)は基本的に**単一パスの自己検証 or 単一subprocess
チェッカー**で、独立した複数の懐疑者が投票する「adversarial verify」パターン(N人の独立
skeptic に refute させて多数決を取る、workflow orchestration で使われる手法)は見当たらない。
propguard の subprocess モードも1チェッカーのみ。重要な完了判定(GATEクレート自体の変更など)
に限り、複数の独立チェッカーによる多数決を挟む価値はあるかもしれない。
