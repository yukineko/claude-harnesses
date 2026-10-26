> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# taskprog 仕様

## 概要

`taskprog` は、プロジェクトに単一の進捗ファイル `.claude/progress.md` を持たせるための
subscription-native なハーネスである（`ANTHROPIC_API_KEY` 不要）。2 つのライフサイクル hook
（SessionStart / Stop）を登録するが、**どちらの hook も何も出力しない**（stdout・stderr とも空、exit 0。
ユーザー裁定「progressはいらない」「何も表示しないようにして」）。**SessionStart** hook は何もしない
（進捗ファイルを注入しない）。**Stop** hook（`update::on_stop`）は、進捗ファイルが存在しない／空のときだけ
決定論的な skeleton を自前で seed してファイルが欠落のまま残らないようにする。ユーザーにもモデルにも
メッセージは出さない（以前の Stop `systemMessage` 通知と SessionStart 注入は撤去した）。判断（何を
Completed/Pending/Blockers に書くか）はモデルに委ね、バイナリはその周囲の決定論的な read/write/skeleton-seed に
徹する。手動更新コマンドは `/taskprog` スキル（`skills/taskprog/SKILL.md`）から駆動され、`taskprog show` →
3 セクション（Completed/Pending/Blockers）を Write ツールで書き直す HOTL ワークフローを踏む。

## 不変条件

- **hook は何も出力しない** — `SessionStart`/`Stop` は `harness_core::hook::run_hook` 下で走り、常に exit 0、
  stdout・stderr とも空（`tests/silent_hooks.rs`）。これらの hook は判定を持たない（出力が無いので下流消費者が
  存在しない）。stdin が壊れていても `HookInput::parse(&raw).unwrap_or_default()` で既定入力に落ち、
  skeleton seed のみで通過する（`tests/integration.rs` の `session_start_malformed_stdin_exits_zero`
  / `stop_malformed_stdin_exits_zero`）。設定ロード（`Config::load`）は TOML パース失敗時に
  `Config::default()` に落ちる。skeleton の書き込み失敗は `on_stop` が `Err` として返し（`Seeded` とは
  報告しない）、hook 側はそれを出力せずに捨てる。
- **無効化の 2 経路** — 環境変数 `TASKPROG_DISABLED=1`（または `true`）で `config::disabled_env()` が真に
  なると Stop hook は即 `Ok(())` で何もしない（skeleton も書かない）。加えて `taskprog.toml` の
  `enabled = false` でも Stop hook が no-op になる。SessionStart hook はもともと何もしない。
- **既存の進捗ファイルを上書きしない** — `on_stop` は「real content を持つ非空ファイル」だけを present と
  みなす（`progress::read_file(&path).is_some()`）。既存の model-authored な内容があれば skeleton を
  書かず、何もしない（`StopOutcome::Existing`。`on_stop_preserves_existing_content` テスト）。skeleton seed は
  「ファイル欠落 or 空/空白のみ」のときに限る（`on_stop_writes_skeleton_when_missing`）。
- **ルート不明なら何も書かない** — `resolve_progress_path` がプロジェクトルートを決定できないとき、`on_stop` は
  cwd にフォールバックせず何も書かず（何も出力もせず）、`StopOutcome::NotAnchored(reason)` を返す。
- **skeleton は決定論的** — `build_skeleton` は外部依存もタイムスタンプも使わず、`## Done` /
  `## In progress / remaining` / `## Blockers` の標準 3 セクションを固定文言で書き、breadcrumb
  （プロジェクトルートのディレクトリ名、`input.session_id`／空なら `(unknown session)`）だけを埋める。出力は
  入力から一意に定まる。
- **`inject_limit` は効果を持たない** — 設定キーは後方互換のため parse され（既定 4096）、`status` が表示するが、
  何も注入しないので挙動には影響しない。
- **設定の precedence は固定** — `Config::load` は project-local `<cwd>/taskprog.toml` > home
  `~/.taskprog/config.toml` > `Config::default()` の順で最初に成立したものを採る。
- **進捗ファイルパスの解決** — `resolve_progress_path` は `progress_file` 指定時に
  `harness_core::config::expand_tilde` で `~` 展開してそのまま使い、未指定なら
  `<project root>/.claude/progress.md`（`.git` を持つ最寄りの祖先）。書き込み（`write_progress`）は
  親ディレクトリを `create_dir_all` で作ってから書く。

## 振る舞い

サブコマンドは `clap` の `Command` enum（`main.rs`）で定義。

- **`session-start`（SessionStart hook）** — stdin を読み捨てるだけで、何も出力しない。
- **`stop`（Stop hook）** — `update::on_stop` を呼ぶ。非空ファイルが無ければ `build_skeleton` を seed する。
  どの結果（`Seeded` / `Existing` / `NotAnchored` / `Err`）でも何も出力しない。
- **`show`** — 現在の進捗ファイルを `read_file(path)` で全文表示。欠落時は stderr に
  `No progress file at …`。
- **`write --cwd <dir>`** — stdin から全内容を読み `write_progress` で進捗ファイルへ書き込む（`/taskprog`
  スキルや外部からの決定論的書き込み経路）。
- **`init [target]`** — `taskprog.toml`（既定名）に `taskprog.example.toml` の内容を書き出す。既存なら
  bail。
- **`install [--dry-run]` / `uninstall [--dry-run]`** — `~/.claude/settings.json` に `SessionStart`
  （`{bin} session-start`, timeout 5）と `Stop`（`{bin} stop`, timeout 10）の hook group を marker
  `"taskprog"` でマージ／除去する（`harness_core::install`）。プラグイン導入時は `hooks/hooks.json` が
  `${CLAUDE_PLUGIN_ROOT}/bin/taskprog …` で同等の配線を担う。
- **`status`** — 解決済み設定（`enabled` / `progress_file` / `inject_limit` / `file_exists`）を表示する
  read-only コマンド。

## module 責務

- **`main`** — clap CLI 定義とディスパッチ。hook 系（`session-start`/`stop`）を `run_hook` で包み exit 0 を
  保証。`session-start` は stdin を読み捨てるだけ。`stop` は `read_stdin` + `HookInput::parse` で入力を取り、
  `stop_hook`（`disabled_env` と `enabled` を先に判定するガード）へ委譲。
- **`config`** — `Config`（`enabled`/`progress_file`/`inject_limit`, `serde(default)`）の TOML
  deserialize、precedence 付き `load`、`resolve_progress_path`、`disabled_env`（`TASKPROG_DISABLED`）、
  `init_config`（example toml の write-out）。
- **`progress`** — 進捗ファイルの純粋な読み取り層。`read_file`（全文。欠落・空は None）。
- **`update`** — Stop hook の中核。`on_stop`（skeleton seed 判定。出力なし、`StopOutcome` を返す）、
  `build_skeleton`（決定論的 3 セクション skeleton）、`write_progress`（親作成つき write。`taskprog write` も使用）。
- **`install`** — `~/.claude/settings.json` への hook group のマージ／除去（`harness_core::install` に委譲、
  marker `"taskprog"`）。

## 補足（stub / 実装の限界）

- Stop の skeleton seed 以外の「進捗の自動更新」は行わない。Completed/Pending/Blockers の内容決定は
  完全にモデル（`/taskprog` スキルによるモデルの Write）に委ねられ、hook はモデルにもユーザーにも更新を促さない。
  ファイルの存在は決定論的 skeleton のみが保証し、バイナリは内容を検証・強制しない。skeleton は breadcrumb を
  除き常にプレースホルダ（`(nothing recorded yet)` / `(none recorded)`）で、モデルが flesh out しなければ
  そのまま残る。新セッションへ進捗は自動では渡らない（`taskprog show` か `/taskprog` で明示的に読む）。
- README のサンプル進捗ファイルは `## Completed/Pending/Blockers/Notes`、`/taskprog` スキルも同 4 セクション
  だが、Stop hook の `build_skeleton` が seed するのは `## Done` / `## In progress / remaining` /
  `## Blockers`（Notes 無し）で、セクション名が両者で一致していない（seed 経路と手動更新経路で見出しが
  ずれる）。
