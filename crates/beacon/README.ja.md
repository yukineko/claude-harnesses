# beacon

Claude Code 向けのデスクトップ／Webhook 通知。ターンが終わったとき、あるいは Claude が入力を待っているときに知らせる **Stop** と **Notification** の 2 つのフック。

## 目的

beacon は、Claude Code のセッション中に「いま画面に戻るべき瞬間」を通知する小さなフックである。配線されるのは 2 つのイベントだけ:

| Claude Code のイベント | beacon の通知 | 既定の本文 |
|---|---|---|
| **Stop**（ターン完了） | 「✅ \<project\> — 完了」 | Claude の最後のメッセージ末尾 |
| **Notification**（入力・許可待ち） | 「🔔 \<project\> — 確認」 | Claude 自身の通知テキスト |

通知先は `beacon.toml` で任意に組み合わせて有効化する:

- **desktop** — macOS は `osascript`（任意で `sound`）、Linux は `notify-send`。
- **slack_webhook** — Slack の incoming webhook（`{"text": …}`）。URL をコミットしないよう `BEACON_SLACK_WEBHOOK` 環境変数を優先でき、これはファイル設定を上書きする。
- **webhook** — 汎用エンドポイント。`{event, project, title, body}` を JSON POST で受け取る（`BEACON_WEBHOOK` が上書き）。
- **command** — エスケープハッチ。`BEACON_EVENT` / `BEACON_PROJECT` / `BEACON_TITLE` / `BEACON_BODY` を環境に入れ、`sh -c` で任意のシェルコマンドを実行する。ただし**プロジェクトローカルの** `beacon.toml` に書かれた `command` は、そのプロジェクトで `beacon trust` を実行するまで無視される（git の `safe.directory` / `HARNESS_TRUST_ALL` に相当する workspace-trust ゲート）。home／既定設定の `command` は trust 不要。

ネットワーク系の通知先は `curl --max-time 8` を呼ぶだけで、HTTP スタックはバイナリにリンクしていない。

beacon は **subscription-native** であり、バンドルされた単一の Rust バイナリだけで動く。**API キーは不要**で、常駐デーモンもない。フックは *通知することしかできない*——ターンをブロックせず常に exit 0 で終わるため、`curl` が無い・通知が拒否された・stdin が空、といった状況でも何のコストもかからない。

## どうして必要か

長いセッションでは、Claude が作業を続けている間に席を離れたくなる。だが離れると、ターンが終わったこと、あるいは許可・入力を求められて処理が止まっていることに気づけず、無駄に待ち時間が生まれる。逆に張り付いて待ち続けるのも非効率だ。

beacon はこの「いつ戻ればいいか分からない」問題を、Stop と Notification の 2 イベントへの通知で解く。Devin の Slack 通知に着想を得つつ、外部サービスへの依存を持たない極小のローカルフックとして作り直したものである。フックは決して処理を妨げない設計なので、通知が失敗してもセッションには一切影響しない——「あれば便利、無くても壊れない」という安全側に倒してある。

## どう使うか

### プラグインとして

プラグインマーケットプレイス経由でインストールすると、同梱の `hooks/hooks.json` が Stop と Notification の両フックを自動で配線する。追加の作業は不要。通知先を選ぶには `beacon.toml` をプロジェクト（または `~/.beacon/config.toml`）に置く。置かなければデスクトップ通知が既定で有効になる。

### スタンドアロン（cargo）

```sh
cargo install --path .
beacon init          # 雛形の ./beacon.toml を書き出す（--force で上書き）
beacon test          # 設定済みチャンネルへサンプル通知を 1 発撃つ
beacon install       # Stop + Notification フックを ~/.claude/settings.json にマージ
beacon status        # 解決済み設定と有効チャンネルを表示
beacon trust         # このプロジェクトを信頼し beacon.toml の `command` を有効化
beacon uninstall     # フックを取り除く
```

`beacon install` / `uninstall` は `--dry-run` を受け付け、冪等で、書き込み前に `settings.json` をバックアップし、他プラグインのフックグループは保持する。

### 設定

主要なつまみは `on_stop`、`on_notification`、`include_snippet` / `snippet_chars`、各チャンネルのフィールド、`log` など（[`beacon.example.toml`](beacon.example.toml) を参照）。`BEACON_DISABLE=1` ですべての通知を黙らせる。
