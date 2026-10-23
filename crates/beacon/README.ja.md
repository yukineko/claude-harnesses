フックは *通知することしかできない*：`beacon notify` は正常なペイロード、壊れた／空の stdin、通知先の失敗のいずれでも exit 0 で終わり、stdout には何も出力しない（block 判定を返さない）。

配線されるのは Stop と Notification の 2 つのイベントだけ。

- **Stop**（ターン完了）ではタイトルが「✅ \<project\> — 完了」、既定の本文は Claude の最後のメッセージの先頭 160 文字（`snippet_chars`）で、それより長ければ「…」が付く。transcript が無い場合の本文は 「ターンが完了しました。」
- **Notification**（入力・許可待ち）ではタイトルが「🔔 \<project\> — 確認」、本文は Claude 自身の通知テキスト。

通知先は `beacon.toml` で有効化し、複数を同時に有効にできる:

- **desktop** — macOS は `osascript`（任意で `sound`）。
- **slack_webhook** — Slack の incoming webhook（`{"text": …}`）。 URL をコミットしないよう `BEACON_SLACK_WEBHOOK` 環境変数を優先でき、これはファイル設定を上書きする。
- **webhook** — 汎用エンドポイント。 `{event, project, title, body}` を JSON POST で受け取る（`BEACON_WEBHOOK` が上書き）。
- **command** — エスケープハッチ。 `BEACON_EVENT` / `BEACON_PROJECT` / `BEACON_TITLE` / `BEACON_BODY` を環境に入れ、シェル経由で任意のシェルコマンドを実行する。 ただし**プロジェクトローカルの** `beacon.toml` に書かれた `command` は、そのプロジェクトで `beacon trust` を実行するまで無視される（git の `safe.directory` / `HARNESS_TRUST_ALL` に相当する workspace-trust ゲート）。home／既定設定の `command` は trust 不要。

ネットワーク系の通知先は `curl --max-time 8` を呼ぶ。HTTP クライアントのクレートは直接の依存に含まれない。

同梱の `hooks/hooks.json` は Stop と Notification の両フックを `bin/beacon notify` に配線する。 通知先を選ぶには `beacon.toml` をプロジェクト（または `~/.beacon/config.toml`）に置く。 置かなければデスクトップ通知が既定で有効になる。

### cargo でスタンドアロンに導入するときは次を実行すること

```sh
cargo install --path .
beacon init          # 雛形の ./beacon.toml を書き出す（--force で上書き）
beacon test          # 設定済みチャンネルへサンプル通知を 1 発撃つ
beacon install       # Stop + Notification フックを ~/.claude/settings.json にマージ
beacon status        # 解決済み設定と各チャンネルの設定値を表示
beacon trust         # このプロジェクトを信頼し beacon.toml の `command` を有効化
beacon uninstall     # フックを取り除く
```

`beacon install` / `uninstall` は `--dry-run` を受け付け、冪等で、書き込み前に `settings.json` をバックアップし、他プラグインのフックグループは保持する。

主要なつまみは `on_stop`、`on_notification`、`include_snippet` / `snippet_chars`、各チャンネルのフィールド、`log` など（[`beacon.example.toml`](beacon.example.toml) を参照）。 `BEACON_DISABLE=1` はフック（`beacon notify`）を黙らせる。`beacon test` は送信する。
