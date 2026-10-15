再利用可能な「手順インクルード」 [test: crates/runbook/tests/integration.rs::help_describes_procedure_includes]。プロンプトに書いた `!name` マクロを、リポジトリにコミットされた手順書（`.runbook/<name>.md`）へ展開する `UserPromptSubmit` フックである [test: crates/runbook/tests/prose_claims.rs::claim_inject_expands_macro] [test: crates/runbook/tests/prose_claims.rs::claim_hooks_json_wires_userpromptsubmit]。

手順は 1 ファイル 1 手順の素のマークダウンとして、プロジェクトの `.runbook/`（コミット対象）または `~/.runbook/runbooks/`（グローバル）に置く [test: crates/runbook/tests/prose_claims.rs::claim_store_dirs_project_and_global]。ファイル名の語幹がマクロ名になる（`deploy.md` → `!deploy`） [test: crates/runbook/tests/prose_claims.rs::claim_file_stem_is_macro]。`UserPromptSubmit` フックは、ユーザーがプロンプト中に書いた `!name` を検出し、対応する手順書の本文（Overview / Procedure / Specifications / Forbidden Actions など）を stdout に出力する [test: crates/runbook/tests/prose_claims.rs::claim_inject_expands_macro]。

```
> follow !release-plugin and cut a new version
```

→ フックが `release-plugin` 手順を stdout に出力する [test: crates/runbook/tests/prose_claims.rs::claim_inject_expands_macro]。

フックはプロンプトが要求した手順だけを注入する [test: crates/runbook/tests/prose_claims.rs::claim_inject_only_requested_under_budget]。次の手順で `max_chars` を超える場合、それ以降は `残り N 件` の注記つきで省略される [test: crates/runbook/tests/prose_claims.rs::claim_inject_only_requested_under_budget]。ただし文字数バジェットは厳密ではなく、最初の手順は常に含まれる（`per_runbook_chars` に見出しと省略記号を加えた長さまで）ため、出力は `max_chars` を超えうる [test: crates/runbook/tests/prose_claims.rs::claim_inject_only_requested_under_budget]。期待した手順が注入されなかったら `runbook list` / `runbook status` で確認する（これらは読めないストアを `unknown — …` と報告し、`list` / `show` は exit 1 する） [test: crates/runbook/tests/prose_claims.rs::claim_unreadable_store_reported]。実際の注入は、ハーネス共通の inject-metrics 台帳（`~/.harness/state/inject-metrics.jsonl`）にも 1 行として記録される [test: crates/runbook/tests/prose_claims.rs::claim_inject_metrics_ledger]。

`!name` は文の途中に差し込め、複数を重ねられる（`!build !test`） [test: crates/runbook/src/inject.rs::scans_boundaried_tokens_only] [test: crates/runbook/src/inject.rs::expand_matches_and_dedupes]。 マクロは*既存の手順に解決できたときだけ*発火するため、散文やコード中の `!`（`x != y`、`!!`、`foo!`）は何も注入しない [test: crates/runbook/src/inject.rs::ignores_non_boundary_and_operators] [test: crates/runbook/src/inject.rs::unknown_token_injects_nothing]。

`hooks/hooks.json` が `UserPromptSubmit` フック（`bin/runbook inject`）を配線する [test: crates/runbook/tests/prose_claims.rs::claim_hooks_json_wires_userpromptsubmit]。あとはリポジトリの `.runbook/` に手順を追加し、プロンプト中で `!name` と書いて呼び出すだけでよい [test: crates/runbook/tests/prose_claims.rs::claim_inject_expands_macro]。利用可能な手順の一覧を注入したいときは、プロンプトに `!runbooks` と書く [test: crates/runbook/src/inject.rs::index_token_requests_index]。

手順ファイルには任意で TOML フロントマターを付け、説明やエイリアスを指定できる [test: crates/runbook/src/store.rs::parse_frontmatter] [test: crates/runbook/src/store.rs::parse_body_only]。

```markdown
+++
description = "本番デプロイ手順"
aliases = ["ship"]
+++

# deploy
## Overview …
## Procedure …
## Forbidden Actions …
```

```sh
cargo install --path .
runbook init                 # .runbook/ とサンプル手順を作成 [test: crates/runbook/tests/prose_claims.rs::claim_init_creates_sample]
runbook new deploy --description "本番デプロイ手順"
runbook list                 # 利用可能なマクロを表示 [test: crates/runbook/tests/prose_claims.rs::claim_list_shows_macros]
runbook show deploy          # 手順を 1 つ表示 [test: crates/runbook/tests/prose_claims.rs::claim_show_prints_one]
runbook install              # UserPromptSubmit フックを ~/.claude/settings.json にマージ [test: crates/runbook/tests/prose_claims.rs::claim_install_merges_hook] [test: crates/runbook/tests/prose_claims.rs::claim_install_dryrun_idempotent_backup_foreign]
runbook status               # 解決済みの設定・ディレクトリ・件数を表示 [test: crates/runbook/tests/prose_claims.rs::claim_status_shows_config_dirs_count]
runbook uninstall
```

`runbook new` は `--global`（プロジェクトではなくグローバルディレクトリに作成）と `--force`（既存ファイルを上書き）も受け付ける [test: crates/runbook/tests/prose_claims.rs::claim_new_global_force_normalize]。名前は小文字の `[a-z0-9_-]`（最大 48 文字）に正規化される [test: crates/runbook/tests/prose_claims.rs::claim_new_global_force_normalize]。`runbook install` / `uninstall` は `--dry-run` を受け付け、他プラグインのフックグループを保持する [test: crates/runbook/tests/prose_claims.rs::claim_install_dryrun_idempotent_backup_foreign]。install を 2 回実行しても runbook のフックグループは 1 つで、install は `settings.json` をバックアップする [test: crates/runbook/tests/prose_claims.rs::claim_install_dryrun_idempotent_backup_foreign]。

同名の手順はプロジェクト側がグローバル側を隠す [test: crates/runbook/tests/prose_claims.rs::claim_project_shadows_global]。プロンプトに書いたマクロ（名前・エイリアス）は大文字小文字を区別せずに照合する [test: crates/runbook/tests/prose_claims.rs::claim_case_insensitive_names_aliases]。`index_token` と同名の実在 runbook がある場合、`!runbooks` は一覧ではなくその runbook を注入する [test: crates/runbook/tests/prose_claims.rs::claim_real_index_token_runbook_wins]。

[`runbook.example.toml`](runbook.example.toml) を参照 [test: crates/runbook/tests/prose_claims.rs::claim_example_toml_keys]。`enabled`、`project_dir`、`global_dir`、`include_global`、`prefix`、`index_token`、`max_chars`、`per_runbook_chars` の各キーが記載されている [test: crates/runbook/tests/prose_claims.rs::claim_example_toml_keys]。`RUNBOOK_DISABLE` に空でも `0` でもない値を入れるとフックを無効化する [test: crates/runbook/tests/prose_claims.rs::claim_runbook_disable_env]。

設定は `./runbook.toml`（プロジェクト）**または** `~/.runbook/config.toml`（グローバル）から読む — 先に存在した方が勝ち、両者は**マージされない** [test: crates/runbook/tests/prose_claims.rs::claim_config_first_file_wins]。`max_chars` は下限 200、`per_runbook_chars` は `[200, max_chars]` に clamp される [test: crates/runbook/tests/prose_claims.rs::claim_budget_floor_clamp]。

```sh
cargo build --release -p runbook
cargo test -p runbook
```

プラットフォーム別の `bin/runbook-<os>-<arch>` は**コミットされない**（git-ignore されており、追跡されるのは `bin/runbook` ランチャーのみ） [test: crates/runbook/tests/prose_claims.rs::claim_platform_bins_not_committed]。crate の `Makefile`（`make bins`）は crate ローカルの `target/release/` からコピーするが、ワークスペースビルドはそこに出力しないので、ここでは機能するビルド手順ではない [test: crates/runbook/tests/prose_claims.rs::claim_makefile_crate_local_target]。
