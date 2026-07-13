# runbook

Claude Code 向けの再利用可能な「手順インクルード」。プロンプトに書いた `!name` マクロを、リポジトリにコミットされた手順書（`.runbook/<name>.md`）へ展開する `UserPromptSubmit` フックである。

## 目的

runbook は、繰り返し実行するワークフロー（デプロイ、リリース発行、インシデント対応など）を、毎回まったく同じ手順で走らせるための仕組みである。

手順は 1 ファイル 1 手順の素のマークダウンとして、プロジェクトの `.runbook/`（コミット対象）または `~/.runbook/runbooks/`（グローバル）に置く。ファイル名の語幹がマクロ名になる（`deploy.md` → `!deploy`）。`UserPromptSubmit` フックは、ユーザーがプロンプト中に書いた `!name` を検出し、対応する手順書の本文（Overview / Procedure / Specifications / Forbidden Actions など）を Claude が動き出す前のコンテキストとして注入する。

```
> follow !release-plugin and cut a new version
```

→ フックが `release-plugin` 手順を注入してから Claude が処理を始める。

Devin の Playbooks（`!macro`）に着想を得ているが、ローカルかつ API キー不要のフックとして組み直してある。バンドルされた単一の Rust バイナリで動き、サブスクリプションで完結する（`ANTHROPIC_API_KEY` 不要）。フックはプロンプトが要求した手順を、厳しい文字数バジェットの下で*注入するだけ*で、常に終了コード 0 を返す——ターンをブロックすることは決してない。

同じ harness 内の `playbook` が、原子的な*事実*や規約を関連度でスコアリングして自動注入するのに対し、runbook は名前で明示的に指名されたときだけ*手順*全体を注入する。Claude の skills（`/name`、単独起動）とも異なり、`!name` は文の途中に差し込め、複数を重ねられる（`!build !test`）。

## どうして必要か

繰り返すワークフローを、その都度メモリや記憶や口頭の指示に頼って実行すると、次のような問題が起きる。

- **手順のばらつき。** 同じ「デプロイ」「リリース」でも、実行のたびに段取りや禁止事項が抜け落ち、結果が再現しない。runbook はコードと一緒にバージョン管理された 1 つの正典手順を、毎回同じ内容で注入する。
- **暗黙知の散逸。** 手順が個人の頭の中や散在するチャットログにあると、共有も改訂もできない。runbook は手順をリポジトリ内のただのマークダウンとして残すので、レビュー・差分・履歴がコードと同じ流儀で回る。
- **誤爆。** プロンプトや本文中の `!` を機械的に拾うと事故になる。runbook のマクロは*既存の手順に解決できたときだけ*発火するため、散文やコード中の `!`（`x != y`、`!!`、`foo!`）は何も注入しない。
- **ターンを壊すフックへの不安。** 重い処理や失敗で会話そのものが止まるのは避けたい。runbook のフックは注入だけを行い、文字数バジェットを超えても常に exit 0 で、ターンを止めない。`RUNBOOK_DISABLE=1` で無効化もできる。

## どう使うか

### プラグインとして

マーケットプレイスから導入すると、`hooks/hooks.json` が `UserPromptSubmit` フックを自動で配線する。あとはリポジトリの `.runbook/` に手順を追加し、プロンプト中で `!name` と書いて呼び出すだけでよい。利用可能な手順の一覧を注入したいときは、プロンプトに `!runbooks` と書く。

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

### スタンドアロン（cargo）

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

`runbook install` / `uninstall` は冪等で、`settings.json` をバックアップし、他プラグインのフックグループを保持する。

### 設定

[`runbook.example.toml`](runbook.example.toml) を参照。`enabled`、`project_dir`、`global_dir`、`include_global`、`prefix`、`index_token`、`max_chars`、`per_runbook_chars` を指定できる。`RUNBOOK_DISABLE=1` で全体を無効化する。
