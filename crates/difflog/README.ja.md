- `SessionStart` で現在の HEAD の SHA を `~/.difflog/logs/sessions/<session_id>.json` にスナップショットする。
- `SessionEnd` は `git diff --stat -- <開始SHA>..HEAD` を実行するが、git は `--` 以降の範囲を pathspec として読むため stat が空になり、ログファイルは書き出されない。
- `~/.difflog/logs/<YYYY-MM-DD>-<session8>.md` にファイルは作られない。

プラグインは SessionStart と SessionEnd の 2 つの hook を登録する。

```
/plugin install difflog@yukineko
```

```sh
cargo install --path .
difflog install   # ~/.claude/settings.json に hook をマージする
```

`difflog install` が SessionStart / SessionEnd の 2 つの hook を配線する。

```sh
difflog session-start   # SessionStart hook（stdin の JSON を読む）
difflog session-end     # SessionEnd hook（範囲が pathspec として渡るためログは書き出さない）
difflog last            # 直近のログを表示する
difflog list            # ログファイルを新しい順に一覧する（--limit N、既定 10）
difflog init            # 雛形の ./difflog.toml を書き出す（--force で上書き）
difflog install         # hook を ~/.claude/settings.json にマージする（--dry-run 可）
difflog uninstall       # hook を取り除く（--dry-run 可）
difflog status          # 解決済みの設定を表示する
```

`./difflog.toml`（プロジェクト）、無ければ `~/.difflog/config.toml` を読む（先に存在した方が勝ち、両者は重ね合わせない）。
`log_dir` には SessionStart の状態 `sessions/<session_id>.json` も置かれる。
[`difflog.example.toml`](difflog.example.toml) のキーと既定値: `enabled`（既定 `true`）、`log_dir`（既定 `~/.difflog/logs`）、`diff_body_limit`（既定 4096 バイト）、`exclude_globs`（組み込み既定 `Cargo.lock`、`package-lock.json`、`*.min.js`、`*.min.css`）。
`diff_body_limit` は `difflog status` に表示されるが、ログが書き出されないため出力には影響しない。
`exclude_globs` は読み込まれ `difflog status` に表示されるが、差分処理からは参照されない。
`DIFFLOG_DISABLE=1` で SessionStart のスナップショットを無効化する。

ナラティブは生成のみとし、ログファイルを書き換えることは禁止。

```
/difflog:difflog
```

`difflog` プラグインの `difflog` skill（`/difflog:difflog`）を実行する。特定のセッションを対象にする場合は `/difflog:difflog --session <id>` を渡す。
