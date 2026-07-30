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

- **`begin --key --title [--session] [--scope <csv>] [--done-criteria <text>]`** — キーを排他 claim
  （上記 dedup 契約）。成功時は `Lease` を挿入し `Started` イベントを追記。session_id は `--session` →
  env `CLAUDE_CODE_SESSION_ID` → `pid-<pid>` の順、run_id は env `OVERWATCH_RUN_ID` → `run-<now>`
  （`lease::resolve_session_id`/`resolve_run_id`）。`--scope` はカンマ区切りの files/globs（省略時は空 =
  scope 未確定）、`--done-criteria` は完了定義を lease の PDO-anchor フィールドに格納する（§4.1）。
  **成功時（exit 0）に advisory な success summary JSON を stdout へ出力する**（既存の exit code 契約
  0=成功/1=同 key 保持中は不変）: `{"scope_overlap":[{key,title,scope}], "possible_duplicate":
  [{key,title,similarity}]}`。`scope_overlap`（§4.5）は scope が重なる別 key の live lease を粗い
  glob-prefix マッチで列挙する早期警告（非 blocking。厳密判定は condukt の conflict-check）。
  `possible_duplicate`（§4.6a）は title/done_criteria が語彙的に類似する（Jaccard ≥ 閾値、既定 0.6、
  env `OVERWATCH_DUP_THRESHOLD` で上書き可）別 lease を `harness_core::lessons::text_similarity` で
  列挙する。両者とも既定は空配列で exit code を変えない。scope が空の lease は overlap 検知から除外
  （false positive 回避）。
- **`lease --session <id> [--json]`** — 指定 session が保持中の live lease（PDO anchor）を1件返す
  （複数保持なら最も直近に claim したもの＝`pick_session_lease`）。無ければ exit 1 で無音終了する
  （fail-soft）。§4.3 の ctxrot anchor 再注入の read path。`--json` で `Lease` を JSON、無指定なら
  `<key> — <title>` を印字する。
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
  `heartbeat_at` に加え、PDO-anchor の `scope: Vec<String>`・`done_criteria: Option<String>` を
  `#[serde(default)]` で保持——既存 `leases.json` と後方互換）、`LeaseRegistry`（`BTreeMap`）、
  `LEASE_TTL_SECS`(=1800, ハードコード)、`storage_root`
  （`harness_core::{config::base_dir, projkey}` で解決）、`load_leases`/`save_leases`(原子)/`append_event`/
  `read_events`、`is_held_by_other`/`is_stale`/`reap_stale`/`now`。`init` は no-op プレースホルダ。
- **`event`** — ライフサイクルモデル。`EventKind::{Started,Running,Ended}`（lowercase serde）と
  `LifecycleEvent`（`kind`/`key`/`title`/`session_id`/`run_id`/`ts`/optional `status`）＋ビルダ。制御専用の
  kind は無く、`control` は `Running` を流用する。
- **`lease`** — lease ライフサイクルのコマンド実装（`begin`/`run`/`end`/`heartbeat`/`reap`/`lease`）と
  `resolve_session_id`/`resolve_run_id`。dedup 契約（exit 1 + skip JSON）の実行点。`begin` の PDO-anchor
  advisory（`scope_overlap` の glob-prefix overlap＝`scopes_overlap`/`glob_prefix`、`possible_duplicate`
  の fuzzy 近似重複＝`anchor_text` + `text_similarity`、閾値 `duplicate_threshold`＝既定
  `POSSIBLE_DUPLICATE_THRESHOLD`(=0.6)、env `OVERWATCH_DUP_THRESHOLD` で上書き可）と、
  `lease_for_session`/`pick_session_lease`（session の現在 anchor 選択）を含む。
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

## Continuous-Audit / 統合レビューサーフェス (review-queue)

> **REVIEW-NEEDED**: コードから逆算 (2026-07-10 セッション)。人間レビュー前は正典としない。

**概要**: 従来別々だった3つの観測ストリーム — (1) systemic な gate 違反、(2) canary の rollback
事象、(3) AI/敵対的レビューの CONFIRMED findings — を、`review_queue.rs` が **1本の時系列リスト**
（新しい順）に統合する。指摘の永続化は `review_finding.rs`、ラウンド収束メトリクスは `audit_round.rs`
が担う。3モジュールとも純粋データ＋純関数で、発見・反証という意味判断（LLM 駆動の finder→verifier）は
コアの外（`/continuous-audit` skill + `scripts/continuous-audit.sh`）にあり、ここは決定論の記録・集計・
統合表示だけを持つ。

**不変条件**:
- **`build_queue` は複数スライス上の純関数** — `review_queue::build_queue` は systemic 署名再発
  (`SignatureRecurrence`)・rollback 事象 (`RollbackEvent`)・AI findings (`ReviewFinding`)・condukt
  escalations (`ConduktEscalation`) を入力に取り、各行に `EntryKind` 判別子
  (`systemic`/`rollback`/`ai-finding`/`escalation`) を付けて `ts` 降順（**newest-first**）でマージする。
  I/O・wall-clock を持たない。
- **fail-soft（観測系の never-break-a-turn 不変）** — CLI シェル (`review_queue::run`) は各ストアを
  fail-soft に読む。いずれかのソースが欠落/空/破損でも、そのソースは何も寄与せず他のソースは表示され、
  コマンド全体はエラーにしない。AI findings ストア (`review_findings.jsonl`) が未生成のときは
  ai-finding 行が単に出ないだけ（graceful degrade）。
- **AI findings は永続 append-only ストリーム。ingestion 口は `record-finding` の一点** —
  `review_finding.rs` は overwatch のストレージ root 配下 `review_findings.jsonl` を定義する。書き込みは
  `overwatch record-finding` のみが行い、finding TEXT を持たない reviewgate のゲートログとは独立。
  review-queue は **finding-id で dedup** し、同一 id の再供給は重複行にならず最新状態へ畳まれる
  （`/continuous-audit` が毎ラウンド同じ id を再利用しても1行に収束する前提）。
- **finding の verdict は三値 (`AuditVerdict`: confirmed / refuted / unverified)** — 敵対的検証の結果は
  二値ではない。`unverified` は「立証も反証もできなかった＝判定不能」であり、**制限側の既定**である:
  パース不能な verdict 値は `unverified` に倒れ (silently confirmed にならず、行も捨てられない)、
  verdict キーの無い旧行だけが `confirmed` として読まれる (旧 ingestion 契約が CONFIRMED subset 専用
  だったため)。`confirmed` のみが `--to-backlog` で backlog へ橋渡しされ、`unverified` は
  review-queue に `[UNVERIFIED]` マークつきで残り続ける (pending 扱い: 対応済みにも棄却にもしない)。
- **audit-round ledger は per-round メトリクスの append-only 記録** — `audit_round.rs` はラウンドごとに
  `{new_findings, confirmed, unverified, regression_tests_added}` を追記するだけで、finder/verifier は模さない。
  `unverified` は `confirmed` に畳み込まれない (`new_findings - confirmed` を「残りは refuted」と
  読ませないため)。
  ラウンド越しに読み戻すと収束シグナル（per-round new-findings が下降、closure-rate = 回帰テスト数 ÷
  confirmed）が得られる。emission は fail-soft。

**振る舞い**:
- **`overwatch review-queue [--json] [--since <ts>] [--limit <n>] [--to-backlog]`** — 統合リストを人間可読
  （`[systemic]`/`[rollback]`/`[ai-finding]`/`[escalation]` タグ付き・新しい順）または `kind` 判別子付き
  JSON 配列で表示する。`--since`/`--limit` で窓を絞る。`--to-backlog` は CONFIRMED review findings を
  backlog へ橋渡しする。
- **`overwatch record-finding --source <src> [--verdict confirmed|refuted|unverified] …`** — AI finding を1件
  `review_findings.jsonl` へ追記する（review-queue の ai-finding アームの唯一の書き込み経路）。
  `/continuous-audit` の CONFIRMED subset と UNVERIFIED subset がここへ流れる。`--verdict` を省略すると
  `confirmed`（旧 CONFIRMED 専用契約との後方互換）、未知の値は `unverified`（判定不能は制限側）。
- **`overwatch audit-round record --round <id> --target <csv> [--new-findings N] [--confirmed N]
  [--unverified N] [--regression-tests-added N]`** — 1ラウンドのメトリクスを収束 ledger へ追記する。`--round` は
  **任意の String 識別子**（round id。連番・日付・週番号いずれも可）で、`audit-round close --round <id>
  --tests <n>` が後から同じ id のラウンドの `regression_tests_added` を確定できる（closure feedback）。
- **`overwatch record-disposition` / `compact-findings` / `auto-approved`** — review-effectiveness の
  補助口: finding の disposition（FP/agreement/latency メトリクス）記録、resolved finding の非破壊
  ローテーション（review-queue の読み取りを open 項目に限定）、auto-approve 済み母集団の可視化。
- **`overwatch audit-metrics [--json] [--window <n>]`** — ledger を読み戻し、per-round new-findings 推移・
  closure-rate・`converging` フラグ（既定は末尾3ラウンドの下降判定）を印字する。`converging` は
  successive round が **同一スコープ** を再監査したときのみ意味を持つ（スコープを広げた round では
  new-findings 増加は退行ではない）。
  `converging` は **三値**（`yes` / `NO` / `unknown`）。window 内のラウンドが 2 本未満だと隣接ペアが
  無く趨勢が読めないので `unknown` を返す — 旧実装はここで `true`（「vacuously converging」）を
  返しており、これは判定不能を permissive へ写す CLAUDE.md 第3節そのものだった。しかも
  **ledger を壊すだけで到達できた**: `audit_rounds.jsonl` を truncate / corrupt / chmod 000 すると
  出荷済み 0.2.15 バイナリは `converging: false` → `true` を exit 0 で返した（実測）。
  現在は読み取り自体も三値で、**読めない ledger は「ラウンド 0 本」ではなくエラー**になる。
  残存ギャップ: レコード境界ちょうどでの truncate は妥当な短い ledger になるため内容だけでは
  検出できない（`crates/overwatch/tests/verdict_monotonicity.rs` の known-gap テストが固定）。
- **`overwatch undetermined-metrics [--json] [--window-days N]`** — `harness_core::undetermined` が
  書く「判定不能テレメトリ」stream を集計し、**ゲートが実際にどれだけ諦めているか**を crate 別・
  site（`file:line`）別に印字する（backlog 6d493e39）。第3節は「判定不能は制限側へ倒せ」と要求するが、
  **どれだけ倒れているかは誰も測っていなかった** — 正しくブロックする fleet と壊れている fleet を
  区別する手段が無い状態だった。
  - 集計キーは **site**。reason 文字列はパス・errno・exit code を含むためほぼ全て異なり、
    まとめるには正規化が必要になる。正規化は**本来別物の give-up を合流させて趨勢を捏造しうる**ので
    採らない。site は正確で、監査者が次に開く場所そのものである。
  - **読み取りは fail-closed**: ledger 不在は `Known(empty)`（「まだ誰も諦めていない」は実観測）だが、
    **読めない / パースできない ledger はエラー**（exit 1）であって 0 件レポートではない。部分集計を
    total として出すのは、この stream が測るべき量そのものを過小申告する行為である。
  - **0 は必ず sink 状態と併記する**。`0 件` と「そもそも記録されていない」は同じ `0` に見え、後者は
    good news として読まれる。`sink:` 行（`active (path)` / `SUPPRESSED (CARGO)` / `DISABLED` /
    `UNRESOLVABLE`）を counts より**前**に出すのはこのため。
  - per-process cap に当たった記録が window 内にあると total は **FLOOR** とラベルされる（真の件数は
    それ以上）。window から外れた件数も `excluded by window` として明示する（silent cap を作らない）。
  - 書き込み側は `Undetermined` の payload が private field の `Undet` なので、**記録を経由しない
    `Undetermined` の生成は compile error**（`crates/harness-core/tests/ui/verdict/forge_undetermined.rs`
    が E0603 で固定）。既存 `Undetermined` の転送は再記録しない（origin が既に1回数えている）。
- **rollback 事象の記録口** — `scripts/rollout-plugins.sh` の canary auto-rollback 時に
  `overwatch record-rollback` が `RollbackEvent` を追記する（fail-soft: 記録失敗はロールアウトを止めない）。
