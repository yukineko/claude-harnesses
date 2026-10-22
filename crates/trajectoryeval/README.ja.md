`trajectoryeval` は軌跡 (trajectory) 照合検証ツールである。

 [langchain-ai/agentevals](https://github.com/langchain-ai/agentevals) の trajectory matcher に着想を得ている。

`trajectoryeval` は worker が**たどった経路 (PATH)** を検証する、つまり worker が実行した**ツール呼び出しの順序付き列**を、期待される軌跡 spec と突き合わせる。

`trajectoryeval check --expected <spec.json> --actual <actual.json> [--json]` は実際のツール列を期待 spec と照合し、`{ pass, missing, unexpected, out_of_order }` を報告する。`--json` を付けると人間向けレポートの代わりにシリアライズ結果を JSON で出力する。

`trajectoryeval tier --config <cfg.json> --flow <id> [...]` は、設定の allowlist でフローを **core**(ビジネスクリティカル)か **non-core** に分類する。

- tier の config は次の形の JSON である:
  ```json
  { "core": ["checkout", "payment"],
    "diff_strategy": "structured_data",
    "sample_one_in": 20 }
  ```
  `core` = コア allowlist(exact-match)。
  `diff_strategy` の既定は `structured_data`。
  `sample_one_in` = non-core フローを `1 in N` 回だけサンプルする率。
  `sample_one_in` が 0 ならサンプリング無効=存在チェックのみ。

- **core フロー**は毎回、スナップショットを baseline と **diff** する(`--baseline <base.json> --snapshot <snap.json>` を渡す)。
  構造化データ照合は正規化された shape の JSON 等価性で、オブジェクトのキー順は正規化され無視、値・構造は一致必須である。
  差異は `/` 区切りのパスで列挙される。
  `diff_strategy: "screenshot"` を選ぶと `DiffOutcome::Stubbed` を返す(未実装)。

- `diff_strategy: "fuzzy_hash"` は許容差つきの構造化データ照合である。決定論的なドリフト距離 `distance_permille = round(1000 × 差異のある葉の数 / 葉の総数)` を計算し(差異のある葉が1つでもあれば最低 `1` に切り上げ、実際のドリフトが `0` に丸められることはない)、`distance_permille <= threshold_permille` なら **match** とみなす。超えた場合は `DiffOutcome::DriftedBeyondThreshold`(距離と差異位置の `/` 区切りパス)を返し、これは fail ではなく **needs-human** 判定(exit `3`)になる。
  `threshold_permille` は config のキーで、既定は `0` である。
  [`examples/tier-config-fuzzy.json`](examples/tier-config-fuzzy.json) は `threshold_permille: 250` を設定している。

- **non-core フロー**は軽量な **存在チェック** を受ける(`--exists true|false`)。`--exists` に既定値は無く、non-core フローで省略すると harness エラー(exit `2`)になる。さらに `sample_one_in > 0` なら、`(flow_id, --seed, --run-index)` を鍵にした **決定論的な低頻度サンプリング**で、その run がサンプル対象かを決め、同じ入力なら常に同じ判定になる。
  サンプリング判定は**報告されるだけ**(`present (sampled this run)` / `present (not sampled this run)`)。
  どちらも pass。
  non-core フローを fail にするのは `--exists false` だけ。

CLI はフローの core/non-core 分類と diff(match/mismatch)結果、または non-core の存在/サンプリング判定を、人間向けレポートまたは `--json` で報告する。

- `tier` の終了コード `0` は pass。
- `tier` の終了コード `1` は実際の逸脱(mismatch、または存在チェック失敗)。
- `tier` の終了コード `2` は harness エラー(入力が読めない/パースできない、または core フローで `--baseline` と `--snapshot` の両方が無い)。
- `tier` の終了コード `3` は **needs-human** — core フローが未実装の diff 戦略(現状 `screenshot`)を使った、または `fuzzy_hash` のドリフトが `threshold_permille` を超えたため、自動判定を出せない。`3` は `1` と区別される。未実装の戦略で run を黙って赤にしてはならない: exit `1` にも `result: fail` にもならない。

- 実行可能な例: [`examples/tier-config.json`](examples/tier-config.json)
  ```sh
  trajectoryeval tier --config examples/tier-config.json --flow checkout \
    --baseline base.json --snapshot snap.json     # core フロー → 実 diff
  trajectoryeval tier --config examples/tier-config.json --flow settings \
    --exists true --seed 42 --run-index 3          # non-core → 存在チェック
  ```

- **expected** spec は次の形の JSON である:
  ```json
  { "mode": "strict",
    "steps": [ { "tool": "Read" }, { "tool": "Edit", "optional": true } ] }
  ```
  `mode` は必須(`strict` / `unordered` / `subsequence`)。
  `optional` の既定値は `false`。
  `steps` は省略可で既定は空。
- **actual** の JSON は、ツール名文字列の配列、例 `["Read", "Edit"]`(`extract` の出力をそのまま渡せる)。

`trajectoryeval extract --transcript <jsonl>` は Claude Code のトランスクリプトを読み、`tool_use` の名前を出現順に JSON 配列として stdout に出力し、そのまま `check --actual` に流し込める。ファイル自体が読めない場合は exit `2` になる。JSON として不正な行がある場合や、イベントが1つも無い場合も exit `2` であり、読み飛ばして空の trajectory にすることはない。

- **strict** — 実際の列が、期待される**必須**ステップと順序まで一致しなければならない。optional ステップは欠けていてもよいが、存在する場合は所定の位置になければならない。
  `missing` = マッチしなかった必須ステップ、`unexpected` = 期待順序のどこにも収まらなかった実ツール、`out_of_order` = 正しい集合は揃ったが順序が誤っている。
- **unordered** — 順序を無視する。`missing` = 実際に存在しない必須期待ツール(集合として)、`unexpected` = 期待集合に無い実ツール、`out_of_order` は常に false。
- **subsequence** — 必須ステップが `actual` の中に順序どおり現れればよい(連続している必要はなく、他のツールが間に挟まってよい)。`missing` = 順序付き部分列として見つからなかった必須ステップ。余分は許容されるので `unexpected` は空のまま、`out_of_order` は false。必須ステップを1つも持たない `subsequence` spec は何も制約しないので、`check` は pass にせず exit `2` で拒否する。

いずれのモードでも: `pass = missing.is_empty() && unexpected.is_empty() && !out_of_order`。

- `check` の終了コード `0`: 軌跡が spec と一致した (pass)。
- `check` の終了コード `1`: 逸脱あり (missing / unexpected / 順序違反)。
- `check` の終了コード `2`: harness エラー (入力が読めない / パースできない)。

これはライフサイクル hook ではなく、ふつうの CLI **ゲート**である。

condukt のタスク schema は `expected_trajectory` という省略可フィールドを持つ(`crates/condukt/src/model.rs`)。

```sh
trajectoryeval extract --transcript session.jsonl > actual.json
echo '{"mode":"strict","steps":[{"tool":"Read"},{"tool":"Edit"}]}' > spec.json
trajectoryeval check --expected spec.json --actual actual.json --json
```
