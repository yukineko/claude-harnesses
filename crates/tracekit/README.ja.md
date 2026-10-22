tracekit は、condukt の 1 ラン分のフェーズを、親リンク付きの **span ツリー**として記録する CLI である。各 span は `phase`（フェーズ）・`model`（モデル）・`ms`（所要ミリ秒）・`cost`（コスト）・`status`（状態）を保持し、`tracekit record` 1 回ごとに 1 件ずつ追記される。

tracekit はフックを持たない。`/condukt` スキルのシェル手順が `tracekit record` を呼ぶ。

失敗フェーズは `✗` で印が付き、ロールアップのエラー数にも算入される。

span は `~/.tracekit/<RID>/spans.jsonl` に追記される。並列に record しても既存の行は先頭に残り、各 span id はファイル中にちょうど 1 回現れる。

実測 2026-10-15: 同一ランへ 16 並列で `tracekit record` を走らせると、2 つの span が 1 行に連結されることがあり（15 回中 4 回）、ローダはその行を malformed として読み飛ばす。

サブコマンドは `record` / `trace` / `export` / `list` の 4 つ。

```sh
tracekit record --run RID-42 --span t1 --name "interpret goal" \
  --phase interpreter --model sonnet --ms 1840 --cost 0.012 --status ok
tracekit record --run RID-42 --span t2 --parent t1 --name "impl auth" \
  --phase worker --model opus --ms 30200 --cost 0.41 --status verified
```

`--parent` で親 span を指定してツリーを構成する。`--task` でタスク id を付けられる（`harness.task_id` として export される）。`--ms` の既定は 0、`--status` の既定は `ok`。`--end-unix-ms` を渡すと記録時の終了タイムスタンプを上書きできる。

```sh
tracekit trace RID-42
```

```
trace RID-42
· interpret goal [interpreter/sonnet] 1840ms $0.0120 ok
  · impl auth [worker/opus] 30200ms $0.4100 verified

  2 spans · wall 31480ms · $0.4220 · slowest impl auth (30200ms) · 0 error(s)
```

```sh
tracekit export RID-42                 # → ~/.tracekit/RID-42/otlp-RID-42.json
tracekit export RID-42 --out -         # → 標準出力へ
tracekit export RID-42 --service condukt
```

出力は OTLP/JSON（`resourceSpans → scopeSpans → spans`）で、GenAI のエージェント span 属性を持つ。`gen_ai.operation.name`（エージェントフェーズは `invoke_agent`、ツールフェーズは `execute_tool`）、`gen_ai.request.model`、`gen_ai.usage.cost_usd` に加え、`harness.phase` / `harness.task_id` / `harness.status` が付く。親リンクは `parentSpanId` として保たれる。

```sh
tracekit list      # span を記録済みのラン一覧
```

`/condukt` スキル（`crates/condukt/skills/condukt/SKILL.md`）は `command -v tracekit` の確認と、`interpret` のルート span・親が `interpret` の worker span・親が worker span の verifier span を記録する `tracekit record` 呼び出しを含む。`crates/condukt/src` 配下に `tracekit` や `spans.jsonl` を含むファイルは無い。
