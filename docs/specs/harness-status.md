> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# harness-status 仕様

## 概要

`harness-status` は、harness 一家の各プラグインが個別に持つ状態ストアを **横断して読み取り、
human-on-the-loop の一画面ダッシュボードに集約する** read-only なバイナリである。集約する情報源は
5 つ: 今日の支出（budgetguard の ledger）、直近セッションの turn/token/USD（gauge の session records）、
プロジェクト進捗ファイル（taskprog の `.claude/progress.md`）、Stop-hook レイテンシ（中央 `hook-latency.jsonl`）、
UserPromptSubmit 注入サイズ（中央 `inject-metrics.jsonl`）。加えて `plugins` サブコマンドはモノレポの
crate をスキャンし各プラグインを活性化スコープ（always-on / event-scoped / manual）に分類する dev/HOTL
ツールを提供する。バイナリ自身は**一切書き込まず**他プラグインのストアを読むだけで、API キーも hook も
不要。`/status` skill（`commands/status.md`）か CLI から**手動で**起動する。

## 不変条件

- **read-only 集約境界** — 全モジュール（`budget`/`sessions`/`progress`/`hooks`/`inject`/`plugins`）は
  ストアを読むだけで書き込まない。`main` は各 `read`/`recent`/`report` の結果を stdout へ印字するのみ。
  skill（`commands/status.md`）も「binary never writes」を明示する。
- **hook 非登録（意図的）** — `.claude-plugin/plugin.json` は hook を宣言せず、crate に `hooks/` dir も
  無い。README.ja が述べるとおり `SessionStart` すら登録しないのは、always-on 注入/hook 予算
  （ADR 0001）を膨らませないための設計判断であり、それ自身が計測対象とする `hooks`/`inject` 予算と
  整合させるため。活性化スコープは **manual（CLI 専用）**。
- **fail-soft（never panic）** — ストア欠落はエラーでなく「not installed」。`budget::read` は
  `ledger.json` 不在で `ledger_present:false` を返し、`sessions::recent` は records 空で空 vec、
  `progress::read` は不在で `exists:false`、`hooks::read`/`inject::read` は ledger 欠落/空で空
  report（`get(..8)` は境界安全に全文フォールバック）。`plugins::scan` は `read_dir`/JSON 失敗で
  空/スキップ。
- **canonical schema 共有（drift 防止）** — 各パネルは自前ミラー struct でなく **harness_core の正典型**
  を読む: `ledger::Ledger`（budgetguard）、`session::SessionRecord`（gauge）、`hook_latency`/
  `inject_metrics`（中央 jsonl）。writer のスキーマから逸れられない。
- **cost の権威源一致** — `sessions::recent` は各セッションの USD を **budgetguard の ledger
  （`Ledger::session_cost`）から取り**、ledger に無い時のみ `pricing::session_cost`（既定レート）へ
  フォールバックする。ゲートが実際に計上した数値と表示が乖離しない。
- **clock 依存排除** — 日付は `HARNESS_DATE` env 上書き（テスト用）→ `SystemTime::now()` から
  `days_to_date`（chrono-free の Gregorian 計算）で導出。かつて予測可能な `/tmp` パスへ空ファイルを
  書き mtime を読む TOCTOU 面があったが、これは除去済み（`today_does_not_create_world_writable_tmp_file`
  テストが回帰を守る）。
- **予算の env 上書き** — Stop-hook 集約予算は `HARNESS_HOOK_LATENCY_BUDGET_MS`（既定 30000ms）、
  注入集約予算は `HARNESS_INJECT_BUDGET_CHARS`（既定 20000 chars）。garbage/unset は既定へフォールバック。

## 振る舞い

サブコマンドは `clap` の `Command` enum で定義。`--json` と `--sessions N`（既定 5）は global 引数。

- **（引数なし・既定）** — 全 5 パネルを `budget::read`/`sessions::recent`/`progress::read`/`hooks::read`/
  `inject::read` で収集し、`display::print_status`（枠付きテキスト）または `--json` 時 `display::print_json`
  （`{date,budget,recent_sessions,progress,hook_latency,inject}` envelope）で出力。
- **`budget`** — 今日の支出のみ（`today_usd` × `session_count_today`）。`ledger_present` で未導入を示す。
- **`sessions`** — 直近 N セッション（newest-first に `last_ts` でソート → truncate）。id は先頭 8 桁表示。
- **`progress`** — `<cwd>/.claude/progress.md` を先頭 10 行プレビュー。不在時は path のみ表示し taskprog を案内。
- **`hooks`** — 中央 ledger を session 単位に `aggregate` し、`total_ms > budget` の session に ⚠ を付す。
  重い 600s-timeout ゲート（display 上は donegate/reviewgate/propguard と表記）のみが記録する前提。
- **`inject`** — 中央 ledger を turn（`turn_key = hash(session+prompt)`）単位に `aggregate` し、直近
  `RECENT_TURNS`(5) を表示、`total_chars > budget` の turn に ⚠。注入元は 5 injector（playbook/runbook/
  ctxrot/context-governor/fugu-router）を想定。
- **`plugins`** — `find_repo_root`（`crates/` と `.claude-plugin/marketplace.json` を持つ祖先まで上る）から
  `report` を生成し ALWAYS-ON / EVENT-SCOPED / MANUAL の 3 群に分類・カウント表示。

### module 責務

- **`main`** — clap CLI・サブコマンド dispatch。`today()`/`days_to_date`/`is_leap`（chrono-free 日付導出）。
- **`budget`** — `budgetguard` の `Ledger` から当日 `BudgetStatus`（`today_usd`/`session_count_today`/
  `ledger_present`）を読む。
- **`sessions`** — `gauge` の `SessionRecord` を newest-first で N 件。cost は ledger 権威源優先で
  `SessionSummary` を組む。
- **`progress`** — `taskprog` の `.claude/progress.md` を 10 行プレビューで `ProgressStatus` に。
- **`hooks`** — `harness_core::hook_latency` を消費。`budget_ms`/`SessionLatencySummary`/`HookLatencyReport`/
  `sess8`。over-budget 判定と per-hook 内訳。
- **`inject`** — `harness_core::inject_metrics` を消費。`budget_chars`/`TurnInjectionSummary`/`InjectReport`/
  `key8`。RECENT_TURNS cap と over-budget 判定。
- **`plugins`** — crate ディレクトリの活性化面（`.claude-plugin`/`hooks`/`skills`/`agents`）から `Scope`
  を分類（`classify`：always-on event 優先 → 任意 hook → manual）。`scan`/`report`/`trigger_for`/
  `read_hook_events`。プラグイン面を持たない bare library（例 `harness-core`）は除外。
- **`display`** — テキスト（`print_status`）・JSON（`print_json`）両出力の描画のみ。判定ロジックは持たない。
