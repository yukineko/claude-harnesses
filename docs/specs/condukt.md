> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# condukt 仕様

## 概要

`condukt` は「大きめの課題を解釈→分割→合意→並列/直列スケジュール→worktree 並列実装→検証→完了ゲート」まで
一サイクル回す合意駆動オーケストレーターである。README.ja.md が宣言するとおり **役割を二分**する: *解釈・
実装・検証* という判断は LLM（`/condukt` スキル + interpreter/researcher/worker/verifier の4エージェント）に置き、
*どのタスクを並列化できるか・worktree ライフサイクル・実行状態・完了判定* という決定論はバイナリ（`condukt`）に
置く。バイナリは単一 Rust 実行ファイルでジョブ単位のサブコマンドを公開し、subscription-native（`ANTHROPIC_API_KEY`
不要）。処理はスキル・4エージェント・1つの SessionStart hook（`restore`）・1つの Stop hook（`state record-run
--all`）・1つの PostToolUse hook（`editgate`）経由で Claude Code の中で走る。中核の決定論は `schedule::schedule`
（競合分析による並列/直列バッチ化）と `state::gate_reasons`（完了ゲート）。

## 不変条件

- **判断/決定論の分離** — バイナリはスケジュール・状態・worktree・ゲートのみを決定論的に担う。実装/検証は
  agent が行う。`schedule.rs` doc-comment: 全関数は純関数・決定論（`Task.id` で安定順序）で、同じ
  decomposition は常に同じ `Schedule` を返す。
- **並列化は file-overlap が正典** — 2タスクが同一バッチに入るのは `schedule::files_conflict` が非衝突かつ
  互いに `deps` 無しのときだけ。`entries_conflict` は不確実なら「衝突あり」に倒す（誤衝突は直列化＝安全、
  取りこぼしは同一ファイル競合＝危険）。LLM 宣言の `Class`（`parallel`/`serial`/`gated`）より file-overlap が
  優先される。`shared_globs` に触れるタスクは警告付きで直列へ降格。
- **force-gate（risk 分類の権威）** — `blastguard::classify(task_action_text(t)).requires_gate()` が真の
  タスクは、LLM が宣言した `class` に関わらず `gated` へ強制隔離される（deploy/push/release 等の高リスク非可逆
  アクション）。risk/reversibility 軸の唯一の情報源は blastguard の graded classifier。
- **完了ゲート（"done" の物理的強制）** — `state::gate_reasons` は全タスクが `Status::{Verified,Cancelled,
  Discarded}` でなく、または dirty/未削除の worktree が残る限り理由を積み、`state gate` は非ゼロで終了する。
  完了宣言をモデルの感覚で許さない。
- **F→P 再現性ゲート** — `Task::requires_fp_oracle()`（`kind` が `fix`/`feature`、大小文字非依存）なタスクは、
  `state set --status verified` 昇格時に有効な Fail→Pass 遷移証明を要求する（`enforce_fp_gate`）。`gate_reasons`
  は防御的二重化として `fp_oracle_valid == Some(false)` の verified を追加検出する。判定は `state check-oracle`
  が `tdd oracle` 経由で分類。`tdd` 不在・`reproduction_tests` 無しは従来 done_criteria チェックへ fail-soft 縮退。
- **exit code 契約（決定論ゲート）** — `policy decide` は `Decision::{Auto→0, Escalate→2, Block→3}`（不正入力=1）。
  `policy::decide` のハード不変条件: high risk × irreversible は confidence に依らず必ず `Block`。`circuit check`/
  `gate check` は breaker/escalate 作動時に非ゼロ終了（`if ! condukt … ; then stop; fi`）。`state autonomy-check`/
  `worktree-mode-check`/`is-claimed` も exit code を契約とする。
- **クロスセッション占有（PDO 衝突ガード）** — `state claim/heartbeat/release`（`claims.json`）は
  `conflict-check` の一度きり助言スナップショットを live な強制リースに変える。`state set --status running` が
  自動占有し、別 run の live 保有者と衝突すれば **skip JSON を出して exit 1**、terminal 遷移で自動解放。liveness
  は ephemeral な CLI pid ではなく heartbeat（stuck-TTL）でアンカーし、古い占有は reap される。
- **fail-soft（hook は turn を壊さない）** — `editgate` は本当に壊れた判定のときだけ `{"decision":"block",…}`
  を出し、それ以外は無出力 exit 0。`state record-run`（Stop hook, `recorded_at` で冪等）・`restore`・`statusline`・
  `lessons`・`pr`・`verify *`・`sandbox run`・`escalate` はすべて欠落/エラーで no-op / safest-value に縮退。
  `CONDUKT_DISABLE=1` は SessionStart/statusline hook のキルスイッチ。
- **worktree 規律** — `worktree` サブコマンドは「リポジトリ外のパス」「1 ディレクトリ = 1 ブランチ」を強制する
  （`worktree_base` は repo 外必須）。
- **untrusted 出力の境界化** — `verify::fence_worker_output` が worker のランタイム stderr/stdout tail を
  observational-only な untrusted 出力として fence し、制御フロー入力にしない（injection 面の遮断）。

## 振る舞い

サブコマンドは `main.rs` の `Command` enum（+ サブ enum `StateAction`/`WtAction`/`VerifyAction`/`PolicyAction`
/`ReplanAction`/`ConsensusAction` 等）で定義。decomposition は interpreter が出力し `schedule` が消費する
`model::Decomposition`（`goal`/`linked_hypotheses`/`tasks:[model::Task]`）。`Task` は `id`/`title`/`touched_files`/
`deps`/`class`/`kind`/`suggested_model`/`done_criteria`/任意の `checks`/`expected_trajectory`/`is_behavioral`/
`mechanical_check`（すべて `#[serde(default)]` で後方互換）。

- **`schedule` / `validate`** — decomposition JSON（stdin or `--file`）から順序付きバッチと直列/gated リストを
  出力（`schedule::schedule`）/ 一意 ID・既知依存・循環なしを検証（`schedule::validate`）。
- **`worktree create|merge|remove|cleanup|list`** — git worktree ライフサイクル。
- **`state init|set|show|gate|list|reconcile|resume-context|record-run|test|stats|…`** — 実行状態の永続化と
  完了ゲート。`set --status verified` は F→P ゲートを強制。`reconcile` はマージ済み/削除済みブランチを `verified`
  へ昇格（stale 修復）。**その昇格の前に cross-run 重複完了を検出する（DESIGN §4.6c）**: この run が完了した
  hashkey を、別 run_id が **この run の `claimed_at` より後に** 同じく done/verified にしていないか兄弟 run を
  横断走査し、見つかれば何も変更せず `{"duplicate_completion":[{hashkey,runs:[run_id...]}]}` を出力して **exit 2**
  （escalate = 人間/HOTL がどちらの実装を残すか選ぶ。exit code 契約 0=auto/2=escalate/3=block に従う）。重複が無い
  通常パス（自動 verified・exit 0）は不変。`record-run --all` は fugu-router 向けに outcome を冪等記録（Stop hook）。`test` は
  `[test].command`→自動検出（cargo/npm/pytest）で `sh -c` 実行し exit code 伝播。
- **`state check-oracle|check-criteria`** — F→P 再現証明の判定 / `done_criteria` の機械ゲート＋`skip_verifier`
  導出（振る舞い系は常に `skip_verifier:false`）。`verify::classify_criteria` は構造化 `is_behavioral`/
  `mechanical_check` を権威ヒントに、無ければ散文ヒューリスティック（`BEHAVIORAL_MARKERS`）へ縮退。
- **`state claim/…/is-claimed`, `execution-state`, `conflict-check/abandon/pause/resume/cancel`,
  `checkpoint/rollback`, `verifier-model`** — クロスセッション占有・可逆性・実行編集・verifier≠worker モデル解決。
- **`policy decide/answer/answers`** — 中央 graded-autonomy: risk × reversibility × confidence を
  `auto`/`escalate`/`block` へ写像（`policy::decide`）。`answer` は auto 判定のみ 1 問を非対話的に self-answer。
- **`verify digest/runtime/launch/regressions/confidence/checks`** — 決定論的 verifier ヘルパー（整形のみ）。
  `launch` は blastguard 検証済みエンベロープ内で実ターゲット起動（破壊的 `--cmd` は fail-closed、`--docker` で
  隔離コンテナ、`--run-policy` で `decide_run_policy` を融合し `escalate_docker` 判定のときだけ launch）。
- **`replan`, `run-policy`, `circuit check`, `gate check`, `consensus plan/vote`, `sandbox run`, `escalate`,
  `pr create`, `lessons`, `knowledge`** — reflux 分類 / verify→docker→ship 判定 / circuit-breaker / gate-exec /
  self-consistency 投票 / opt-in docker サンドボックス / 非同期エスカレーション / gh PR（`--execute` は人間承認後
  のみ）/ cross-task 学習 capture / 規約注入。
- **`shadow-run enable|disable|status|exec|finish`** — opt-in・手動発火限定の投機実行モード
  （`shadow_run.rs`）。レートリミット窓の残り時間を取得する API が存在しないため自動発火は無く、
  `enabled` フラグ（`~/.condukt/shadow_run.json`、既定 disabled）は人間だけが立てる。`exec` は既存の
  `worktree create` で第2の worktree を作り、同一タスクを別モデルで並行実装させる。`finish` は
  worktree/branch を**必ず破棄**（merge は絶対にしない。本採用は主ワーカーの成果のみ）した上で、
  pass/fail/cost/duration を `fugu-router record --class shadow-run` へベストエフォートで記録する
  （fugu-router 不在でも成功するソフト依存）。
- **`restore`（SessionStart）/ `statusline` / `editgate`（PostToolUse）/ `status [--all]` /
  `loop --module <server|client|e2e>` / `init` / `install [--dry-run]` / `uninstall`** — hook・進捗表示・
  edit-time コンパイルゲート・test-fix ループ 1 イテレーション・手動インストール（プラグインユーザーは不要）。

## module 責務

- **`model`** — decomposition スキーマの正典。`Decomposition`/`Task`/`Batch`/`Schedule`/`Class`（`Parallel`/
  `Serial`/`Gated`）/`Check`/`MechanicalCheck`/`HookInput`。`Task::requires_fp_oracle()`（`kind` 判定）。
- **`schedule`** — 決定論的スケジューラ。`files_conflict`/`entries_conflict`（保守的衝突判定）・`validate`・
  `schedule`（force-gate → depth layering → greedy graph coloring → **バッチ幅の cap 分割**）。
  file-overlap は `class` に対し権威。最後の段は引数 `max_parallel`（＝`Config::max_parallel`。
  `harness_core::parallel` のセッション同時実行上限で、既定かつ上限 3）でカラークラスを切り分ける —
  **partition であって truncation ではない**（幅の広いクラスは複数バッチになるだけで、タスクは 1 つも落ちない）。
  cap は**引数で渡す**（env を読むのはこの module ではなく `Config::load` 側。全関数が純関数という
  この module の契約を保つため）。渡された値は純粋な `harness_core::parallel::clamp` で再 clamp するので、
  0 も上限超えも通らない。
- **`state`** — 実行状態の永続化・完了ゲート・stale 修復。`RunState`/`Status`（`Pending`/`Running`/`Failed`/
  `Verified`/`Cancelled`/`Discarded`）・`gate_reasons`・`enforce_fp_gate`・reconcile/resume/record-run。最大 module。
  `TaskState` は cross-run 相関のための per-task identity として `hashkey: Option<String>`（claim registry と同じ
  opaque hashkey）と `claimed_at: Option<i64>`（占有した Unix 秒）を持つ（両方 `#[serde(default)]` で後方互換。
  未キーのタスク・旧レイアウトは fail-soft）。`detect_duplicate_completions` がこの2フィールドで §4.6c の
  重複完了検出を行う。
- **`policy` / `gate_exec` / `circuit` / `run_policy`** — graded-autonomy の決定コア。純関数
  `decide`（Level×3→`Decision`）・`decide_gate_exec`・`decide_circuit`・`decide_run_policy` と、その周囲の
  fail-soft な信号収集＋exit-code emit（`run_gate_check`/`run_circuit_check`）。
- **`claim`** — クロスセッション占有レジストリ（`claims.json`）。ファイル占有＋`task_claims`。heartbeat/TTL reap。
- **`verify` / `replan` / `consensus`** — verifier ステージ・reflux 分類・self-consistency 投票。決定論的整形/
  分類のみで、修正・再分解の判断は LLM に残す。`fence_worker_output`・`classify_criteria`・`canonical`（tier の
  token 境界一致）。
- **`worktree`** — git worktree ライフサイクル（repo 外・1 dir=1 branch 強制、`is_dirty`）。
- **`config`** — `Config`（`worktree_base`/`default_branch`/`max_parallel`/`shared_globs`/`autonomous`/
  `single_worktree`/`[test]`/`[consensus]`/`[worker]`/`[loop]`）を TOML から `load`。`CONDUKT_*` 環境変数で上書き。
- **`editgate`** — PostToolUse: worktree 内 Rust ファイル編集後の edit-time コンパイルゲート（broken 判定のみ block）。
- **`oracle` / `checkpoint` / `escalate` / `gatelog` / `lessons` / `pr` / `ci` / `lock` / `install` /
  `status` / `store` / `hooks`** — F→P オラクル判定・可逆スナップショット・非同期エスカレーション・decision
  journal・cross-task 学習 capture・gh PR・CI 連携・per-run ロック直列化・手動インストール・状態表示・入口。

## cross-task 学習の計測と post-execution diff-risk

> **REVIEW-NEEDED**: コードから逆算 (2026-07-10 セッション)。人間レビュー前は正典としない。

- **`learning_signal`** — 「retrieved lesson の注入は本当に replan を減らすか？」を機械計測可能にする
  **read-only な集計層**。既存の2台帳（`harness_core::retrieval` の run ごとの lessons ヒット有無、
  `state::load_replan_records` の per-run replan 記録）をいずれも `run_id` キーで join し、
  `LearningSignal`（lessons-hit 群と miss 群の replan 平均などの対比）を `aggregate`/`compute` で導く。
  元台帳は書き換えず追加もしない（純粋な read-side）。CLI 入口は `condukt learning-signal`
  （`Command::LearningSignal` → `learning_signal::compute`）。
- **`diffrisk_record`** — 実行後（post-execution）に worktree の **実 diff** を blastguard の
  public-API / 機微パス分類器（`blastguard::diffrisk::classify_diff`）へ通し、High-risk 判定を overwatch
  violation レジストリへ記録する（finding 4 / WorkItem-A）。狙いは、本番で dead になっていた
  public-API シグナル（`changes_public_symbol`）に実データ経路を与えること。タスクの実行完了報告時、
  すなわち `state set --status done` ハンドラ（`main.rs`）から `record_post_execution_diff_risk` として
  呼ばれる（実 diff が存在するのは実行後のこの時点のため。`schedule` は実行前の計画のみで実 diff を持たない）。
