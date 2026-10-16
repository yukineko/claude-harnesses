playbook は、ノートを蓄え、プロンプトのたびに関連するノートと `always` ノートを stdout に出力する CLI 兼 Claude Code の **UserPromptSubmit** フックである [test: crates/playbook/tests/prose_claims.rs::claim_hooks_json_wiring] [test: crates/playbook/tests/prose_claims.rs::claim_nothing_relevant_nothing_injected] [test: crates/playbook/tests/prose_claims.rs::claim_note_format]。

- ノートは TOML フロントマター（`+++` でフェンス）付きの Markdown ファイル [test: crates/playbook/tests/prose_claims.rs::claim_note_format]。 `title` / `tags` / `triggers` / `always` を持つ [test: crates/playbook/tests/prose_claims.rs::claim_note_format] [test: crates/playbook/tests/prose_claims.rs::claim_score_weights]。
- ノートはプロジェクト単位のストア（`<store>/<basename>-<hash>/`）と、横断共有の `_global/` ストアに分かれて置かれる [test: crates/playbook/tests/prose_claims.rs::claim_visible_stores]。

注入は `playbook inject` が担い、**UserPromptSubmit** フックに配線される [test: crates/playbook/tests/prose_claims.rs::claim_hooks_json_wiring]。 プラグインとして導入した場合は `hooks/hooks.json` が `${CLAUDE_PLUGIN_ROOT}/bin/playbook inject` を起動する [test: crates/playbook/tests/prose_claims.rs::claim_hooks_json_wiring]。 スタンドアロンで `cargo install` した場合は次で設定する（`settings.json` はバックアップされる） [test: crates/playbook/tests/prose_claims.rs::claim_install_backs_up]。

```sh
cargo install --path .
playbook install                 # UserPromptSubmit フックを配線する [test: crates/playbook/tests/prose_claims.rs::claim_install_backs_up]
```

プロンプトごとに `inject` は、次の 2 つの処理を行う: [test: crates/playbook/tests/prose_claims.rs::claim_flow_and_notes_together]

1. **複雑性検出** — プロンプトに明示的な flow 要求（「condukt で回して」等）が含まれるか、本番影響・Rust の変更・テスト不可な制約のうち 2 つ以上が含まれるかをキーワードで判定 [test: crates/playbook/tests/prose_claims.rs::claim_complex_prompt_flow_line]。 複雑な問題として検出された場合、`→ /flow <problem>` 形式の誘導を stdout に出力する [test: crates/playbook/tests/prose_claims.rs::claim_complex_prompt_flow_line]。

2. **ナレッジ注入** — (1) cwd から見えるノート（プロジェクトストア＋`_global`）を読み、(2) プロンプトに対して各ノートをスコアリングし（`triggers` ×5 ＞ `tags` ×3 ＞ タイトル語 ×2 ＞ 本文一致（上限 4） [test: crates/playbook/tests/prose_claims.rs::claim_visible_stores] [test: crates/playbook/tests/prose_claims.rs::claim_score_weights]。 CJK は 1 文字単位でトークン化するので日本語のタイトルは日本語プロンプトに一致するが、複数文字の日本語 trigger はスコア 0 になる（1 文字のものだけが一致する））、(3) `always` ノートを先に、続いて `min_score` 以上の上位を `top_k` まで選び、`max_chars` 予算を超える scored ノートは落とし、(4) stdout に出力する [test: crates/playbook/tests/prose_claims.rs::claim_score_weights] [test: crates/playbook/tests/prose_claims.rs::claim_score_weights_japanese_trigger_observed] [test: crates/playbook/tests/prose_claims.rs::claim_selection_order_budget] [test: crates/playbook/tests/prose_claims.rs::claim_nothing_relevant_nothing_injected]。

複雑と判定された場合でも既存のナレッジ注入は並行して実行される [test: crates/playbook/tests/prose_claims.rs::claim_flow_and_notes_together]。

```sh
playbook add --title "メモリ: 一括読込み禁止" \
  --trigger "pandas,read_csv,lightgbm,memory" --tags "data" \
  --body "read_csv 等で全件ロード禁止。chunksize / ParquetFile.read_row_group で部分読み。"

playbook add --title "commit は branch を切ってから" --always \
  --body "main で直接コミットしない。必ず作業ブランチを切る。"
# --global で横断共有の _global ストアに保存 [test: crates/playbook/tests/prose_claims.rs::claim_add_global_and_stdin]。--body を省くと本文は stdin から読む [test: crates/playbook/tests/prose_claims.rs::claim_add_global_and_stdin]

playbook list                              # ノート一覧 [test: crates/playbook/tests/prose_claims.rs::claim_list_shows_notes]
playbook search lightgbm が OOM で落ちる    # ノートごとのスコアを表示（✓ は min_score 以上のノートと always ノート） [test: crates/playbook/tests/prose_claims.rs::claim_search_check_mark]
playbook rm <slug>                         # ノート削除 [test: crates/playbook/tests/prose_claims.rs::claim_rm_removes]
playbook status                            # 解決された設定・ストアパス・可視ノート数 [test: crates/playbook/tests/prose_claims.rs::claim_status_output]
playbook init                              # ストアディレクトリとサンプルノートを作成 [test: crates/playbook/tests/prose_claims.rs::claim_init_creates]
playbook uninstall                         # フックを外す（install/uninstall は --dry-run 可） [test: crates/playbook/tests/prose_claims.rs::claim_uninstall_and_dry_run]
```

フックを配線せずに注入を試すには、`inject` に stdin で JSON を渡す [test: crates/playbook/tests/prose_claims.rs::claim_inject_stdin_json]。

```sh
echo '{"cwd":"'$PWD'","prompt":"lightgbm が OOM で落ちる"}' | playbook inject
```

設定は `playbook.example.toml` を参照 [test: crates/playbook/tests/prose_claims.rs::claim_example_toml]。 プロジェクトの `./playbook.toml` ＞ `~/.playbook/config.toml` ＞ 組み込みデフォルト の順で、最初に存在したファイルが採用される（マージはされない） [test: crates/playbook/tests/prose_claims.rs::claim_config_precedence]。

| key | 意味 | デフォルト [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] |
|---|---|---|
| `store_dir` | ノートストアのルート | `~/.playbook/store` [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] |
| `top_k` | プロンプトごとに注入する scored ノートの最大数（`always` ノートは数えない） | 3 [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] [test: crates/playbook/tests/prose_claims.rs::claim_top_k_excludes_always] |
| `min_score` | 関連度のしきい値（`always` は迂回する） | 5 [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] |
| `max_chars` | scored ノートの文字数予算（`always` ノートは超過しても全文注入される。超過する scored ノートは、最初に採用されるノートでない限り落とされる） | 1500 [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] [test: crates/playbook/tests/prose_claims.rs::claim_selection_order_budget_max_chars_not_hard_cap_observed] |
| `include_global` | `_global` ストアも検索するか | true [test: crates/playbook/tests/prose_claims.rs::claim_config_defaults] |

キルスイッチ: `PLAYBOOK_DISABLE` に `0` 以外の空でない値を設定すると `inject` は何もしない [test: crates/playbook/tests/prose_claims.rs::claim_kill_switch]。
