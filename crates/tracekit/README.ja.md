# tracekit

condukt の 1 ラン（実行）を span ツリーとして記録・可視化し、OpenTelemetry GenAI 形式で出力するトレーサー。

## 目的

tracekit は、condukt の 1 ラン分のフェーズ（interpreter → worker → verifier）を、親リンク付きの **span ツリー**として記録するスタンドアロンの CLI である。各 span は `phase`（フェーズ）・`model`（モデル）・`ms`（所要ミリ秒）・`cost`（コスト）・`status`（状態）を保持し、フェーズ完了のたびに 1 件ずつ追記される。

責務は次の 3 つに絞られている。

- **record** — フェーズが終わるたびに 1 span を追記する。
- **trace** — ラン全体の span ツリーをレンダリングし、span 数・合計時間（wall）・合計コスト・最遅 span・エラー数のロールアップを出す。
- **export** — span ツリーを OpenTelemetry GenAI セマンティック規約に沿った OTLP/JSON として書き出す。

ライフサイクルフックではなく素の CLI であり、呼び出し側（condukt の `state set` 遷移、または人間）が `tracekit record` を直接呼ぶ。サブスクリプションネイティブな設計で、バンドルされた単一の Rust バイナリのみで動く。**ファイルのみ・ネットワークなし・API キー不要**である。

## どうして必要か

gauge はエージェントのコストを **kind**（main か sub-agent か）という*バケット*で答えるが、run / task / span の連結を持たない。そのため condukt のランが遅い・失敗したときに、*どのフェーズ*が原因か——interpreter なのか、あるワーカーなのか、verifier なのか——を指し示すものが何もない。

tracekit は、gauge のコストバケットと本物の OTel トレースの間にある、この欠けた因果ビューを埋める。1 ランの各フェーズを親リンク付きの span ツリーとして記録するので、失敗したランの「遅い／高い／壊れた」フェーズが一目で分かる。失敗フェーズは `✗` で印が付き、ロールアップのエラー数にも算入される。

span は `~/.tracekit/<RID>/spans.jsonl` に**追記専用**で書かれるため、並列ワーカーの完了が互いを上書きすることがない。

## どう使うか

サブコマンドは `record` / `trace` / `export` / `list` の 4 つ。

### span を記録する

フェーズが終わるたびに 1 span を追記する。

```sh
tracekit record --run RID-42 --span t1 --name "interpret goal" \
  --phase interpreter --model sonnet --ms 1840 --cost 0.012 --status ok
tracekit record --run RID-42 --span t2 --parent t1 --name "impl auth" \
  --phase worker --model opus --ms 30200 --cost 0.41 --status verified
```

`--parent` で親 span を指定してツリーを構成する。`--task` でタスク id を付けられる（`harness.task_id` として export される）。`--ms` の既定は 0、`--status` の既定は `ok`。`--end-unix-ms` を渡すと記録時の終了タイムスタンプを上書きでき、決定論的なリプレイに使える。

### ツリーをレンダリングする

```sh
tracekit trace RID-42
```

```
trace RID-42
· interpret goal [interpreter/sonnet] 1840ms $0.0120 ok
  · impl auth [worker/opus] 30200ms $0.4100 verified

  2 spans · wall 31480ms · $0.4220 · slowest impl auth (30200ms) · 0 error(s)
```

### OTel GenAI span をエクスポートする

```sh
tracekit export RID-42                 # → ~/.tracekit/RID-42/otlp-RID-42.json
tracekit export RID-42 --out -         # → 標準出力へ
tracekit export RID-42 --service condukt
```

出力は OTLP/JSON（`resourceSpans → scopeSpans → spans`）で、GenAI のエージェント span 属性を持つ。`gen_ai.operation.name`（エージェントフェーズは `invoke_agent`、ツールフェーズは `execute_tool`）、`gen_ai.request.model`、`gen_ai.usage.cost_usd` に加え、`harness.phase` / `harness.task_id` / `harness.status` が付く。親リンクは `parentSpanId` として保たれる。

### ラン一覧

```sh
tracekit list      # span を記録済みのラン一覧
```

### condukt との連携について

tracekit 自身はフックを持たない。`/condukt` スキル（`crates/condukt/skills/condukt/SKILL.md`）がシェル手順から soft 依存として `tracekit record` を呼ぶ — まず `interpret` のルート span、続いてタスクごとに worker span と verifier span を記録する。condukt バイナリ自体は span を発行せず、gauge のエージェント単位コストを対応する span に結合する処理も自動化されていない。

なお、バックエンドへのライブ OTLP/HTTP プッシュは予定されている後続作業である。エクスポート済みファイルは既に OTLP の `TracesData` 形状に一致しているため、後からリプレイできる。
