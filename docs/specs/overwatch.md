> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# overwatch 仕様

## 概要

`overwatch` は、プロジェクト全体（cross-session）の実行レジストリと dedup ガード、および PDO 進捗ビューを
束ねるハーネスである。中核は `store::Lease`（key→保有 session の claim）を `<base>/<project-key>/overwatch/`
配下の `leases.json` に永続化し、ライフサイクルを `events.jsonl`（append-only）へ記録すること。単一 Rust
バイナリ + 2 フック（SessionStart / Stop、ともに `overwatch status` を注入）で動き、サブスクリプション
ネイティブ（追加 API key 不要）。dedup 契約は「`begin` が別 live session に保持されたキーへ試行したら
skip JSON を stdout に出して exit 1」で、呼び出し元は先へ進んではならない。`status`/`sessions` は overwatch
台帳に加え backlog / hypothesis / condukt / compass を fail-soft にシェルして集約する（`aggregate::build`）。
pause/resume/reassign の制御面は下流（condukt）へ委譲するが、人間承認（HOTL）はバイナリ内ではなく
`/overwatch` skill 側に置く。

**partial/stub 注意（実装が README/SKILL に未追随）**: (1) TTL は `store::LEASE_TTL_SECS` に **1800 秒
(30 分) ハードコード**で、README/SKILL が謳う「既定 5 分・`overwatch.toml` で設定可能」は未実装（config
モジュール・toml 読み込みは存在しない）。(2) `overwatch install` コマンドは無い（README の手動インストール
手順は未実装）。(3) `store::init()` は no-op プレースホルダ。(4) `store.rs` の docstring は保存先を
`~/.overwatch/...` と書くが、実装は `harness_core::config::base_dir("overwatch")`（既定
`~/.local/share/claude-harnesses`）。(5) `reap --ttl-secs`/`--session` 等 SKILL 記載のフラグは CLI に無い。

## 不変条件

- **dedup 契約（ハード不変条件）** — `lease::begin` は load → `reap_stale` の後 `store::is_held_by_other`
  が真（別 session_id かつ非 stale）なら holder 情報を含む skip JSON を print し `std::process::exit(1)`。
  同一 session の再取得は冪等で、`claimed_at` を保持して heartbeat のみ更新する。
- **liveness = heartbeat TTL** — lease の生死は保存 pid ではなく `heartbeat_at` の鮮度で判定する
  （`store::is_stale`＝`now - heartbeat_at > LEASE_TTL_SECS`、`saturating_sub`）。crash/hang した session は
  heartbeat が止まり stale 化し、`reap`/次 `begin` の `reap_stale` で除去される。
- **fail-soft 永続化** — `load_leases` は欠落・破損 JSON を空レジストリとして扱う（`unwrap_or_default`）。
  `save_leases` は temp + rename の原子的書き込み。`read_events` は不正行を捨てて読める分だけ返す。
- **fail-soft 集約** — `aggregate::build` は infallible。台帳ロード失敗、下流バイナリ（backlog/hypothesis/
  condukt/compass）の欠落や非ゼロ終了はすべて `shell_soft` が `None` を返して該当セクションを省くだけで、
  turn を落とさない。
- **制御面 fail-soft & 監査必須** — `control::{pause,resume,reassign}` は下流 condukt が無い/非ゼロでも
  panic せず（`run_fail_soft`）、成否にかかわらず `record_control_event` で overwatch 台帳へ `running`
  イベント（`title = "control:…"`）を必ず追記する。HOTL 承認自体はバイナリでなく skill が担う。
- **決定論的順序** — `LeaseRegistry` は `BTreeMap`。`roster_from_leases` は session_id でソートされ、
  出力・シリアライズは決定論的。

## 振る舞い

CLI は `clap` の `Command` enum（`main.rs`）で定義。ledger/lease 系はすべて `store::now()` と cwd から
解決した `<base>/<project-key>/overwatch/` を土台にする。

- **`begin --key --title [--session]`** — キーを排他 claim（上記 dedup 契約）。成功時は `Lease` を挿入し
  `Started` イベントを追記。session_id は `--session` → env `CLAUDE_CODE_SESSION_ID` → `pid-<pid>` の順、
  run_id は env `OVERWATCH_RUN_ID` → `run-<now>`（`lease::resolve_session_id`/`resolve_run_id`）。
- **`run --key [--note]`** — 保持中 lease の `heartbeat_at` を更新し `Running` イベント（note を status に）を
  追記。**キー未保持でも fail-soft**（派生 id でイベントのみ記録）。
- **`heartbeat --key`** — lease があれば TTL リセットし `{"refreshed":1}`、無ければ `{"refreshed":0}`。
- **`end --key --status`** — `Ended` イベント（終端 status）を記録してから lease を remove。未保持でも
  イベントは記録（fail-soft）。
- **`reap`** — stale lease を全削除し `{"reaped":N}` を出力。
- **`status [--json]`** — `aggregate::build` の `ProgressView` を人間可読 5 セクション（Sessions / PDO
  hypotheses / Backlog / Condukt runs / Compass gap）または pretty JSON で表示（`render::status`）。
  SessionStart / Stop フックの注入先。
- **`sessions [--json]`** — per-session lease roster のみを表示（`render::sessions`）。
- **`pause --run` / `resume --run`** — `condukt state pause|resume --run <id>` へ委譲（`control`）。
- **`reassign --key --to`** — 現 lease を release（load→remove→save）し、新保有者 `to` を監査イベントに記録。

## module 責務

- **`store`** — lease 永続化・liveness 判定の中核。`Lease`（`key`/`title`/`session_id`/`run_id`/`claimed_at`/
  `heartbeat_at`）、`LeaseRegistry`（`BTreeMap`）、`LEASE_TTL_SECS`(=1800, ハードコード)、`storage_root`
  （`harness_core::{config::base_dir, projkey}` で解決）、`load_leases`/`save_leases`(原子)/`append_event`/
  `read_events`、`is_held_by_other`/`is_stale`/`reap_stale`/`now`。`init` は no-op プレースホルダ。
- **`event`** — ライフサイクルモデル。`EventKind::{Started,Running,Ended}`（lowercase serde）と
  `LifecycleEvent`（`kind`/`key`/`title`/`session_id`/`run_id`/`ts`/optional `status`）＋ビルダ。制御専用の
  kind は無く、`control` は `Running` を流用する。
- **`lease`** — lease ライフサイクルのコマンド実装（`begin`/`run`/`end`/`heartbeat`/`reap`）と
  `resolve_session_id`/`resolve_run_id`。dedup 契約（exit 1 + skip JSON）の実行点。
- **`aggregate`** — 状態射影。`ProgressView` とサブ構造（`SessionRoster`/`LeaseInfo`/`BacklogSummary`/
  `HypoBuckets`/`RunRow`）、純パーサ（`roster_from_leases`/`parse_backlog`/`bucket_hypotheses`/
  `parse_condukt_runs`）、fail-soft シェル（`shell_soft`）、`build`（infallible 集約）。
- **`render`** — `ProgressView` の人間可読 / JSON 整形（`status`/`sessions` と各 `format_*`）。
- **`control`** — 制御面。純 argv ビルダ（`pause_cmd`/`resume_cmd`）、fail-soft 実行（`run_fail_soft`）、
  監査追記（`record_control_event`）、`pause`/`resume`/`reassign`。HOTL 承認は skill 側。
- **`lib`** — テスト・他クレート向けに `event`/`store` を re-export する薄い公開面。

## fleet 相関エラー検知 (violation signatures)

> **REVIEW-NEEDED**: コードから逆算 (2026-07-09 セッション)。人間レビュー前は正典としない。

**概要**: `violation.rs` は blastguard（破壊的コマンド拒否）/ propguard（PROP-* 失敗）/ specguard（spec drift
検出）/ mutategate（mutant kill 失敗）由来の gate 違反を、正規化した**署名（signature）**付きで記録する
append-only なレジストリである。同じ種類の失敗は、発生した task/session に依らず同じ署名に畳み込まれる。

**不変条件**:
- **正規化は純関数** — `normalize_signature(source, &RawViolation)` は `<source>:<discriminator>[:<discriminator2>]`
  形式の安定した文字列を返す純関数。大小文字・空白差は `norm`（trim + lowercase + space→`-`）で吸収し、
  同一失敗が表記ゆれで別署名に分裂しない。`ViolationSource::{Blastguard,Propguard,Specguard,Mutategate}`
  ごとに discriminator の由来フィールド（`rule_id`/`property_id`/`drift_kind`+`symbol`/`mutation_operator`）
  が異なるが、`RawViolation` 経由で単一の正規化関数に集約される。
- **時刻は注入される** — `detect_recurrence`/`systemic_issues` は `now: i64` を引数で受け取り、内部で
  wall-clock を読まない。ウィンドウ判定は `now - ev.ts > policy.window_secs`（未来イベント `ev.ts > now`
  も除外）。呼び出し側（`violation_cli::recurrence_report`）が `store::now()` を一度だけ読んで渡す。
- **systemic 昇格は「閾値到達 AND 複数 task/session に跨る」の両方が必要** — `detect_recurrence` の
  `is_systemic` は `occurrences >= policy.threshold && (distinct_tasks > 1 || distinct_sessions > 1)`。
  単一 task が同じ違反を何度リトライしても `distinct_tasks == 1 && distinct_sessions == 1` である限り
  systemic とはならない（ローカルなリトライループと fleet 規模の相関エラーを区別するための不変条件）。

**振る舞い**:
- **`record-violation --source --discriminator [--symbol] --task [--session] [--detail]`** —
  `parse_source` で source token をパースし、`violation_cli::record` が `RawViolation` を組み立てて
  `violation::build_event`（`normalize_signature` を内部で呼ぶ）で `ViolationEvent` を生成、
  `store::append_violation` で project-wide ledger へ追記する。`session` 省略時は `resolve_session_id`
  が `--session` → env `CLAUDE_CODE_SESSION_ID` → `pid-<pid>` の順で解決（`lease.rs` と同じパターン）。
  `{"recorded":true,"signature":...}` を標準出力に出す。
- **`violations [--json] [--threshold] [--window-secs]`** — `resolve_policy` で `RecurrencePolicy`
  （既定 `threshold=3`, `window_secs=86400`＝24h）を組み立て、`violation_cli::print_recurrence` が
  全署名の再帰統計（occurrences/distinct_tasks/distinct_sessions/first_seen/last_seen/is_systemic）を
  人間可読（`[SYSTEMIC]`/`[isolated]` マーカー付き）または JSON で表示する。
- **`escalations [--json] [--threshold] [--window-secs]`** — 同じ `recurrence_report` を計算した上で
  `is_systemic` な署名だけに絞り込んで表示する（`violation_cli::print_escalations`）。ウィンドウ内で
  同一署名が閾値回数以上、かつ複数の distinct task/session にまたがって再発したときに初めて
  ここへ現れる。

## canary 段階展開 (canary rollout)

> **REVIEW-NEEDED**: コードから逆算 (2026-07-09 セッション)。人間レビュー前は正典としない。

**概要**: `canary.rs` は `scripts/rollout-plugins.sh --canary` が使う段階的ロールアウトの決定論的コアである。
プラグイン集合をステージへ分割し、fleet 相関エラー検知（violation-rate）を健全性シグナルとして
`Proceed`/`Rollback` を判定し、ロールバック時に何を復元すべきかを計算する。3つとも純粋データ生成のみで、
実際の rollout・レジストリ書き換えは一切実行しない。

**不変条件**:
- **plan/health-gate/rollback-plan はすべて純関数** — `plan_stages_by_size`/`plan_stages_by_count`/
  `decide_from_count`/`evaluate_health_gate`/`compute_rollback_plan` はいずれも I/O・乱数・wall-clock を
  持たず、同一入力から同一出力を返す（テスト `stage_planning_is_deterministic`/
  `evaluate_health_gate_is_deterministic_under_injected_time`/`compute_rollback_plan_is_deterministic`
  で担保）。`stage_size`/`stage_count` に 0 を渡しても panic せず 1 に丸めて最も保守的なカナリア
  （1 プラグイン/ステージ）に縮退する。
- **health-gate の `now` は注入パラメータ** — `evaluate_health_gate`/`evaluate_health_gate_systemic`は
  `now: i64` を引数として受け取り、内部で wall-clock を読まない。item-B（violation.rs）と同じ
  ウィンドウ規約（`ev.ts <= now && now - ev.ts <= window_secs`、`violations_in_window`）を再利用する。
  CLI 層 (`canary_cli::gate`) のみが `now_override.unwrap_or_else(store::now)` で一度だけ現在時刻を
  読み、それ以降は明示引数として渡す。
- **rollback plan は「復元先を記述する純データ」であり、コア自身はロールバックを実行しない** —
  `compute_rollback_plan` は `PriorInstallState`（stage 適用前のレジストリ状態）と `CanaryTarget`
  （stage が実際に動かした先）を plugin 名で突き合わせ、`RollbackPlan`（`PluginRollbackTarget` の列。
  `prior_version`/`restore_install_path`/`canary_version`/`is_new` を含む）を返すだけで、ファイルシステムや
  `installed_plugins.json` には一切触れない。新規導入プラグイン（prior state が無い）は `is_new: true` と
  なり、復元先が無いことを明示する（`RollbackPlan::all_new`）。

**振る舞い**:
- **`canary-plan --plugins [--stage-size | --stage-count]`** — `parse_plugin_list` でカンマ/空白区切りを
  正規化した後、`stage_size` 指定があれば `plan_stages_by_size`、無く `stage_count` があれば
  `plan_stages_by_count`、両方省略なら `plan_stages_by_size(&list, 1)`（最も保守的な既定値）で
  `StagePlan` を組み立てて JSON 出力する。`stage_size` と `stage_count` が両方指定された場合は
  `stage_size` が優先される。
- **`canary-gate [--observed-violations] [--threshold] [--window-secs] [--systemic] [--now]`** —
  2つの入力モードを持つ。(1) `--observed-violations` 指定時は完全に純粋な経路で
  `decide_from_count` へ直接渡す。(2) 省略時は cwd の item-B violation レジストリ
  （`store::read_violations`）を読み、`--now` があればそれを、無ければ `store::now()` を `now` として
  `evaluate_health_gate`（`--systemic` 指定時は `evaluate_health_gate_systemic`。孤立した一過性ノイズを
  除き、systemic に昇格した署名の件数のみを閾値と比較する）で判定する。違反率が閾値を超えたら
  `GateDecision::Rollback`、以下なら `Proceed`。判定結果は JSON で出力され、`main.rs` は rollback 判定時
  `std::process::exit(3)` でシェル側に非ゼロ終了として伝える。
- **`canary-rollback-plan --stage-index --prior --canary-targets`** — `prior`/`canary-targets` に
  シェル側が `installed_plugins.json` から読んだ JSON 配列（`PriorInstallState`/`CanaryTarget`）を渡すと
  `compute_rollback_plan` を呼んで `RollbackPlan` を JSON 出力する。
- **実際のロールアウト実行は shell 側の opt-in フラグ経由** — `scripts/rollout-plugins.sh` は既定では
  `--canary` フラグ無しで従来通り動作し、`--canary`（+ `--canary-stage-size`/`--canary-threshold`）を
  明示的に渡したときのみ、上記3サブコマンドの出力を使って段階的ロールアウト・health-gate 判定・
  ロールバックを実際に実行する。つまり overwatch の Rust コアは計画・判定のみを行い、副作用
  （プラグインの実配布・レジストリ書き換え）は一切持たない。
