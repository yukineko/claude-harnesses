> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# budgetguard 仕様

## 概要

`budgetguard` は Claude Code のセッション見積もりコスト (USD) を監視し、設定上限を超えたら Stop を
**ブロックする**「コスト予算ゲート」ハーネスである。gauge が観測（read-only）に徹するのに対し、
budgetguard は制御（turn を止める）を担う。`budgetguard gate` が Claude Code の **Stop** hook に配線され
（`hooks/hooks.json`: `${CLAUDE_PLUGIN_ROOT}/bin/budgetguard gate`, `timeout` 30s）、各 Stop で
(1) セッション transcript (JSONL) を読んでモデル別トークンを集計し、(2) `harness-core` 組み込み料金表で
USD を見積もり（`estimate_transcript_cost` → `TranscriptCostEstimate::cost_usd`、gauge と同一）、
(3) 日次台帳 `ledger.json` を更新し、(4) セッション合計と当日累計を上限と照合して verdict を出す。
コスト計算・料金表・台帳（`harness_core::ledger::Ledger`）はすべて `harness-core` に委譲し、本 crate は
gate 判定・config 層・ledger ロック・hook 配線という薄い周辺に徹する。判定に API キーは不要（transcript は
既にディスク上にあり決定論的に読むだけ）。

## 不変条件

- **turn を壊さない（fail-soft ハード不変条件）** — gate は「ブロックする」ときですら常に **exit 0** し、
  ブロックは stdout の `{"decision":"block", …}` JSON で表現する（`gate::emit_and_exit` は全分岐で
  `exit(0)`）。config 欠落・transcript 読めない・stdin パース失敗・空 `session_id`/`transcript_path`・
  data error（`evaluate` が `None`）はすべて無出力で許可（`tests/integration.rs` の
  `gate_with_empty_fields_exits_zero` / `gate_with_malformed_stdin_exits_zero` が固定）。
- **安全側デフォルト（0 = 無効）** — 全上限は既定 0.0。`verdict` は各閾値を `> 0.0` でのみ評価するため、
  何も設定しない導入は完全な no-op（`zero_threshold_means_disabled` テスト）。`enabled=false` または
  環境変数 `BUDGETGUARD_DISABLE=1`（`Config::disabled_env`）でも即 return。
- **再帰ブロック防止** — 直前の Stop hook が既にブロックして再入した場合（`input.stop_hook_active`）は
  再ブロックせず即 return。さもなくば over-budget セッションが毎回ブロックし turn を終われなくなる
  （一度警告済みなので停止を許す）。
- **block > warn の優先** — `verdict` は session block → daily block → （両 warn 集約）の順で評価し、
  block 閾値到達なら warn を出さずブロックする。閾値判定は inclusive（`>=`。`*_is_inclusive_at_threshold`
  テスト）。session と daily は独立に発火する（`daily_block_triggers_independently_of_session`）。
- **ledger の lost-update 防止** — 日次台帳は全 Stop が更新する単一共有ファイル。`evaluate` は
  load→record→save を `lock::LedgerLock`（`O_EXCL` の `ledger.lock`、`STALE_AFTER`30s で stale steal、
  `ACQUIRE_TIMEOUT`3s、`BACKOFF`25ms）で直列化する。ロックは best-effort — 取れなくても turn を止めず
  続行し（`held=false`）、`Drop` で自分の持つ file のみ削除する。
- **corrupt ledger を破壊しない** — `Ledger::load_checked` が parse 失敗を返したら、上書きせず（当日累計を
  消して budget を fail-open させないため）ファイルを温存し、当日 total はこのセッション自身のコストに
  fall back する（保守的＝過小報告しない）。
- **日次リセットはローカル深夜** — 当日キーは UTC ではなく `chrono::Local` の `%Y-%m-%d`
  （`today_str`/`date_key`）。+09:00 (JST) ユーザの予算が 09:00 ではなくローカル深夜にロールする
  （`date_key_follows_the_carried_local_offset_not_utc` テストで tz-generic に固定）。

## 振る舞い

サブコマンドは `clap` の `Command` enum で定義（`main`）。

- **`gate`（Stop hook）** — `run_hook(gate_command)` 経由。`disabled_env`→stdin パース→空フィールド
  →`stop_hook_active` の各 early-return を通り、`gate::evaluate(cfg, session_id, transcript_path, today)`
  で verdict を算出、`emit_and_exit` が出力する。verdict は 3 種:
  **Allow**（exit 0 無出力）／**Warn**（`{"additionalContext": "⚠ budgetguard:…"}`、助言のみ非ブロック）／
  **Block**（`{"decision":"block","reason":"budgetguard: …予算超過 …作業を保存し、コミットして終了して
  ください。"}`）。実行時の session/day 合計は stderr にログし、hook が読む stdout JSON は汚さない。
- **`status [--json]`** — 解決済み config と当日支出を表示（read-only）。人間向けは
  `enabled`/`state_dir`/`session.warn|block_usd`/`daily.warn|block_usd`/`today ($…spent)` を出す。
  `--json` は `{day_usd, daily_warn_usd, daily_block_usd, pressure}` を出し、`pressure` は
  `gate::budget_pressure`（`daily_warn_usd > 0.0 && day_usd >= daily_warn_usd`、gate の warn 開始点と一致）。
  fugu-router 等の下流ルータが budget pressure を読みモデル選択を downgrade するための信号。
- **`init [--force]`** — 雛形 `./budgetguard.toml`（`include_str!("../budgetguard.example.toml")`）を
  書き出す。既存なら `--force` 無しでは上書きしない。
- **`install [--dry-run]` / `uninstall [--dry-run]`** — `~/.claude/settings.json` の `Stop` イベントに
  `<bin> gate`（`TIMEOUT_SECS`30）を marker `"budgetguard"` でマージ／除去する
  （`harness_core::install` に委譲）。`--dry-run` は結果 JSON を表示するのみ。

閾値・料金の外形: `[session]`/`[daily]` の `warn_usd`/`block_usd`（USD, 0=無効）、`[[price]]` スタンザ
（`pattern`/`input`/`output`、モデル id 部分一致で組み込み料金を上書き）。組み込み料金表（$/1M in/out）は
Fable/Mythos 10/50・Opus 5/25・Sonnet 3/15・Haiku 1/5（gauge と同一）。

### module 責務

- **`config`** — `budgetguard.toml`（project root）を優先し、無ければ `~/.budgetguard/config.toml` を読む
  1 段レイヤ（両者マージではなく先に見つかった方のみ）。`FileConfig`/`BudgetLevel`/`PriceOverrideCfg` を
  TOML から deserialize し、公開 `Config`（`enabled`/`session_*`/`daily_*`/`state_dir`/`price_overrides`）へ
  写像。read 失敗・parse 失敗は default を保つ fail-soft。`disabled_env`（`BUDGETGUARD_DISABLE`）と
  `base_dir`（`harness_core::config::base_dir("budgetguard")`）を提供。
- **`gate`** — 中核判定。`evaluate`（transcript→cost 見積 → lock 下で ledger 更新 → `verdict`）、
  `verdict`（純: block→warn の優先ロジック）、`budget_pressure`（純: 下流ルーティング信号）、
  `GateResult`/`Verdict`、`emit_and_exit`（`!` 返し、常に exit 0）。コスト集計と ledger は harness-core に委譲。
- **`lock`** — `LedgerLock`。ledger の read-modify-write を並行セッション間で直列化する O_EXCL 製の
  クロスプラットフォーム advisory lock。best-effort（timeout で unheld 続行）・stale steal・Drop 解放。
  外部 crate 非依存。
- **`install`** — `~/.claude/settings.json` への Stop hook マージ/除去（`harness_core::install::push_group`
  /`remove_hooks_from_settings`/`write_settings`）。EVENT=`Stop`, SUB=`gate`, marker=`budgetguard`。
