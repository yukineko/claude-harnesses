コミット・マージ・プッシュは決して自動実行しない。明示的なユーザー承認を必ず先に得ること。

`ship check`・`ship check --run-safe`・`ship rollout` が実行する git サブコマンドは読み取り専用のものだけである。
`scripts/rebuild-plugins.sh` と `scripts/rollout-plugins.sh` のどちらにも、状態を変更する git コマンドは含まれない。

```sh
ship check             # チェックリストを表示：未コミット変更、未マージの condukt/* ブランチ、残った worktree、未 push コミット、古いプラグインバイナリ
ship check --run-safe  # さらに repo root で scripts/rebuild-plugins.sh --stage-repo を実行し、GATED 項目を再表示
ship rollout           # repo root で scripts/rollout-plugins.sh を引数なしで実行
ship session-end       # SessionEnd hook のコマンド
```

`ship check` は作業ツリーも stage 済みの内容も `HEAD` も変更しない。
`ship check --run-safe` は `scripts/rollout-plugins.sh` を自分では実行せず、`ship rollout` は `scripts/rebuild-plugins.sh` を自分では実行しない。
`scripts/rollout-plugins.sh` は `scripts/rebuild-plugins.sh` を `--stage-repo` なしで実行し、`scripts/rebuild-plugins.sh` が `crates/*/bin` へコピーするのは `--stage-repo` のときだけである。

プラグインが登録する hook は 1 つ：`SessionEnd` が `${CLAUDE_PLUGIN_ROOT}/bin/ship session-end` を実行する。
セッション後、`/ship` を実行して出荷リチュアルをステップバイステップで進めること。

stale 判定は、各クレートの `src/` 配下の最新 mtime をローカルの `crates/<name>/bin/<name>-linux-x86_64`
と比べる。
このリポジトリではプラットフォーム別バイナリ（`crates/*/bin/*-linux-x86_64`）は git 管理外である。

## コミット・マージ・プッシュを決して自動実行しない

- **コミット・マージ・プッシュ**: 決して自動実行しない。明示的なユーザー承認を必ず得ること。diff を表示したうえで「承認？」と尋ね、「了解」まで待つこと。

## プラグインをインストールすること

```
/plugin install ship@yukineko
```

## ソースからインストールすること

```sh
cargo install --path .
```

ライセンス: MIT
