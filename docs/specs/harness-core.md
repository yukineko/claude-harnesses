> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# harness-core 仕様

## 概要

`harness-core` は **プラグインではなく、ビルド時のライブラリ crate** である（`plugin.json`・hook・committed
`bin/` を持たない）。harness の全プラグインが自身の自己完結バイナリへ **静的リンク** して合成するため、配布される
`crates/<plugin>/bin/` は実行時に `../harness-core` を参照しない。ここには「全プラグインで挙動が同一でなければ
ならない土台」——並列セッション安全なノートストア（`store`）、ターンを絶対に壊さない hook/gate ラッパ
（`hook`/`gate`）、`~/.claude/settings.json` インストール機構（`install`）、プロジェクトアドレッシング
（`projkey`/`hash`）、transcript→(usage,cost) 推定（`usage`/`pricing`/`estimate`）、cross-project 学習
（`lessons`/`retrieval`）、決定論スコアラー（`scorer`）——だけを一度実装する。ドメインロジックや config/metrics の
*フィールド* は各プラグイン側に残す。crate ルートには `estimate_transcript_cost` と `score`（＋関連型）を re-export し、
単一 API として呼べるようにしている。`lib.rs` は module 一覧の唯一の宣言点。

## 不変条件

- **panic=unwind 強制** — `lib.rs` の `compile_error!`（`#[cfg(not(panic = "unwind"))]`）が `panic="abort"` でのビルドを
  拒否する。`hook::run_hook` と `gate::run::run_guarded` の `catch_unwind` は abort 下で NO-OP になり
  never-break-a-turn 保証を無音で失うため、静かに壊すより build を大声で落とす。
- **ターンを壊さない** — hook 系の失敗は必ず exit 0 に写像する（`hook::run_hook`／`gate::run_guarded` は非対話=hook
  モードで panic を握り潰し 0 で抜ける。対話モードのみ `<name>: internal error` を stderr に出して exit 1）。台帳・
  ログ書き込み（`gate::run::append_jsonl`, `metrics::emit`, `hook_latency::record`, `inject_metrics::record`）は
  すべて best-effort でエラーを握り潰す。
- **並列セッション安全** — `store::Store` の書き込みは `project_dir`（`project_key = <basename>-<fnv1a32-hex>`）配下に
  隔離され、per-project／per-session ファイル名で衝突しない。append-only JSONL（`discovery`/`lessons`/`retrieval`/
  `metrics`/`hook_latency`/`inject_metrics`）は他プロセスの行を上書きしない。
- **advisory lock による read-check-append 直列化** — `lessons::append`（idempotent by `id`）と `retrieval` は
  `OpenOptions::create_new` の原子性で `<path>.lock` を取り、`LockGuard`（RAII, `Drop` で除去＝panic unwind でも解錠）で
  critical section を跨プロセス直列化する。取得は `LOCK_MAX_ATTEMPTS`(200)×`LOCK_RETRY_DELAY_MS`(5ms) の budget で
  fail-fast（無限スピンしない）。
- **決定論** — `hash::fnv1a32/64`（非暗号 FNV-1a、オンディスクアドレッシングの唯一の正典）、`projkey::project_key`、
  `lessons::search`（lexical Jaccard、embedding 非依存）、`code_index::search`、`scorer::score` はすべて clock/RNG/I/O を
  持たない純関数（`scorer` は proptest で clamp/finiteness/monotonicity を固定）。
- **env override は絶対パス時のみ尊重** — `lessons::store_path`（`LESSONS_STORE_DIR`）等は相対 override を無視する
  （caller cwd ごとに store が黙って分裂するのを防ぐ）。
- **fail-soft ロード** — `store::load_json`／`lessons::load`／`discovery::load`／`spans::load_from`／`retrieval::load` は
  欠落ファイルで空/既定、malformed 行はスキップし panic しない（hook から呼ばれ得るため load-bearing）。

## 振る舞い（プラグインが消費する public API 面）

- **hook I/O** — `hook::HookInput`（stdin payload）と `run_hook`／`is_headless`／`read_stdin_if_piped` を提供（詳細は下記
  module 責務）。
- **gate 実行** — `gate::run::run_guarded(name, interactive, body)` が Stop-hook 本体を panic guard 下で回し、
  `consume_skip`（one-shot skip marker 消費）と `append_jsonl`（共有 event log）を添える。`gate::runner::run` は外部
  コマンドを実行し `RawOutcome`（tail は `TAIL_CAP_BYTES`=256KiB で cap）を返す。`gate::state` は session ごとの
  `SessionState`（`load`/`save`/`reset`/`bump`）で失敗回数を TTL 付きで管理する。
- **note store** — `store::Store::{new, project_dir, write_note, list_notes}`（`.md`、mtime 新しい順）＋ `load_json`/
  `save_json`/`save_bytes`。context-ledger 系パス（`context_ledger_base`/`context_state_dir`/`ledger_path`）も提供。
- **usage/cost 推定** — `usage::aggregate`（streaming JSONL transcript を全読み込みせず model 別集計）＋
  `pricing::{rate_for, cost, session_cost}`（cache read/write 倍率込み）を `estimate::estimate_transcript_cost`
  （crate ルート re-export）が 1 呼び出しに束ねる。`gauge`/`budgetguard`/`session-insights` はこれを共有し独自推定を持たない。
- **cross-project 学習** — `lessons::{append, load, search, search_default(k=DEFAULT_K=3), stats}` が machine-scope の
  `~/.lessons/lessons.jsonl` を保守。`retrieval::{record, load, retrieval_stats}` は検索イベント台帳。
- **priority scoring** — crate ルート `score(&Candidate)` = `severity.weight() × clamp(goal_proximity,0,1) ÷
  effort.factor() × lens.multiplier()`（`Lens::L2`/`L5`=security/safety は >1.0 の乗数＝壊さない・安全側）。
- **install/settings** — `install::{load_settings, backup, write_settings, command_group, push_group,
  remove_hooks_from_settings}` が `~/.claude/settings.json` の read/timestamp-backup/write と marker による所有権検出を担う。

### module 責務

- **`hook`** — 全 hook が共有する stdin payload 構造体（`HookInput`/`ContextWindow`）と、ターンを壊さない `run_hook`／
  `is_headless`／`read_stdin_if_piped`／`catch_silent`／`catch_and_log`。**詳細は `docs/specs/harness-core-hook.md`。**
- **`gate`**（`run`/`runner`/`state`）— Stop-gate 機構。`run_guarded` panic guard、`consume_skip`、共有 event log
  `append_jsonl`、外部コマンド runner（`RawOutcome`, tail cap）、session 単位の失敗カウント state（TTL bump/reset）。
- **`store`** — 永続・Obsidian 互換の per-project ノートストア（`Store`, `project_key`, `load_json`/`save_json`）＋
  context-ledger のパス解決。並列セッション安全のフォールバックを内包。
- **`lessons`** — machine-scope・project-INDEPENDENT な cross-task 学習ストア。`Lesson`/`Kind`（error-pattern/convention）、
  idempotent-by-id な `append`（advisory lock）、lexical Jaccard `search`、`Stats`。
- **`retrieval`** — lessons 検索イベントの append-only 台帳（`RetrievalEvent`/`record`/`retrieval_stats`）。`lessons` の
  `LockGuard` を再利用する。
- **`code_index`** — 決定論的な symbol 索引。`Symbol` 抽出（`extract_symbols`）、`write_index`/`load_index`、`fingerprint`、
  lexical `search`（`Scored`, `DEFAULT_K`=10）。embedding 非依存の code-RAG 基盤。
- **`scorer`** — 決定論優先度スコアラー。`score`/`Candidate`/`Severity`/`Effort`/`Lens`（crate ルート re-export）。純関数。
- **`usage`/`transcript`/`estimate`/`pricing`** — streaming transcript リーダー、model 別 usage 集計（`Aggregate`/
  `ModelUsage`/`subagent_usage`）、pricing 表（`Rate`/`PriceOverride`）、両者を束ねる `estimate_transcript_cost`。
- **`install`** — `~/.claude/settings.json` の load/backup/write と command-marker owner 検出、hook group の push/strip。
- **`config`** — home/base-dir 解決、`expand_tilde`、`env_u64`/`env_bool` の env パースプリミティブ。
- **`hash`/`projkey`** — FNV-1a（32/64bit、`Fnv1a64` incremental）と project key `<basename>-<fnv1a32-hex>`、`repo_root` 解決。
- **`discovery`** — per-repo の重複ガード台帳。`DiscoveryRecord`/`append`/`already_discovered_by_other`/`mark_selected`
  （cross-session の二重着手防止、idempotency-by-fingerprint）。
- **`session`** — session ごとの正典レコード（`SessionRecord`, `upsert`/`load_one`/`load_all`, `<state_dir>/sessions/<id>.json`）。
- **`ledger`/`daily`** — 日次支出台帳（`Ledger`/`DayEntry`）とカレンダー日単位の one-shot ガード（`DailyGuard`）。
- **`inject`/`inject_metrics`** — context-injection hook の共有基盤（layered config load、`CharBudget`, `truncate_chars`）と、
  `turn_key = hash(session+prompt)` キーで 5 インジェクタがプロセス間協調なしに同一ターン分を合算する injection サイズ台帳。
- **`hook_latency`** — Stop-hook レイテンシの中央 append-only 台帳（`record`/`aggregate`/`over_budget`, best-effort）。
- **`metrics`/`spans`** — 並列安全な append-only JSONL メトリクス SINK（`emit`）と Span モデル＋防御的 JSONL ローダー（`load_from`）。
- **`interrogate`** — ドメイン非依存の gate 単位 interrogation 制御構造（`RigorGates` trait, `Bundle`/`OpenQuestion`/
  `CarveState`, `evaluate`/`apply`）。
- **`shell`/`trust`** — クロスプラットフォームな shell 起動の唯一の正典（`shell`/`command`）と、project-local config の
  コマンド文字列を尊重するための workspace-trust ゲート（`is_trusted`/`add`/`remove`/`trust_all`）。
