> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# beacon 仕様

## 概要

`beacon` は Claude Code のセッション中に「いま画面に戻るべき瞬間」を通知する極小フックである。
配線されるイベントは 2 つだけで、どちらも同じ `notify` サブコマンド（`Command::Notify` → `notify_hook`）へ
入る: **Stop**（ターン完了）は「✅ \<project\> — 完了」、**Notification**（入力・許可待ち）は
「🔔 \<project\> — 確認」を出す（`build_note`）。通知先（channel）は `beacon.toml`（`config::Config`）で
任意に組み合わせて有効化する: `desktop`（macOS `osascript` / Linux `notify-send`）、`slack_webhook`、
汎用 `webhook`、エスケープハッチ `command`。ネットワーク系は `curl --max-time 8` を shell out するだけで
HTTP スタックをバイナリにリンクしない（`notify::curl_post_json`）。**subscription-native** で API キー・
常駐デーモン不要。フックは *通知することしかできない* — ターンをブロックせず常に exit 0 で終わる。

## 不変条件

- **フックは非ブロッキング・fail-soft** — `notify_hook` は `harness_core::hook::run_hook` 経由で走り、
  config 欠落・stdin 空・channel 失敗いずれでも何も起こさず正常終了する。全 channel 送出は best-effort で
  副作用のみ: `notify::run_quiet` がコマンド失敗（`curl`/`osascript`/`notify-send` 不在、network down 等）を
  握りつぶし `false` を返す。`transcript::last_assistant_text` は parse/read エラーで `None` を返し、
  呼び出し側は汎用文言へフォールバックする。
- **workspace-trust ゲート（command のトラスト境界）** — **プロジェクトローカルの** `beacon.toml` に書かれた
  `command` は、`harness_core::trust::is_trusted(root)` が真（`beacon trust` で登録済み or `HARNESS_TRUST_ALL`）で
  ない限り `Config::load` で drop される（stderr に警告）。home／既定設定（`~/.beacon/config.toml`）由来の
  `command` と、組み込み channel（desktop/slack/webhook）は trust 不要で無影響。git の `safe.directory` /
  VS Code Workspace Trust に相当。
- **command の値は shell で再解釈されない** — `command` 文字列自体はユーザ authored で trusted だが、通知の
  各フィールドは `.env("BEACON_EVENT"/…)` で環境変数として渡され、値（`$BEACON_BODY` 等）が shell に
  再パースされることはない（`notify::run_command`）。
- **AppleScript インジェクション防御** — macOS desktop 通知の title/body は `applescript_escape` で
  バックスラッシュ・二重引用符・制御文字（`\n`/`\r`/`\t`）をエスケープし、細工した本文が `"..."` リテラルを
  閉じて後続 AppleScript を注入できないようにする。`$`・backtick は osascript にとって無害なので素通し
  （script は `osascript -e` の単一 argv で shell を経ない）。
- **秘匿値は env 優先** — `BEACON_SLACK_WEBHOOK` / `BEACON_WEBHOOK` はファイル設定を上書きするので URL を
  コミットせずに済む（`Config::load`）。空文字列は「未設定」扱い（`non_empty`/`env_non_empty`）。
- **config はマージせず最初に存在した層が勝つ** — precedence は project `beacon.toml` > home
  `~/.beacon/config.toml` > 組み込み既定（`Config::default`）。層は合成しない。
- **CJK 安全** — snippet 切り詰めは byte でなく char 単位（`transcript::truncate`、末尾 `…`）。
  `snippet_chars` は `clamp(20, 1000)` でサニタイズ。
- **キル・スイッチ** — `BEACON_DISABLE=1`（非空かつ `!= "0"`）で全通知を黙らせる（`Config::disabled_env`、
  `notify_hook` 冒頭で早期 return）。

## 振る舞い

サブコマンドは `clap` の `Command` enum で定義（`main`）。

- **`notify`** — Stop / Notification フック本体（`run_hook(notify_hook)`）。`disabled_env` チェック → stdin を
  `read_stdin` → `HookInput::parse`（失敗なら return）→ `cwd_or_current` で root 解決 → `Config::load` →
  `cfg.enabled && cfg.any_channel()` でなければ return → `build_note`（当該イベントが muted なら `None`）→
  `notify::dispatch` で全 channel へ fan-out → `log_event`。Stop の本文は `include_snippet` 有効時に
  Claude 最終メッセージ末尾、無ければ「ターンが完了しました。」。Notification の本文は `input.message`
  （空なら「入力待ちです。」）。
- **`test`** — 設定済み channel へサンプル通知（「🔔 \<project\> — beacon test」）を 1 発撃つセットアップ確認。
  channel 未設定なら案内文、送出ゼロなら curl/osascript/notify-send の有無を促す。
- **`install [--dry-run]`** — Stop + Notification フックを `~/.claude/settings.json` へマージ
  （`install::install`、`harness_core::install` の `push_group`/`command_group`）。冪等・書き込み前バックアップ・
  他プラグインの hook group 保持。standalone（`cargo install`）用途。plugin 経由では代わりに `hooks/hooks.json`
  が配線する。
- **`uninstall [--dry-run]`** — `MARKERS = ["beacon"]` に一致する hook group を除去（`remove_hooks_from_settings`）。
- **`init [--force]`** — 雛形 `./beacon.toml`（`STARTER`）を書き出す。既存時は `--force` 無しで bail。
- **`status`** — 解決済み config（source path・各つまみ・channel の set/unset マスク・state_dir）を表示。
- **`trust`** — カレントプロジェクト root を `harness_core::trust::add` で信頼登録し、project-local
  `beacon.toml` の `command` を有効化する。

チャンネル送出（`notify::dispatch`、実際に試みた channel 名の `Vec` を返す）:

- **desktop** — macOS は `osascript -e "display notification …"`（任意 `sound`）、Linux は `notify-send`、
  他 OS は `false`。
- **slack** — `{"text": "<title>\n<body>"}` を `curl_post_json`。
- **webhook** — `{event, project, title, body}` を JSON POST。
- **command** — `harness_core::shell::command(cmd)` に `BEACON_*` env を載せて実行。

`log = true` の時 `<state_dir>/log.jsonl` へ配信 1 件 1 行（`ts`/`event`/`project`/`title`/`channels`）を追記
（`log_event`）。

### module 責務

- **`main`** — CLI 定義（`Cli`/`Command`）、`notify_hook`、`build_note`（イベント→`Note` or muted 時 `None`）、
  `test`/`status`/`init`/`trust_project`、`log_event`、`STARTER` テンプレート。
- **`config`** — `Config`（実行時解決済み）と `FileConfig`（TOML deserialize）。`load`（precedence 解決 +
  trust ゲート + env override + sanitize）、`disabled_env`、`any_channel`、`project_path`/`home_path`/`base_dir`。
  workspace-trust ゲートを担う。
- **`notify`** — channel dispatch。`Note`、`dispatch`、`desktop`/`slack`/`webhook`/`run_command`、
  `curl_post_json`、`run_quiet`、`applescript_escape`（AppleScript インジェクション防御）。
- **`transcript`** — JSONL transcript から Claude 最終 assistant テキストを best-effort 抽出。
  `last_assistant_text`（tool-only ターンはスキップし遡る）、`flatten`（空白畳み）、`truncate`（CJK 安全）。
- **`install`** — `~/.claude/settings.json` への Stop/Notification フックの冪等マージ／除去。
  `install`/`uninstall`、`MARKERS`/`EVENTS`/`TIMEOUT_SECS`。`harness_core::install` に委譲。
- **`model`** — 正典 `harness_core::hook::HookInput`（`message`/`stop_hook_active`/`project_name()`）の re-export のみ。
