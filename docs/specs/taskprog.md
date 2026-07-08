> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# taskprog 仕様

## 概要

`taskprog` は、プロジェクトに単一の進捗ファイル `.claude/progress.md` を持たせ、それをセッション
境界をまたいで最新に保つための subscription-native なハーネスである（`ANTHROPIC_API_KEY` 不要）。役割
は 2 つのライフサイクル hook に集約される。**SessionStart** hook（`session_start_hook`）で進捗ファイル
を `additionalContext` として注入し、新セッション開始時点で「完了・残・ブロッカー」を即座に把握できる
ようにする。**Stop** hook（`update::on_stop`）でセッション終了前に、モデルへ進捗ファイル更新を促す
`additionalContext` を返し、あわせて進捗ファイルが存在しない／空のときは決定論的な skeleton を自前で
seed してファイルが決して stale/欠落のまま残らないようにする。判断（何を Completed/Pending/Blockers に
書くか）はモデルに委ね、バイナリはその周囲の決定論的な read/write/skeleton-seed に徹する。手動更新
コマンドは `/taskprog` スキル（`skills/taskprog/SKILL.md`）から駆動され、`taskprog show` → 3 セクション
（Completed/Pending/Blockers）を Write ツールで書き直す HOTL ワークフローを踏む。

## 不変条件

- **hook はターンを壊さない（fail-soft）** — `SessionStart`/`Stop` は `harness_core::hook::run_hook`
  下で走り、常に exit 0。stdin が壊れていても `HookInput::parse(&raw).unwrap_or_default()` で既定入力に
  落ち、注入しない／skeleton seed のみで通過する（`tests/integration.rs` の `session_start_malformed_stdin_exits_zero`
  / `stop_malformed_stdin_exits_zero`）。設定ロード（`Config::load`）も TOML パース失敗時は
  `Config::default()` に落ち、read/write の IO エラーは `update::on_stop` 内で `let _ =` により黙って握り潰す。
- **無効化の 2 経路** — 環境変数 `TASKPROG_DISABLED=1`（または `true`）で `config::disabled_env()` が真に
  なると両 hook は即 `Ok(None)`/`Ok(())` で何もしない。加えて `taskprog.toml` の `enabled = false` でも
  両 hook が no-op になる。
- **既存の進捗ファイルを上書きしない** — `on_stop` は「real content を持つ非空ファイル」だけを present と
  みなす（`progress::read_file(&path, 0).is_some()`）。既存の model-authored な内容があれば skeleton を
  書かず、その内容を `additionalContext` に埋め込んで update を促すだけ（`on_stop_preserves_existing_content`
  テスト）。skeleton seed は「ファイル欠落 or 空/空白のみ」のときに限る（`on_stop_writes_skeleton_when_missing`）。
- **skeleton は決定論的** — `build_skeleton` は外部依存もタイムスタンプも使わず、`## Done` /
  `## In progress / remaining` / `## Blockers` の標準 3 セクションを固定文言で書き、session breadcrumb
  （`input.project_name()`, `input.session_id`／空なら `(unknown session)`）だけを埋める。出力は入力から
  一意に定まる。
- **注入は境界制御される** — `SessionStart` の注入量は `inject_limit`（既定 4096 バイト、0 = 無制限）で
  上限を持つ。`read_file` は上限超過時に **改行境界**で truncate し、末尾に
  `*(truncated — see full file for details)*` を付す（バイト途中で切らない）。
- **設定の precedence は固定** — `Config::load` は project-local `<cwd>/taskprog.toml` > home
  `~/.taskprog/config.toml` > `Config::default()` の順で最初に成立したものを採る。
- **進捗ファイルパスの解決** — `resolve_progress_path` は `progress_file` 指定時に
  `harness_core::config::expand_tilde` で `~` 展開、未指定なら `<cwd>/.claude/progress.md`。書き込み
  （`write_progress`）は親ディレクトリを `create_dir_all` で作ってから書く。

## 振る舞い

サブコマンドは `clap` の `Command` enum（`main.rs`）で定義。

- **`session-start`（SessionStart hook）** — stdin の HookInput JSON を読み、無効化/`!enabled` でなければ
  `progress::build_context(path, inject_limit)` で `"## Progress file ({path})\n\n{content}"` を組み立て、
  `{ "additionalContext": ctx }` を stdout へ出す。ファイル欠落・空なら何も出さない。
- **`stop`（Stop hook）** — `update::on_stop` を呼ぶ。非空ファイルが無ければ `build_skeleton` を seed し、
  「Before ending this session, please update the progress file … Write it with the Write tool.」という
  `additionalContext`（既存内容 or skeleton 案内を付随）を stdout へ出す。
- **`show`** — 現在の進捗ファイルを `read_file(path, 0)`（無制限）で全文表示。欠落時は stderr に
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
  保証、`read_stdin` + `HookInput::parse` で入力を取り、`session_start_hook`/`stop_hook`（`disabled_env`
  と `enabled` を先に判定するガード）へ委譲。
- **`config`** — `Config`（`enabled`/`progress_file`/`inject_limit`, `serde(default)`）の TOML
  deserialize、precedence 付き `load`、`resolve_progress_path`、`disabled_env`（`TASKPROG_DISABLED`）、
  `init_config`（example toml の write-out）。
- **`progress`** — 進捗ファイルの純粋な読み取り層。`read_file`（limit バイト境界＋改行境界 truncate、空は
  None）、`build_context`（SessionStart 注入文字列の組み立て）。
- **`update`** — Stop hook の中核。`on_stop`（skeleton seed 判定＋update 促し）、`build_skeleton`
  （決定論的 3 セクション skeleton）、`write_progress`（親作成つき write。`taskprog write` も使用）。
- **`install`** — `~/.claude/settings.json` への hook group のマージ／除去（`harness_core::install` に委譲、
  marker `"taskprog"`）。

## 補足（stub / 実装の限界）

- SessionStart/Stop 以外の「進捗の自動更新」は行わない。Completed/Pending/Blockers の内容決定は
  完全にモデル（`/taskprog` スキル or Stop の promptに応じたモデルの Write）に委ねられ、バイナリは
  内容を検証・強制しない。skeleton は breadcrumb を除き常にプレースホルダ（`(nothing recorded yet)` /
  `(none recorded)`）で、モデルが flesh out しなければそのまま残る。
- README のサンプル進捗ファイルは `## Completed/Pending/Blockers/Notes`、`/taskprog` スキルも同 4 セクション
  だが、Stop hook の `build_skeleton` が seed するのは `## Done` / `## In progress / remaining` /
  `## Blockers`（Notes 無し）で、セクション名が両者で一致していない（seed 経路と手動更新経路で見出しが
  ずれる）。
