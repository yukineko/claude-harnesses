# trajectoryeval

> 🌐 [English](README.md) ・ **日本語**

**軌跡 (trajectory) 照合検証ツール — 出力検証 (output verifier) の対になる存在。**

[langchain-ai/agentevals](https://github.com/langchain-ai/agentevals) の trajectory matcher に着想を得ている。**subscription-native**: 同梱の Rust バイナリ 1 つで完結し、API キーもネットワークも要らない。

## 目的

worker が**たどった経路**を検証する。具体的には、worker が実行した**ツール呼び出しの順序付き列**を、期待される軌跡 spec と突き合わせる。

condukt のオンライン検証器がタスクの**出力 (OUTPUT)**(その `done_criteria`)を見るのに対し、trajectoryeval は worker が結果に至るまでの**経路 (PATH)** を見る。両者は兄弟関係にあり、片方は「何が出来たか」を、もう片方は「どうやって出来たか」を検証する。

照合結果は `{ pass, missing, unexpected, out_of_order }` として報告される。

## どうして必要か

出力だけを検証すると、「結果は正しいが、やり方が間違っている」軌跡を見逃す。たとえば、本来 Read してから Edit すべきところを Read を飛ばして編集していたり、想定外のツールを呼んでいたり、ステップの順序が入れ替わっていても、最終的な出力さえ `done_criteria` を満たせば出力検証は通ってしまう。

trajectoryeval はこの死角を埋める。期待する経路を spec として宣言しておけば、必須ステップの欠落 (`missing`)、想定外のツール呼び出し (`unexpected`)、順序の乱れ (`out_of_order`) を機械的に検出できる。出力検証と組み合わせることで、「正しい結果を、正しい手順で」得られたかを担保する。

## どう使うか

これはライフサイクル hook ではなく、ふつうの CLI **ゲート**である。slash command ではなく、同梱バイナリ `trajectoryeval` のサブコマンドとして起動する。

### サブコマンド

#### `trajectoryeval extract --transcript <jsonl>`

Claude Code のトランスクリプトを**1 行ずつ**ストリーム処理し(全体をメモリに載せない)、`tool_use` の名前を出現順に JSON 配列として stdout に出力する。そのまま `check --actual` に流し込める。

#### `trajectoryeval check --expected <spec.json> --actual <actual.json> [--json]`

実際のツール列を期待 spec と照合し、`{ pass, missing, unexpected, out_of_order }` を報告する(人間向けレポート、または `--json` でシリアライズ結果)。

#### `trajectoryeval tier --config <cfg.json> --flow <id> [...]`

**リスク階層化された e2e 検証。** 設定駆動の「コア」フロー allowlist で、フローを **core**(ビジネスクリティカル)か **non-core** に分類し、階層に応じた検証を行う。分類・diff・サンプリングはいずれも決定論的な純関数(LLM・ネットワーク・時計に依存しない)。

- **config** JSON:
  ```json
  { "core": ["checkout", "payment"],
    "diff_strategy": "structured_data",
    "sample_one_in": 20 }
  ```
  `core` = コア allowlist(exact-match)。`diff_strategy` の既定は `structured_data`。`sample_one_in` = non-core フローを `1 in N` 回だけ深くサンプルする率(0 で無効=存在チェックのみ)。

- **core フロー**: 毎回スナップショットを取得して実際に **diff** する。`--baseline <base.json> --snapshot <snap.json>` を渡す。このリポジトリはデプロイ済みランタイム UI を持たないため、常時利用可能な一次メカニズムは決定論的な **構造化データ照合**(正規化された shape の JSON 等価性。オブジェクトのキー順は正規化され無視、値・構造は一致必須)。差異があれば差分位置を JSON ポインタで列挙する。
  - **perceptual-hash / screenshot** 比較は `diff_strategy: "screenshot"` として trait/enum 境界の背後に用意してあるが、これは**正直な stub**(未実装)で、選ぶと `DiffOutcome::Stubbed` を返すだけ。後から実装を差し込めるよう境界だけ切ってある。

- **non-core フロー**: 軽量な **存在チェック**(specguard spec-audit 流の「そのフローが在るか」)。`--exists true|false`(既定 true)。さらに `sample_one_in > 0` なら、`(flow_id, --seed, --run-index)` を鍵にした **決定論的な低頻度サンプリング**(seedable。unseeded な乱数は使わない)で、その run が深いサンプル対象かを決める。同じ入力なら常に同じ判定。

CLI はフローの core/non-core 分類と diff(match/mismatch)結果、または non-core の存在/サンプリング判定を、人間向けレポートまたは `--json` で報告する。

- **expected** spec の JSON:
  ```json
  { "mode": "strict",
    "steps": [ { "tool": "Read" }, { "tool": "Edit", "optional": true } ] }
  ```
  `optional` の既定値は `false`。
- **actual** の JSON: ツール名文字列の配列、例 `["Read", "Edit"]`(`extract` の出力をそのまま渡せる)。

### モード

- **strict** — 実際の列が、期待される**必須**ステップと順序まで一致しなければならない。optional ステップは欠けていてもよいが、存在する場合は所定の位置になければならない。`missing` = マッチしなかった必須ステップ、`unexpected` = 期待順序のどこにも収まらなかった実ツール、`out_of_order` = 正しい集合は揃ったが順序が誤っている。
- **unordered** — 順序を無視する。`missing` = 実際に存在しない必須期待ツール(集合として)、`unexpected` = 期待集合に無い実ツール、`out_of_order` は常に false。
- **subsequence** — 必須ステップが `actual` の中に順序どおり現れればよい(連続している必要はなく、他のツールが間に挟まってよい)。`missing` = 順序付き部分列として見つからなかった必須ステップ。余分は許容されるので `unexpected` は空のまま、`out_of_order` は false。

いずれのモードでも: `pass = missing.is_empty() && unexpected.is_empty() && !out_of_order`。

### 終了コード

evalkit / schemaguard と同じ 0/1/2 ゲートポリシーに従う。

| code | 意味 |
|------|------|
| `0`  | 軌跡が spec と一致した (pass) |
| `1`  | 逸脱あり (missing / unexpected / 順序違反) |
| `2`  | harness エラー (入力が読めない / パースできない) |

## condukt への組み込み (Phase 6)

`trajectoryeval` は単なるスタンドアロンバイナリではない — condukt のタスク schema は
`expected_trajectory: {mode, steps:[{tool}]}` という省略可フィールドを持ち
(`crates/condukt/src/model.rs`)、正しさが「何ができたか」だけでなく「どうやったか」に
依存するタスクでは condukt-interpreter がこれを埋める。タスクがこれを持つ場合、condukt の
Phase 6 (`crates/condukt/skills/condukt/SKILL.md`) は worker sub-agent の transcript に対して
`extract` を実行し、その結果を通常の出力検証と**並行して** `check` に流し込む — 第2の、経路面の
verifier 次元として機能する。`trajectoryeval` バイナリが `PATH` に無い場合・タスクに
`expected_trajectory` が無い場合・worker の transcript が解決できない場合は、このステップは
まるごと skip される (fail-soft・Phase 6 を壊さない)。経路の逸脱 (exit `1`) は出力の判定を
**上書きしない** — 出力が `done_criteria` を満たせばそのタスクは `verified` のままとなり、
逸脱は HOTL 可視化のため `reason` として記録される。

### 最小例

```sh
trajectoryeval extract --transcript session.jsonl > actual.json
echo '{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}' > spec.json
trajectoryeval check --expected spec.json --actual actual.json --json
```
