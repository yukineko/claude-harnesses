# ship

Claude Code 向けの**シップ（出荷）リチュアル**。Rust 製。

`SessionEnd` で未出荷の状態（コミットされていない git 変更、古いプラグインキャッシュ）を検出し、リマインダーを表示します。
出荷ワークフロー（チェック・プラグイン再ビルド・コミット・マージ・プッシュ）を段階的にガイドします。

**GATED 不変式**: コミット・マージ・プッシュは、明示的なユーザー承認を必須とします。
`scripts/rebuild-plugins.sh`（`ship check --run-safe` 経由）と `scripts/rollout-plugins.sh`（`ship rollout` 経由）
のみが自動実行可能です。どちらも `~/.claude/plugins/` の状態のみを変更し、git は一切操作しません。
出荷リチュアルはユーザー主導であり、エージェントはチェックリストを提示し、各ゲートの前に承認を促します。

決定論的な出荷検出は、ひとつの Rust バイナリと 1 つの hook（SessionEnd）だけで完結し、API キーを必要としません。

## コマンド

```sh
ship check             # 未出荷状態を表示（dirty git、古いプラグインキャッシュ）
ship check --run-safe  # scripts/rebuild-plugins.sh を実行（安全：既存キャッシュ dir へのバイナリ差し替え）
ship rollout            # scripts/rollout-plugins.sh を実行（重い操作：/plugin update ステップを自動化）
ship session-end       # SessionEnd hook（stdin の JSON を読み、未出荷ならリマインダーを表示）
```

## ワークフロー

1. **診断**: `ship check` で何が未出荷かを確認する。
2. **自動再ビルド**: `ship check --run-safe` でプラグインキャッシュをソースから再ビルドする。
3. **ロールアウト**（必要に応じて）: `ship rollout` で `/plugin update`（インストール済みバージョンポインタの
   前進＋キャッシュ version dir 作成）を自動化し、その後 rebuild+sync まで行う。`--run-safe` とは別の、
   明示的なステップ。
4. **コミット**（ユーザー承認必須）: `git add && git commit -m "..."`
5. **マージ**（ユーザー承認必須）: `git merge <branch>`
6. **プッシュ**（ユーザー承認必須）: `git push origin <branch>`

## SessionEnd hook

SessionEnd で ship は自動実行し、未出荷の仕事があればリマインダーを表示します。
リマインダーは情報提供のみで、決してブロックしません。

## /ship skill

セッション後、`/ship` を実行して出荷リチュアルをステップバイステップで進めます。
skill は、コミット・マージ・プッシュが実行される前に、あなたが何が実行されるかを完全に把握できるようにしています。

## GATED 不変式 — 重要

- **コミット・マージ・プッシュ**: 決して自動実行しない。MUST 明示的なユーザー承認を得る。diff を表示し、「承認？」と尋ね、「了解」まで待つ。
- **rebuild-plugins.sh**: `ship check --run-safe` 経由で自動実行可能（既存キャッシュ dir へのバイナリ差し替え）。
- **rollout-plugins.sh**: `ship rollout` 経由で自動実行可能（重い・明示的な操作 — `/plugin update` の自動化）。

## インストール（プラグイン）

```
/plugin install ship@yukineko
```

## 手動インストール

```sh
cargo install --path .
ship session-end  # テストする
```

## ライセンス

MIT
