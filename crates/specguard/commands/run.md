---
description: 仕様↔実装 整合監査を subscription-native に実行する。specguard ハーネスで shard を描画し、各 shard を read-only subagent で監査し、結果をハーネスに戻してレポート/sentinel を更新する。
argument-hint: "[--baseline <ref>]"
allowed-tools: Bash, Task, Write, Read
---

あなたは specguard の **オーケストレータ** です。決定的なハーネス処理は `specguard`
バイナリ (PATH 上) に委譲し、LLM 判定は **このセッション内の read-only subagent**
(`specguard-auditor`) に委譲します。`claude --print` のサブプロセスは起動しません
（ホストセッションの subscription でそのまま課金されます）。

追加引数: `$ARGUMENTS` (例: `--baseline HEAD~10`)。空なら付けない。

以下の手順を **順番に** 実行してください。

## 1. shard プロンプトを取得する (ハーネス: scope + 描画)

`specguard prompt --json $ARGUMENTS` を Bash で実行する。

- **exit 5** (未批准): prompt (メタ正典) が未批准/変更ありで監査が拒否された。
  stderr の指示どおり、内容を確認のうえ `/specguard:accept-prompt` を案内して **停止**。
- **exit 2** など非ゼロ: stderr を提示して停止 (config 不在なら `specguard init` を案内)。
- **成功**: stdout は JSON エンベロープ
  `{project, baseline, head, date, marker, shards: [{label, prompt}]}`。これを parse する。

`shards` が **空配列** の場合は監査対象なし。手順 2 を飛ばして、空の outputs
(`{"shards": []}`) で手順 3 に進む (ハーネスが「監査対象なし」レポートを記録し、
baseline を適切に前進/据え置きする)。

## 2. 各 shard を read-only subagent で監査する (判定: subscription)

`shards` の **各要素について** `Task` ツールで `specguard-auditor` subagent を
起動する。**並列で同時に起動してよい** (各 shard は独立・fresh context が設計意図)。

- subagent への入力プロンプト = その shard の `prompt` フィールドを **一字一句そのまま**。
  要約・改変・抜粋をしない (フォーマットとマーカーの正典はプロンプト側)。
- subagent の **最終メッセージ全文** を、その shard の `stdout` として保持する
  (`label` と対応づけて控える)。

subagent は read-only (Read/Grep/Glob/読み取り専用 git のみ; Edit/Write/network は剥奪
済み)。監査だけさせ、修正は絶対にさせない (Human-on-the-loop)。

## 3. 結果をハーネスに戻す (ハーネス: parse → verify → report → sentinel/baseline)

集めた各 shard の出力を次の JSON にまとめ、`Write` で一時ファイル
(`.specguard-ingest.json`) に書き出す:

```json
{ "shards": [ { "label": "<shard label>", "stdout": "<subagent の最終メッセージ全文>", "code": 0 } ] }
```

- `label` は手順 1 の JSON の各 shard の `label` を **そのまま** 使う (ハーネスが
  label で突き合わせる)。返し損ねた shard はエージェント失敗 (exit 4) になるので、
  **全 shard を必ず含める**。
- 監査対象なしのときは `{ "shards": [] }`。

次に `specguard ingest --from .specguard-ingest.json $ARGUMENTS` を Bash で実行する。
終了後、一時ファイルは `rm -f .specguard-ingest.json` で削除する。

ingest の終了コードで結果を解釈する:

- **exit 0**: 監査完了。stdout に「修正候補あり/なし」「report のパス」「baseline 前進/
  据え置き」が出る。それを **そのままユーザーに要約報告** する。`needs_user=yes` の
  指摘があれば、report ファイルを `Read` で開いて findings の要点を伝え、対応後は
  `/specguard:ack` で sentinel を解除する旨を案内する。
- **exit 3** (no-marker): いずれかの subagent がマーカー無しの出力を返した。レポートは
  保存されるが findings は確定できない。どの shard か stderr を見て、その shard を
  手順 2 から **やり直す** か、ユーザーに報告する。
- **exit 4** (agent-failed): 返し損ねた shard / 失敗 shard がある。stderr が該当 label を
  挙げるので、その shard を手順 2 で再実行して手順 3 をやり直す。

## 注意

- このコマンドの read-only 保証は subagent の **ツール名レベル** (Edit/Write/network 剥奪
  + 読み取り専用 git のプロンプト規律) による。バイナリ単体の `specguard run` は
  `claude --print` の **Bash 引数 allowlist** でより強く強制するので、prompt-injection
  耐性を最優先したい監査対象では standalone の `specguard run` も選択肢になる。
- レポート/sentinel/baseline の意味づけ・批准ゲートはすべてバイナリ側が単一の正典として
  決める。このコマンドは「描画→ subagent → ingest」の配管に徹し、判定ロジックを複製しない。
