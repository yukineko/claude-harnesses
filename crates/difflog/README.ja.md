- `SessionStart` で現在の HEAD の SHA を `~/.difflog/logs/sessions/<session_id>.json` にスナップショットする[test: crates/difflog/tests/prose_claims.rs::claim_session_start_snapshots_head]。
- `SessionEnd` は `git diff --stat -- <開始SHA>..HEAD` を実行するが、git は `--` 以降の範囲を pathspec として読むため stat が空になり、ログファイルは書き出されない[test: crates/difflog/tests/prose_claims.rs::claim_session_end_writes_log]。
- `~/.difflog/logs/<YYYY-MM-DD>-<session8>.md` にファイルは作られない[test: crates/difflog/tests/prose_claims.rs::claim_log_path_format]。

プラグインは SessionStart と SessionEnd の 2 つの hook を登録する[test: crates/difflog/tests/prose_claims.rs::claim_two_hooks_registered]。

```
/plugin install difflog@yukineko
```

```sh
cargo install --path .
difflog install   # ~/.claude/settings.json に hook をマージする [test: crates/difflog/tests/prose_claims.rs::claim_install_merges_hooks]
```

`difflog install` が SessionStart / SessionEnd の 2 つの hook を配線する[test: crates/difflog/tests/prose_claims.rs::claim_install_merges_hooks]。

```sh
difflog session-start   # SessionStart hook（stdin の JSON を読む） [test: crates/difflog/tests/prose_claims.rs::claim_session_start_snapshots_head]
difflog session-end     # SessionEnd hook（範囲が pathspec として渡るためログは書き出さない） [test: crates/difflog/tests/prose_claims.rs::claim_session_end_writes_log]
difflog last            # 直近のログを表示する [test: crates/difflog/tests/prose_claims.rs::claim_last_prints_newest]
difflog list            # ログファイルを新しい順に一覧する（--limit N、既定 10） [test: crates/difflog/tests/prose_claims.rs::claim_list_newest_first_limit]
difflog init            # 雛形の ./difflog.toml を書き出す（--force で上書き） [test: crates/difflog/tests/prose_claims.rs::claim_init_writes_starter]
difflog install         # hook を ~/.claude/settings.json にマージする（--dry-run 可） [test: crates/difflog/tests/prose_claims.rs::claim_install_merges_hooks]
difflog uninstall       # hook を取り除く（--dry-run 可） [test: crates/difflog/tests/prose_claims.rs::claim_uninstall_removes_hooks]
difflog status          # 解決済みの設定を表示する [test: crates/difflog/tests/integration.rs::status_prints_resolved_config] [test: crates/difflog/tests/prose_claims.rs::claim_config_project_wins] [test: crates/difflog/tests/prose_claims.rs::claim_config_defaults]
```

`./difflog.toml`（プロジェクト）、無ければ `~/.difflog/config.toml` を読む（先に存在した方が勝ち、両者は重ね合わせない）[test: crates/difflog/tests/prose_claims.rs::claim_config_project_wins]。
`log_dir` には SessionStart の状態 `sessions/<session_id>.json` も置かれる[test: crates/difflog/tests/prose_claims.rs::claim_log_dir_holds_session_state]。
[`difflog.example.toml`](difflog.example.toml) のキーと既定値: `enabled`（既定 `true`）、`log_dir`（既定 `~/.difflog/logs`）、`diff_body_limit`（既定 4096 バイト）、`exclude_globs`（組み込み既定 `Cargo.lock`、`package-lock.json`、`*.min.js`、`*.min.css`）[test: crates/difflog/tests/prose_claims.rs::claim_config_defaults]。
`diff_body_limit` は `difflog status` に表示されるが、ログが書き出されないため出力には影響しない[test: crates/difflog/tests/prose_claims.rs::claim_diff_body_limit_no_log_effect]。
`exclude_globs` は読み込まれ `difflog status` に表示されるが、差分処理からは参照されない[test: crates/difflog/tests/prose_claims.rs::claim_exclude_globs_effect]。
`DIFFLOG_DISABLE=1` で SessionStart のスナップショットを無効化する[test: crates/difflog/tests/prose_claims.rs::claim_disable_env]。

ナラティブは生成のみとし、ログファイルを書き換えることは禁止。

```
/difflog:difflog
```

`difflog` プラグインの `difflog` skill（`/difflog:difflog`）を実行する[test: crates/difflog/tests/prose_claims.rs::claim_skill_slash_command] [test: crates/difflog/tests/prose_claims.rs::claim_skill_invocation_namespace]。特定のセッションを対象にする場合は `/difflog:difflog --session <id>` を渡す[test: crates/difflog/tests/prose_claims.rs::claim_skill_session_arg] [test: crates/difflog/tests/prose_claims.rs::claim_skill_invocation_namespace]。
