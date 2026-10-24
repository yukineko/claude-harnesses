playbook は、ノートを蓄え、プロンプトのたびに関連するノートと `always` ノートを stdout に出力する CLI 兼 Claude Code の **UserPromptSubmit** フックである。

- ノートは TOML フロントマター（`+++` でフェンス）付きの Markdown ファイル。 `title` / `tags` / `triggers` / `always` を持つ。
- ノートはプロジェクト単位のストア（`<store>/<basename>-<hash>/`）と、横断共有の `_global/` ストアに分かれて置かれる。

注入は `playbook inject` が担い、**UserPromptSubmit** フックに配線される。 プラグインとして導入した場合は `hooks/hooks.json` が `${CLAUDE_PLUGIN_ROOT}/bin/playbook inject` を起動する。 スタンドアロンで `cargo install` した場合は次で設定する（`settings.json` はバックアップされる）。

```sh
cargo install --path .
playbook install                 # UserPromptSubmit フックを配線する
```

プロンプトごとに `inject` はナレッジ注入を行う（`/flow` を勧めるキーワード分類器は、状態確認の質問や sub-agent の報告に誤爆したため撤去した）:

**ナレッジ注入** — (1) cwd から見えるノート（プロジェクトストア＋`_global`）を読み、(2) プロンプトに対して各ノートをスコアリングし（`triggers` ×5 ＞ `tags` ×3 ＞ タイトル語 ×2 ＞ 本文一致（上限 4）。 CJK は 1 文字単位でトークン化するので日本語のタイトルは日本語プロンプトに一致するが、複数文字の日本語 trigger はスコア 0 になる（1 文字のものだけが一致する））、(3) `always` ノートを先に、続いて `min_score` 以上の上位を `top_k` まで選び、`max_chars` 予算を超える scored ノートは落とし、(4) stdout に出力する。

```sh
playbook add --title "メモリ: 一括読込み禁止" \
  --trigger "pandas,read_csv,lightgbm,memory" --tags "data" \
  --body "read_csv 等で全件ロード禁止。chunksize / ParquetFile.read_row_group で部分読み。"

playbook add --title "commit は branch を切ってから" --always \
  --body "main で直接コミットしない。必ず作業ブランチを切る。"
# --global で横断共有の _global ストアに保存。--body を省くと本文は stdin から読む

playbook list                              # ノート一覧
playbook search lightgbm が OOM で落ちる    # ノートごとのスコアを表示（✓ は min_score 以上のノートと always ノート）
playbook rm <slug>                         # ノート削除
playbook status                            # 解決された設定・ストアパス・可視ノート数
playbook init                              # ストアディレクトリとサンプルノートを作成
playbook uninstall                         # フックを外す（install/uninstall は --dry-run 可）
```

フックを配線せずに注入を試すには、`inject` に stdin で JSON を渡す。

```sh
echo '{"cwd":"'$PWD'","prompt":"lightgbm が OOM で落ちる"}' | playbook inject
```

設定は `playbook.example.toml` を参照。 プロジェクトの `./playbook.toml` ＞ `~/.playbook/config.toml` ＞ 組み込みデフォルト の順で、最初に存在したファイルが採用される（マージはされない）。

| key | 意味 | デフォルト |
|---|---|---|
| `store_dir` | ノートストアのルート | `~/.playbook/store` |
| `top_k` | プロンプトごとに注入する scored ノートの最大数（`always` ノートは数えない） | 3 |
| `min_score` | 関連度のしきい値（`always` は迂回する） | 5 |
| `max_chars` | scored ノートの文字数予算（`always` ノートは超過しても全文注入される。超過する scored ノートは、最初に採用されるノートでない限り落とされる） | 1500 |
| `include_global` | `_global` ストアも検索するか | true |

キルスイッチ: `PLAYBOOK_DISABLE` に `0` 以外の空でない値を設定すると `inject` は何もしない。
