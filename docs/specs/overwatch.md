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
