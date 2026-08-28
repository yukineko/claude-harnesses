---
description: 仕様マップ store (`specguard map`) を情報源に、実装と仕様の "正しさ" (correctness) とテストの網羅性 (coverage) を read-only で監査する — 仕様↔実装の "整合" (consistency) を見る `/specguard:run`/`/specguard:drift-map` とは対照的。仕様の健全性・実装の正しさ・テストの妥当性/網羅性を read-only subagent で判定し、結果を report/sentinel に反映する (Human-on-the-loop)。自身は仕様/実装/テストを一切書き換えない — coverage 不足の是正 (テスト追加) は backlog/condukt (executor) に委譲する。
argument-hint: "[target] [--baseline <ref>]"
allowed-tools: Bash, Task, Read, Write
---

あなたは specguard の **正しさ監査オーケストレータ** です。決定的なハーネス処理
(マップの読み込み・shard 描画・parse・report/sentinel 更新) は `specguard` バイナリ
(PATH 上) に委譲し、LLM 判定は **このセッション内の read-only subagent**
(`specguard-auditor`) に委譲します。`claude --print` のサブプロセスは起動しません
（ホストセッションの subscription でそのまま課金されます）。

## consistency (整合) と correctness (正しさ) の違い — 混同しないこと

- **`/specguard:run` / `/specguard:drift-map` (spec-drift)**: 仕様書と実装が
  **一致しているか** (consistency) を見る。仕様と実装がズレていないかの突き合わせで、
  scope は **git diff** (baseline からの変更ファイル) です。
- **`/specguard:spec-audit` (このコマンド)**: 実装が実際に **正しいか**、仕様書
  そのものが **健全か** (correctness) を見る。仕様と実装が「互いに一致している」
  だけでは不十分 — 両方が間違って一致している (誤りが仕様として固定されている) 場合も
  検出対象です。scope は git diff ではなく **仕様マップ store**
  (`.specguard/spec-map.toml`、feature/endpoint 単位) です。
- このコマンドは **read-only** です。指摘 (findings) を report/sentinel に記録して
  Human-on-the-loop に委ねるだけで、仕様書もコードもテストも一切書き換えません。
- coverage 不足のように「テストを追加すべき」指摘が出た場合も、spec-audit は自分では
  書かず、是正を **executor (backlog/condukt) にハンドオフ** します (手順 6)。これは
  ハーネスの **source → executor 分離** に沿っています — spec-audit は findings の
  read-only な **source** であり、実際のテスト追加は condukt / backlog+flow が行います。
- 見つかった仕様/実装側の drift を実際に是正するのは `/specguard:drift-map` の仕事です。

追加引数: `$ARGUMENTS` (例: `crates/specguard`、`drift-map`、`/health`、`e2e`、
`--baseline HEAD~10`)。ターゲットが空なら map 全体を対象にします。

以下の手順を **順番に** 実行してください。

## 1. ターゲットを解決する ($ARGUMENTS → --filter)

`$ARGUMENTS` からコマンドの対象 (target) と `--baseline` オプションを分離します。
target 解決はコスト有界 (無際限な repo 探索をしない) にします:

- **target が空** → filter なし (map 全体を対象)。
- **target がそのまま検索キーとして使える** (コマンド名・crate パス・API route・
  部分文字列など) → そのまま `--filter <target>` として次の手順に渡す。
  - 例: コマンドを対象にする → `specguard audit --json --filter drift-map`
  - 例: crate を対象にする → `specguard audit --json --filter crates/specguard`
  - 例: API route を対象にする → `specguard audit --json --filter /health`
  - 例: **e2e テストを対象にする** → `specguard audit --json --filter e2e`
    (`test_files` の `tests/e2e/...` 系パスにマッチする。マッチした entry の
    test-adequacy 判定で「その e2e テストが本当に正しさを検証しているか」を
    subagent が判断する)
- **target が自然言語** (例: 「drift-map コマンドの正しさを見て」「e2e テストを
  audit して」) → そこから語彙的なキー (lexical key) を抽出する (例: `drift-map`、
  `e2e`) し、それを `--filter` として使う。
- **抽出した lexical filter が空/曖昧** な場合に限り、まず `specguard audit --json`
  (filter なし) で全 entry の `label` 一覧を取得し、target の意図に最も合致する
  entry を LLM で選び、そのキーで `--filter` を再指定して絞り込む。
  **これ以上の開放的な repo 探索はしない** (map に無い対象は手順 2 でマップを
  ビルドしてから絞り込む)。

## 2. マップの鮮度を確保する (ハーネス: rebuild-if-needed — audit の前に必ず)

古い/不完全なマップを監査しないよう、audit 実行の **前に** マップを最新化します:

1. まず `specguard map sync $BASELINE_ARGS` を実行する (安価な増分反映)。
2. 手順 1 で決めた filter で `specguard audit --json --filter <resolved>` を
   一度試し、**マッチする shard が 0 件**、または対象が明らかに未マッピング
   (新規コマンド/API など) の場合は、`specguard map build $BASELINE_ARGS` で
   マップを (再) 構築してから同じ filter で再試行する。
3. `map build`/`map sync` が書き込むのは **マップ store
   (`.specguard/spec-map.toml`) だけ** です。これは仕様書やコードの修正ではなく
   マップの保守 (skeleton の追跡) であり、spec-audit は仕様/コードに対しては
   read-only のままです。
4. マップが既に鮮度十分と分かっている場合は、この手順の rebuild を省略して
   現在のマップのまま audit してよい (`--no-rebuild` 相当の意図: 手順 1 の
   sync だけ行い、build はスキップする)。

## 3. audit shard を取得する (ハーネス: scope + 描画)

`specguard audit --json --filter <resolved> $BASELINE_ARGS` を Bash で実行する。
`--filter` が空 (map 全体対象) の場合は省略してよい。

- 成功時、stdout は `prompt --json` と **同じ形の** JSON エンベロープ
  `{project, baseline, head, date, marker, shards: [{label, prompt}], total_auditable, truncated}`
  (`marker` は `<<<SPEC_AUDIT>>>`)。これを parse する。
- `shards` が **空配列** の場合は監査対象なし。手順 4 を飛ばし、空の outputs
  (`{"shards": []}`) で手順 5 に進む (ハーネスが「監査対象なし」を記録する)。
- **`truncated: true`** の場合 (`total_auditable` が `shards.length` より大きい、
  すなわち `MAX_AUDIT_SHARDS` 上限で切り詰められた) は、**この回の監査は map 全体を
  カバーしていない** ことを手順 5 のユーザー報告に明示すること (「今回は
  `shards.length`/`total_auditable` 件のみ監査、残りは次回以降に持ち越し」)。
  黙って「監査完了」とだけ報告しない — CA-specguard-07: 以前は eprintln で stderr に
  しか出ておらず、この harness は stderr を読まないため truncation が常に見落とされた。

## 4. 各 shard を read-only subagent で監査する (判定: subscription)

`shards` の **各要素について** `Task` ツールで `specguard-auditor` subagent を
起動する。**並列で同時に起動してよい** (各 shard は独立・fresh context が設計意図)。

- subagent への入力プロンプト = その shard の `prompt` フィールドを **一字一句
  そのまま**。要約・改変・抜粋をしない (フォーマットとマーカーの正典はプロンプト側)。
  このプロンプトは「仕様の健全性 (spec soundness)」「実装の正しさ
  (implementation correctness)」「テストの妥当性/網羅性 (test adequacy / coverage)」
  の 3 次元で判定するよう subagent に指示します。
- subagent の **最終メッセージ全文** を、その shard の `stdout` として保持する
  (`label` と対応づけて控える)。

subagent は read-only (Read/Grep/Glob/読み取り専用 git のみ; Edit/Write/network は
剥奪済み)。監査だけさせ、修正は絶対にさせない (Human-on-the-loop)。

### coverage (網羅性) を監査の一部として明示的に見る

test adequacy の次元は「テストファイルが存在するか」だけでなく、**テストが仕様の
振る舞い/エッジケースを実際に網羅しているか** まで含みます。次の順で安価な信号から
使います:

1. **決定的な構造シグナル (最優先・LLM 不要)**: t1 の構造的 finding **`Untested`**
   (`impl_files` はあるが `test_files` が空) が、テストが皆無の entry を既に
   フラグします。手順 3 の `specguard audit --json` の findings / 人間サマリに
   現れるので、それをまず使う。
2. **妥当性シグナル (subagent 判定)**: `test_files` がある entry については、
   auditor subagent が「既存テストが仕様の振る舞い/エッジケースを本当に網羅して
   いるか (単にファイルが存在するだけでないか)」を判定する。
3. **より深いシグナル (任意・ハード必須にしない)**: リポジトリにカバレッジ
   ツールがある場合 (Rust なら `cargo llvm-cov` / `cargo tarpaulin`、または
   プロジェクト独自の coverage コマンド) は Bash でそれを使って未カバー行を
   補足してよい。無ければ 1.+2. の構造 + 妥当性シグナルにフォールバックする。
   **カバレッジツールをハード要件にしない** (無くても監査は成立する)。

## 5. 結果をハーネスに戻す (ハーネス: parse → verify → report → sentinel/baseline)

集めた各 shard の出力を次の JSON にまとめ、`Write` で一時ファイル
(`.specguard-audit-ingest.json`) に書き出す:

```json
{ "shards": [ { "label": "<shard label>", "stdout": "<subagent の最終メッセージ全文>", "code": 0 } ] }
```

- `label` は手順 3 の JSON の各 shard の `label` を **そのまま** 使う (ハーネスが
  label で突き合わせる)。返し損ねた shard はエージェント失敗 (exit 4) になるので、
  **全 shard を必ず含める**。
- 監査対象なしのときは `{ "shards": [] }`。

次に `specguard ingest --from .specguard-audit-ingest.json $BASELINE_ARGS` を Bash
で実行する。終了後、一時ファイルは `rm -f .specguard-audit-ingest.json` で削除する。

ingest の終了コードで結果を解釈する:

- **exit 0**: 監査完了。stdout に「修正候補あり/なし」「report のパス」「baseline
  前進/据え置き」が出る。それを **そのままユーザーに要約報告** する。
  `needs_user=yes` の指摘があれば、report ファイルを `Read` で開いて findings の
  要点を伝え、対応 (仕様/実装のどちらを直すかは `/specguard:drift-map` の仕事) 後は
  `/specguard:ack` で sentinel を解除する旨を案内する。
- **exit 3** (no-marker): いずれかの subagent がマーカー無しの出力を返した。
  レポートは保存されるが findings は確定できない。どの shard か stderr を見て、
  その shard を手順 4 から **やり直す** か、ユーザーに報告する。
- **exit 4** (agent-failed): 返し損ねた shard / 失敗 shard がある。stderr が該当
  label を挙げるので、その shard を手順 4 で再実行して手順 5 をやり直す。

## 6. coverage 是正を executor にハンドオフする (テスト追加は spec-audit がしない)

監査が「テストを追加すべき」指摘 (手順 3 の構造的 `Untested` finding、または手順 4
の coverage 妥当性判定で「網羅が不十分」とされた entry) を出した場合、spec-audit は
**自分ではテストを書きません**。ハーネスの **source → executor 分離** に従い、是正を
executor に委譲します。**コスト有界**: 監査が実際にフラグした entry のぶんだけ
ハンドオフし、フラグされていない entry には何もしない。

- フラグされた entry ごとに (または 1 つのバッチタスクにまとめて) backlog に
  是正タスクを積む:

  ```sh
  backlog add --title "add tests for <entry key>: <網羅すべき振る舞い>" --project "$PWD" --priority p2
  ```

  (`backlog add` は Bash 経由。backlog store に書くだけで、リポジトリの仕様/実装/
  テストには触れないので spec-audit は read-only のままです。)
- あわせて、実際にテストを実装するには `/condukt "add tests for <…>"` (または
  `/flow`) を回すようユーザーに案内する。

要するに: **spec-audit は findings の read-only な source** であり、実際のテスト追加は
**condukt / backlog+flow (executor)** が行う、という役割分担です。

## 注意

- **read-only の徹底**: このコマンドは仕様書もコードもテストも一切 `Edit`/`Write`
  しません (`allowed-tools` に `Edit` を含めていないのはそのためです)。`Write` は
  手順 5 の ingest 一時ファイル書き出しにのみ使います。coverage 不足のテスト追加は
  手順 6 で backlog/condukt (executor) にハンドオフし、仕様/実装側の drift 是正は
  `/specguard:drift-map` に委ねてください。
- このコマンドの read-only 保証は subagent の **ツール名レベル** (Edit/Write/network
  剥奪 + 読み取り専用 git のプロンプト規律) による。
- scope 源は **仕様マップ store** (`.specguard/spec-map.toml`、feature/endpoint 単位)
  であり、`/specguard:run` の git-diff スコープとは異なる。マップの保守ロジック
  (build/sync) は `specguard map` バイナリに委譲し、このコマンドでは再実装しない。
- レポート/sentinel/baseline の意味づけ・判定ロジックはすべてバイナリ側が単一の
  正典として決める。このコマンドは「ターゲット解決 → マップ鮮度確保 → 描画 →
  subagent → ingest → (coverage 是正の backlog ハンドオフ)」の配管に徹する。
