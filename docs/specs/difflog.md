> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# difflog 仕様

## 概要

difflog は Claude Code のセッション単位で git 差分ログを記録するプラグインバイナリ。`SessionStart` フックで HEAD の SHA をスナップショットし、`SessionEnd` フックで `<start>..HEAD` の構造化差分サマリ（コミット一覧・stat・A/M/D ファイル・上限つき diff 本文）を markdown として `log_dir` に書き出す。書かれたログは `/difflog` skill が LLM に読ませて開発者向けナラティブを生成するための素材になる。

## 不変条件

- **決定論**: 出力 markdown は git コマンド出力と `SessionState` のみに依存する（`render_log`）。タイムスタンプ (`started_at`/`ended_at`) は UTC・`%Y-%m-%dT%H:%M:%SZ` 固定書式。
- **副作用境界**: 書き込みは `log_dir` 配下のみ（`state::save` は `<log_dir>/sessions/<id>.json`、`write_log` は `<log_dir>/<date>-<id8>.md`）。git は read-only なサブコマンド（`rev-parse`/`diff`/`log`）のみ実行。
- **fail-soft**: フックはエラーを握り潰す。stdin 未 parse・`session_id` 空・state 不在・git 失敗のいずれでも早期 return し、`session_end` の書き込み結果は `let _ =` で無視する（`session_start`/`session_end`/`on_session_end`）。git ヘルパは失敗時に空文字/空 Vec を返す（`diff_stat`/`diff_name_status`/`diff_body`/`log_oneline`）。
- **無変更時は無出力**: `diff_stat` が空（trim 後）ならログを書かない（`on_session_end`）。
- **引数インジェクション防御**: `base` が `-` 始まりなら git 引数として渡さず空を返す（`validate_base` を全 git ヘルパが先頭で呼ぶ）。
- **パス安全化**: `session_id` はファイル名化時に非英数字を `_` へ置換（state は全長、ログ名は先頭 8 文字。`session_path`/`write_log`）。
- **無効化**: 環境変数 `DIFFLOG_DISABLE`（`Config::disabled_env`）または config の `enabled=false` でフックを不活性化。

## 振る舞い

サブコマンド（`Command`）:

- `SessionStart` / `SessionEnd`: フック本体。`run_hook` 経由で実行。前者は `read_stdin`、後者は `read_stdin_if_piped` で `HookInput` を読む。
  - SessionStart (`on_session_start`): `git rev-parse HEAD` を取り、`SessionState { session_id, start_sha, project, started_at }` を保存。`project` は cwd の basename。
  - SessionEnd (`on_session_end`): state を読み、`diff_stat`/`diff_name_status`/`diff_body`/`log_oneline` と現 HEAD を集めて `render_log` で markdown 化し、`<date>-<id8>.md` に書く。diff 本文は `diff_body_limit` バイトで打ち切り（`0` で本文なし、超過時は `… (truncated at N bytes)` を付す）。
- `List { limit }`: `log_dir` 内の `.md` をファイル名降順（新しい順）に `limit` 件（既定 10）表示。
- `Last`: 最新の `.md` の内容を stdout に出力。
- `Install { dry_run }` / `Uninstall { dry_run }`: `~/.claude/settings.json` の両フックを追加/削除（`install` モジュール）。
- `Init { force }`: `./difflog.toml` を雛形から生成（既存時は `--force` 必須）。
- `Status`: 解決済み config（`enabled`/`log_dir`/`diff_body_limit`/`exclude_globs`）を表示。

Config（`Config::load`）: プロジェクト `./difflog.toml` → 無ければ `~/.difflog/config.toml` の順で 1 ファイルを既定値へ上書き（`enabled=true`、`log_dir=<base>/logs`、`diff_body_limit=4096`、`exclude_globs` に lock/min 系）。
