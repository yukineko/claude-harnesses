# playbook

> 🌐 [English](README.md) ・ **日本語**

Claude Code 向けの**プロジェクト知識の蓄積＋注入**ハーネス。Rust 製。Devin の **Knowledge** 機能をローカルフックとして再現する。

## 目的

playbook は、プロジェクト固有の「事実」を 1 件 1 件の**アトミックノート**として蓄え、プロンプトのたびに関連するものだけを文脈へ注入する CLI 兼 Claude Code の **UserPromptSubmit** フックである。

- ノートは TOML フロントマター（`+++` でフェンス）付きの Markdown ファイル。`title` / `tags` / `triggers` / `always` を持つ。
- ノートはプロジェクト単位のストア（`<store>/<basename>-<hash>/`）と、横断共有の `_global/` ストアに分かれて置かれる。
- 注入の判定はすべて**決定論的**で、埋め込みも API キーも使わない（キーワード＋トリガーのスコアリングのみ）。サブスクリプションで完結する。

注入処理（フック経路）が本体の背骨であり、ほかのサブコマンドは同じストアと設定を扱う**キュレーション用ツール**にすぎない。

## どうして必要か

エージェントには同じ指示を何度も繰り返してしまう——「ここはチャンク読み込みにして」「コミット前にブランチを切って」「ステージング URL は X だ」。こうした規約は、放っておくと毎回手入力するか、忘れられて事故になる。かといって CLAUDE.md に全部書き込めば文脈が膨らみ続ける。

playbook はこの痛みを次のように解く。

- **規約の再浮上。** 一度ノートにしておけば、関連するプロンプトのときだけ自動で文脈に戻ってくる。毎回言い直す必要がない。
- **文脈の肥大化を防ぐ。** `max_chars` の厳格な文字数予算と `top_k` の上限の中で、関連度の高いものだけを選ぶ。無関係なら何も注入しない（関連ノートなし ⇒ 出力なし）。
- **判定がぶれない。** スコアリングは決定論的で、出力は slug で安定ソートされる。埋め込みや外部 API に依存しないので、同じプロンプトには同じ結果が返る。
- **ターンを壊さない。** フックはパニックを握りつぶし常に正常終了する。stdin が壊れていても、ストアが無くても、関連ノートが無くても、黙って何も出さない——プロンプトを止める知識フックは、黙っているフックより悪い、という方針である。

## どう使うか

### フック配線

注入は `playbook inject` が担い、**UserPromptSubmit** フックに配線される。プラグインとして導入した場合は `hooks/hooks.json` が `${CLAUDE_PLUGIN_ROOT}/bin/playbook inject` を起動するため、追加の配線は不要。スタンドアロンで `cargo install` した場合は次で設定する（`settings.json` はバックアップされる）。

```sh
cargo install --path .
playbook install                 # UserPromptSubmit フックを配線する
```

プロンプトごとに `inject` は、次の 2 つの処理を行う:

1. **複雑性検出** — プロンプトに本番影響・Rust の複数クレート変更・テスト不可な制約・明示的な「condukt で回して」キーワードが含まれるかを決定論的に判定。複雑な問題として検出された場合、`→ /flow <problem>` 形式で system-reminder に誘導を注入し、ユーザーが手動で `/flow` を思い出さなくても複雑な作業が parallel/serial ルーティング対象になるようにする。

2. **ナレッジ注入** — (1) cwd から見えるノート（プロジェクトストア＋`_global`）を読み、(2) プロンプトに対して各ノートをスコアリングし（`triggers` ×5 ＞ `tags` ×3 ＞ タイトル語 ×2 ＞ 本文一致（上限あり）。CJK は 1 文字単位でトークン化するので日本語プロンプトも一致する）、(3) `always` ノートを先に、続いて `min_score` を超える上位を `top_k` まで、`max_chars` 予算に達するまで選び、(4) 追加文脈として出力する。

複雑性検出はキーワードベースの軽量分類器で、LLM 判定を使わない。複雑と判定された場合でも既存のナレッジ注入は並行して実行される（fail-soft）。

### ノートのキュレーション

```sh
playbook add --title "メモリ: 一括読込み禁止" \
  --trigger "pandas,read_csv,lightgbm,memory" --tags "data" \
  --body "read_csv 等で全件ロード禁止。chunksize / ParquetFile.read_row_group で部分読み。"

playbook add --title "commit は branch を切ってから" --always \
  --body "main で直接コミットしない。必ず作業ブランチを切る。"

playbook list                              # ノート一覧
playbook search lightgbm が OOM で落ちる    # 何が注入されるか（スコア付き、✓ が注入対象）を確認
playbook rm <slug>                         # ノート削除
playbook status                            # 解決された設定・ストアパス・可視ノート数
```

フックを配線せずに注入を試すには、`inject` に stdin で JSON を渡す。

```sh
echo '{"cwd":"'$PWD'","prompt":"lightgbm が OOM で落ちる"}' | playbook inject
```

### 設定

設定は `playbook.example.toml` を参照。プロジェクトの `./playbook.toml` ＞ `~/.playbook/config.toml` ＞ 組み込みデフォルト の順で、最初に存在したファイルが採用される（マージはされない）。

| key | 意味 | デフォルト |
|---|---|---|
| `top_k` | プロンプトごとに注入する最大ノート数 | 3 |
| `min_score` | 関連度のしきい値（`always` は迂回する） | 5 |
| `max_chars` | 注入文字数の上限 | 1500 |
| `include_global` | `_global` ストアも検索するか | true |

キルスイッチ: `PLAYBOOK_DISABLE=1` を設定すると `inject` は何もしない。
