再利用可能な「手順インクルード」。プロンプトに書いた `!name` マクロを、リポジトリにコミットされた手順書（`.runbook/<name>.md`）へ展開する `UserPromptSubmit` フックである。

手順は 1 ファイル 1 手順の素のマークダウンとして、プロジェクトの `.runbook/`（コミット対象）または `~/.runbook/runbooks/`（グローバル）に置く。ファイル名の語幹がマクロ名になる（`deploy.md` → `!deploy`）。`UserPromptSubmit` フックは、ユーザーがプロンプト中に書いた `!name` を検出し、対応する手順書の本文（Overview / Procedure / Specifications / Forbidden Actions など）を stdout に出力する。

```
> follow !release-plugin and cut a new version
```

→ フックが `release-plugin` 手順を stdout に出力する。

フックはプロンプトが要求した手順だけを注入する。次の手順で `max_chars` を超える場合、それ以降は `残り N 件` の注記つきで省略される。ただし文字数バジェットは厳密ではなく、最初の手順は常に含まれる（`per_runbook_chars` に見出しと省略記号を加えた長さまで）ため、出力は `max_chars` を超えうる。期待した手順が注入されなかったら `runbook list` / `runbook status` で確認する（これらは読めないストアを `unknown — …` と報告し、`list` / `show` は exit 1 する）。実際の注入は、ハーネス共通の inject-metrics 台帳（`~/.harness/state/inject-metrics.jsonl`）にも 1 行として記録される。

`!name` は文の途中に差し込め、複数を重ねられる（`!build !test`）。 マクロは*既存の手順に解決できたときだけ*発火するため、散文やコード中の `!`（`x != y`、`!!`、`foo!`）は何も注入しない。

`hooks/hooks.json` が `UserPromptSubmit` フック（`bin/runbook inject`）を配線する。あとはリポジトリの `.runbook/` に手順を追加し、プロンプト中で `!name` と書いて呼び出すだけでよい。利用可能な手順の一覧を注入したいときは、プロンプトに `!runbooks` と書く。

手順ファイルには任意で TOML フロントマターを付け、説明やエイリアスを指定できる。

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
runbook init                 # .runbook/ とサンプル手順を作成
runbook new deploy --description "本番デプロイ手順"
runbook list                 # 利用可能なマクロを表示
runbook show deploy          # 手順を 1 つ表示
runbook install              # UserPromptSubmit フックを ~/.claude/settings.json にマージ
runbook status               # 解決済みの設定・ディレクトリ・件数を表示
runbook uninstall
```

`runbook new` は `--global`（プロジェクトではなくグローバルディレクトリに作成）と `--force`（既存ファイルを上書き）も受け付ける。名前は小文字の `[a-z0-9_-]`（最大 48 文字）に正規化される。`runbook install` / `uninstall` は `--dry-run` を受け付け、他プラグインのフックグループを保持する。install を 2 回実行しても runbook のフックグループは 1 つで、install は `settings.json` をバックアップする。

同名の手順はプロジェクト側がグローバル側を隠す。プロンプトに書いたマクロ（名前・エイリアス）は大文字小文字を区別せずに照合する。`index_token` と同名の実在 runbook がある場合、`!runbooks` は一覧ではなくその runbook を注入する。

[`runbook.example.toml`](runbook.example.toml) を参照。`enabled`、`project_dir`、`global_dir`、`include_global`、`prefix`、`index_token`、`max_chars`、`per_runbook_chars` の各キーが記載されている。`RUNBOOK_DISABLE` に空でも `0` でもない値を入れるとフックを無効化する。

設定は `./runbook.toml`（プロジェクト）**または** `~/.runbook/config.toml`（グローバル）から読む — 先に存在した方が勝ち、両者は**マージされない**。`max_chars` は下限 200、`per_runbook_chars` は `[200, max_chars]` に clamp される。

```sh
cargo build --release -p runbook
cargo test -p runbook
```

プラットフォーム別の `bin/runbook-<os>-<arch>` は**コミットされない**（git-ignore されており、追跡されるのは `bin/runbook` ランチャーのみ）。crate の `Makefile`（`make bins`）は crate ローカルの `target/release/` からコピーするが、ワークスペースビルドはそこに出力しないので、ここでは機能するビルド手順ではない。
