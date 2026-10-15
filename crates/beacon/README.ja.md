フックは *通知することしかできない*：`beacon notify` は正常なペイロード、壊れた／空の stdin、通知先の失敗のいずれでも exit 0 で終わり、stdout には何も出力しない（block 判定を返さない）[test: crates/beacon/tests/integration.rs::notify_with_malformed_stdin_exits_zero] [test: crates/beacon/tests/prose_claims.rs::claim_exit_zero_never_blocks]。

配線されるのは Stop と Notification の 2 つのイベントだけ[test: crates/beacon/tests/prose_claims.rs::claim_two_events_only]。

- **Stop**（ターン完了）ではタイトルが「✅ \<project\> — 完了」、既定の本文は Claude の最後のメッセージの先頭 160 文字（`snippet_chars`）で、それより長ければ「…」が付く[test: crates/beacon/tests/prose_claims.rs::claim_stop_title_body]。transcript が無い場合の本文は [test: crates/beacon/tests/prose_claims.rs::claim_stop_title_body] 「ターンが完了しました。」
- **Notification**（入力・許可待ち）ではタイトルが「🔔 \<project\> — 確認」、本文は Claude 自身の通知テキスト[test: crates/beacon/tests/prose_claims.rs::claim_notification_title_body]。

通知先は `beacon.toml` で有効化し、複数を同時に有効にできる [test: crates/beacon/tests/prose_claims.rs::claim_any_combination]:

- **desktop** — macOS は `osascript`（任意で `sound`）[test: crates/beacon/tests/prose_claims.rs::claim_desktop_channel]。
- **slack_webhook** — Slack の incoming webhook（`{"text": …}`）[test: crates/beacon/tests/prose_claims.rs::claim_slack_payload_env_override]。 URL をコミットしないよう `BEACON_SLACK_WEBHOOK` 環境変数を優先でき、これはファイル設定を上書きする[test: crates/beacon/tests/prose_claims.rs::claim_slack_payload_env_override]。
- **webhook** — 汎用エンドポイント[test: crates/beacon/tests/prose_claims.rs::claim_webhook_payload_env_override]。 `{event, project, title, body}` を JSON POST で受け取る（`BEACON_WEBHOOK` が上書き）[test: crates/beacon/tests/prose_claims.rs::claim_webhook_payload_env_override]。
- **command** — エスケープハッチ[test: crates/beacon/tests/prose_claims.rs::claim_command_env_vars]。 `BEACON_EVENT` / `BEACON_PROJECT` / `BEACON_TITLE` / `BEACON_BODY` を環境に入れ、シェル経由で任意のシェルコマンドを実行する[test: crates/beacon/tests/prose_claims.rs::claim_command_env_vars]。 ただし**プロジェクトローカルの** `beacon.toml` に書かれた `command` は、そのプロジェクトで `beacon trust` を実行するまで無視される（git の `safe.directory` / `HARNESS_TRUST_ALL` に相当する workspace-trust ゲート） [test: crates/beacon/src/config.rs::project_command_is_gated_behind_workspace_trust] [test: crates/beacon/tests/prose_claims.rs::claim_trust_cli]。home／既定設定の `command` は trust 不要 [test: crates/beacon/src/config.rs::project_command_is_gated_behind_workspace_trust]。

ネットワーク系の通知先は `curl --max-time 8` を呼ぶ[test: crates/beacon/tests/prose_claims.rs::claim_curl_max_time_no_http_stack]。HTTP クライアントのクレートは直接の依存に含まれない[test: crates/beacon/tests/prose_claims.rs::claim_curl_max_time_no_http_stack]。

同梱の `hooks/hooks.json` は Stop と Notification の両フックを `bin/beacon notify` に配線する[test: crates/beacon/tests/prose_claims.rs::claim_two_events_only]。 通知先を選ぶには `beacon.toml` をプロジェクト（または `~/.beacon/config.toml`）に置く[test: crates/beacon/tests/prose_claims.rs::claim_config_locations_default_desktop]。 置かなければデスクトップ通知が既定で有効になる[test: crates/beacon/tests/prose_claims.rs::claim_config_locations_default_desktop]。

### cargo でスタンドアロンに導入するときは次を実行すること

```sh
cargo install --path .
beacon init          # 雛形の ./beacon.toml を書き出す（--force で上書き） [test: crates/beacon/tests/prose_claims.rs::claim_init_force]
beacon test          # 設定済みチャンネルへサンプル通知を 1 発撃つ [test: crates/beacon/tests/prose_claims.rs::claim_test_fires_sample]
beacon install       # Stop + Notification フックを ~/.claude/settings.json にマージ [test: crates/beacon/tests/prose_claims.rs::claim_install_merges_hooks]
beacon status        # 解決済み設定と各チャンネルの設定値を表示 [test: crates/beacon/tests/prose_claims.rs::claim_status_active_channels]
beacon trust         # このプロジェクトを信頼し beacon.toml の `command` を有効化 [test: crates/beacon/tests/prose_claims.rs::claim_trust_cli]
beacon uninstall     # フックを取り除く [test: crates/beacon/tests/prose_claims.rs::claim_uninstall_removes]
```

`beacon install` / `uninstall` は `--dry-run` を受け付け、冪等で、書き込み前に `settings.json` をバックアップし、他プラグインのフックグループは保持する[test: crates/beacon/tests/prose_claims.rs::claim_install_dryrun_idempotent_backup_foreign]。

主要なつまみは `on_stop`、`on_notification`、`include_snippet` / `snippet_chars`、各チャンネルのフィールド、`log` など（[`beacon.example.toml`](beacon.example.toml) を参照）[test: crates/beacon/tests/prose_claims.rs::claim_example_toml_knobs]。 `BEACON_DISABLE=1` はフック（`beacon notify`）を黙らせる[test: crates/beacon/tests/prose_claims.rs::claim_disable_env]。`beacon test` は送信する[test: crates/beacon/tests/prose_claims.rs::claim_disable_env]。
