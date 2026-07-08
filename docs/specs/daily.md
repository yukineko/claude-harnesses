> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# daily 仕様

## 概要

`daily` は、Claude Code の `SessionStart` イベントに配線された決定論的な Rust バイナリ
（LLM を呼ばない）で、登録済みタスクを **暦日（ローカル時刻）あたり最大 1 回** 実行する
「1 日 1 回タスクランナー」である。時刻ベース（cron）の発火はマシン/シェル依存で脆いため、
`daily` は代わりに **各暦日の最初のセッション** をトリガーにする。「今日もう走ったか」の判定は
共有クレートの `harness_core::daily::DailyGuard`（タスク名ごとに独立した状態キー）に委譲する。

タスクは `~/.daily/config.toml` の `[[task]]` 配列（`name` + shell `command` + 任意 `dir`）に
登録し、各タスクは `sh -c` で実行される。1 つも登録が無ければ組み込みの `security` タスク
（`cargo deny check advisories bans sources licenses`, `default_security_task`）が走り、
従来挙動を維持する。実行結果は非ブロッキングな `additionalContext`（1 行要約）としてセッションに
注入され、各 run は JSONL レポート（`~/.daily/reports.jsonl`）に追記される。subscription-native
（1 hook + 同梱バイナリ、API キー不要、マシン外送信なし）。

**注**: これは別クレートの `daily-report`（git ログ + Obsidian record から日報を合成する LLM 駆動の
skill-only プラグイン）とは無関係の、hook + バイナリ型プラグインである。

## 不変条件

- **ターン非中断（ハード不変条件）** — `session-start` は `harness_core::hook::run_hook` でラップされ、
  config 欠落・不正 TOML・stdin 空/破損・タスク失敗・spawn 失敗のいずれでも **常に exit 0**
  （`tests/integration.rs` の valid/empty/malformed payload テスト、`main.rs` の doc-comment）。
- **暦日あたり 1 回・失敗も「実行済み」** — `DailyGuard::should_run()` は保存日付 ≠ 今日のときだけ真。
  `mark_done()` は **outcome に関わらず** 実行後に刻まれるため、失敗タスクも「今日実行済み」となり
  翌日まで再試行しない（`session_start_cmd` のループ、`session_start_writes_report_and_report_cmd_shows_it`）。
- **タスク名の一意性** — `name` は 1 日 1 回の状態キー（`~/.daily/state/<name>-daily.txt`）を兼ねるため、
  `add_cmd` は既存と重複する name を exit 1 で拒否する（`add_rejects_duplicate_name`）。
- **fail-soft** — `parse_config` は不正 TOML を default（enabled・タスク無し）へフォールバックし
  error は stderr へ。`load_config` は config 欠落で default。`append_report` は IO/serialize エラーを
  黙って握り潰す。`parse_reports` は破損 JSONL 行をスキップする（`report_jsonl_round_trips`）。
- **driver-skip（作業中は割り込まない）** — `skip_when_driver_active`（既定 true）が真かつ `driver_active()`
  が真なら run 全体を静かにスキップ。タスクは `pending today` のまま、次に driver 不在で開始した
  セッションで走る。判定は `backlog lock status` の stdout に基づき（`driver_active_from_status`）:
  `none`/空 → 非稼働、`stale:true` を持つ JSON → 死んだ holder（非稼働）、その他の JSON object →
  稼働中、パース不能な非 `none` 出力 → 保守的に「保持中」扱い。
- **fail-open（driver 検出）** — `backlog` バイナリが不在/エラーなら driver を観測できないため
  「非稼働」を返し通常実行する（soft dependency）。
- **決定論・LLM 非依存** — 発火判定・タスク実行・要約整形はすべて決定論。バイナリは一切 LLM を呼ばない。

## 振る舞い

サブコマンドは `clap` の `Cmd` enum で定義（`main`）。

- **`session-start`（SessionStart hook 本体）** — stdin を `read_stdin`/`HookInput::parse` で読み
  （破損時 default）、`load_config` → `enabled=false` なら即 return → driver-skip 判定 →
  当日未実行の各 `effective_tasks` を `run_task` で実行 → `mark_done` で日付を刻み →
  `append_report` でレポート追記 → 何か走ったら `summary` を `additionalContext` として注入。
- **`list`** — 登録タスク（無ければ組み込み default）を、各タスクの `pending today` / `ran today`
  状態（`DailyGuard::should_run`）と shell command・`dir` 付きで表示。config の enabled 状態と
  登録件数も出す。exit 0。
- **`report [--date <d>] [--last <n>]`** — `~/.daily/reports.jsonl` を parse し、`--last n` なら
  全期間から直近 n 件、それ以外は指定日（既定は今日）の全 entry を `format_report` で表示。
  記録が無ければ「記録なし」。exit 0。
- **`add --name <n> --command <c> [--dir <d>]`** — `~/.daily/config.toml` に `[[task]]` ブロックを
  **既存の内容/コメントを保ったまま追記**（`render_task_block`, `toml_escape` で basic-string
  エスケープ）。重複 name は exit 1 で拒否。成功で exit 0。
- **`install`** — **スタブ（未実装）**: 「add the hook manually」と stderr に出して exit 0。
  hook 配線は現状 plugin 導入（`hooks/hooks.json`）または手動で行う。

### module 責務（単一 crate `src/main.rs`）

- **config / tasks** — `Config`（`enabled`/`skip_when_driver_active` 既定 true, `task: Vec<Task>`）と
  `Task`（`name`/`command`/`dir`）を TOML から deserialize。`parse_config`/`load_config`（fail-soft）、
  `default_security_task`（組み込み cargo-deny 監査）、`effective_tasks`（登録が空なら security を seed）。
- **SessionStart** — `session_start_cmd`。`run_hook` 内で enabled/driver-skip ゲート → タスクループ →
  レポート → `summary` 注入。要約は全成功で `📋`、失敗混在で `⚠️` アイコン（`summary`, 何も走らねば `None`）。
- **driver detection** — `driver_active`（`backlog lock status` を spawn, fail-open）と純関数
  `driver_active_from_status`（stdout 解釈）。
- **task 実行** — `run_task`（`sh -c` で `dir`/cwd 実行、`augmented_path` で `$CARGO_HOME/bin` を
  PATH 先頭に追加）→ `Outcome`（`Ok`/`Failed{code,brief}`/`SpawnError`）。`summarize_output`
  （error/warning/RUSTSEC 行を優先抽出）/`first_line`（160 字で truncate）。
- **report** — `ReportEntry{date,at,task,status,code,detail}`（`Outcome` から `of` で生成、`glyph`）。
  `append_report`（JSONL 追記, fail-soft）/`parse_reports`（破損行スキップ）/`format_report`/
  `render_entry_line`。
- **paths** — `daily_dir`(`~/.daily`)/`daily_state_dir`/`config_path`/`reports_path`。
