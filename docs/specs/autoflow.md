> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# autoflow 仕様

## 概要

`autoflow` は「やり残しを抱えたままセッションが終わる」のを防ぐ session-end auto-flow ゲートである。3 つの
Claude Code フック（`Stop`/`PreCompact`/`UserPromptSubmit`）と同梱 Rust バイナリだけで動く
subscription-native 設計で、API キーもデーモンも不要。中核は **Stop** フックのセッションごとの状態機械
（`state::Phase::{Idle,RecordRequested,Continuing,Done}`）で、ターン完了時に record→condukt の連鎖を
順に検査し、片付いていない仕事があれば `{"decision":"block","reason":…}`（`main::block`）でターン終了を
ブロックし `/` コマンドへ誘導する。判定（どう作業するか）は各 skill と LLM に委ね、autoflow は「終わらせて
よいか」のゲートだけを決定論的に担う。`PreCompact`+`UserPromptSubmit` の組は `/compact` を跨いで flow
ループを一度だけ再開させる。

**backlog キューはこの状態機械の対象ではない（0.1.22 でユーザーの指示により撤去）。** 0.1.21 までは
(a) `SessionStart` が backlog を確認して「バックログに N 件（最優先 …）。/flow で開始しますか？」を注入し、
(b) Stop の condukt pending が空になった後さらに backlog を読んで「/backlog を実行してください」で
**毎ターン block** していた。撤去理由は「繰り返しは検出ではない」— 誰も着手していないキュー項目について
毎ターン尋ねることは、1 回目に無い情報を何も足さない。condukt arm は残る: 本セッションが起こして
放置した in-flight run は事故だが、未着手のキュー項目は事故ではない。

## 不変条件

- **ゲートは block 判定のみ・作業しない** — autoflow 自身は record も condukt も実行せず、`stop_command` は
  `println!` で `{"decision":"block","reason":…}` JSON を出すだけ。実作業は誘導先の skill/LLM が行う。
- **停止は progress ベース（回数プロキシではない）** — condukt ループの継続は
  「残件集合（pending/failed）が縮んでいるか＝進捗の有無」で決まる（`state::decide_progress`）。進捗がある限り
  累積回数に関係なく block（継続）し、進捗が止まった `no_progress_streak` が `cfg.stuck_threshold`（既定 3）に
  達した stuck のときだけ、文面を escalation に切り替えて block する。**stuck でも黙って allow/`Phase::Done` に
  はしない**（見える形で必ず block）。`Phase::Done`（唯一の正当な停止）へ落ちるのは残件集合が空＝タスク完了に
  なったときのみ — その「残件集合」は 0.1.21 まで「condukt pending が空 **かつ** backlog open が空」を
  意味したが、0.1.22 以降は「この condukt run に pending タスクが無い」である。**前提が変わっても観測に
  依拠する点は変わらない**: `condukt::find_pending` の `Determination::Undetermined`（run ファイルが
  読めない・パースできない）は今も block へ倒れるので、`Done` へは「見なかったから」到達できない。
  stuck の文面は `is_autonomous()`（`condukt state autonomy-check` を shell、失敗時は
  非自律に fail-safe）で分岐し、自律時は out-of-band 対処を促し、非自律時はユーザー確認を促す。進捗中の継続は
  両モードとも確認を出さない。
- **他セッションが lock を握れば撤退** — `lock::backlog_driver_active()`（`~/.backlog/run.lock` を read-only 参照、
  owner pid が生きていれば active）が真なら `stop_command` は即 return。稼働中の `/flow`/`/backlog` driver と
  同一 queue を二重駆動しない。stale lock（pid 消滅）は inactive 扱いで wedge しない。
- **compass を参照しない** — 0.1.21 までは backlog を auto-drive する前に `compass::charter_freshness`
  （`compass nudge --json` を shell）で charter の鮮度を見ていた。そのゲートは backlog の auto-drive を
  守るためだけに存在したので、drive の撤去と同時に `compass` モジュールごと削除した（消費者のいない
  キュー読み・鮮度読みを残すと、次の著者が再配線する誘因になる）。
- **fail-soft・ターンを壊さない** — 全フックは `harness_core::hook::run_hook`（panic を捕捉）配下で動く。
  session_id が空（`input.session_id` も `CLAUDE_CODE_SESSION_ID` env も無い）なら何もせず return。
  `Config::load` は config 欠落・パース失敗でも既定値で成立する。`SessionStart` は保留 0 かつ charter fresh なら
  無出力（ターンを壊さない）。
- **resume マーカーは自セッション所有時のみ・厳密に一度** — `pre_compact_run` は (a) `resume_flow_on_compact`
  が真、かつ (b) `lock::this_session_holds_lock(session_id)`（lock の `session_id` が現セッションと一致）の
  両成立時のみ marker を書く。空セッション・lock 欠落・別セッション・legacy lock は全て書かない。
  `prompt_submit_run` は `state::consume_resume_marker`（`remove_file` の成否で TOCTOU 無く消費）で marker を
  削除しつつ 1 回だけ RESUME_FLOW_INJECT を注入する。PreCompact は compaction をブロックしない。
- **path traversal 防御** — session_id 由来のパスは全て `harness_core::store::safe_session` で単一コンポーネントに
  sanitise（`state::state_path`/`resume_marker_path`, `insights::load_metrics`）。
- **無効化** — `Config::enabled == false` または `AUTOFLOW_DISABLE=1`（`Config::disabled_env`）で Stop/PreCompact/
  PromptSubmit のゲートは全て no-op。

## 振る舞い

サブコマンドは `clap` の `Command` enum（`Stop`/`PreCompact`/`PromptSubmit`）。`session-start` は
0.1.22 で廃止され、launcher の `session-start)` 分岐も消えている（引数は catch-all の `exit 1` に落ちる —
廃止した hook 名に成功したように見える応答を返さないため）。

- **`stop`（Stop フック）** — 状態機械を 1 ステップ進める。`Phase::Idle`: `insights::load_metrics`
  （`~/.session-insights/state/<sid>.json` の `turns`/`tool_events`）が `min_turns`(既定2)・`min_tool_events`(既定3)
  以上なら `RecordRequested` に遷移し `/session-insights:record` を block。`RecordRequested|Continuing`:
  `condukt::find_pending`（下記）が非空なら `condukt_prompts` を増やし該当タスクを `mark_running` して
  `/condukt` を block。空なら `Done`（唯一の正当な停止）へ落として latch。`Phase::Done`: 無操作。
- **`pre-compact`（PreCompact フック）** — `pre_compact_run` を呼ぶだけ（上記不変条件のゲート）。marker を書くか黙るか。
- **`prompt-submit`（UserPromptSubmit フック）** — `prompt_submit_run` が `Some(msg)` を返せば `println!`（stdout が
  UserPromptSubmit の注入チャネル）。通常ターンは無出力。

補助ロジック:

- **`condukt::find_pending`** — `~/.condukt/state/<project_key>/` の最新 `run-*.json`（`latest_run_file`）を読み、
  `status` が `pending`/`failed` の `TaskState` を返す。返す前に `running` かつ `updated_at` が `STUCK_SECS`(7200秒=2h)
  超のタスクを `pending` に revert（中断とみなす）。`mark_running` は指定 id を `running`＋現時刻に更新。
  永続化は tmp→rename の atomic write（`save_run`）で、失敗は stderr に出しつつ黙って握らない。
  project key は `harness_core::projkey`（condukt と同一 source of truth）で導出。
- **`backlog` モジュール（キューは読まない）** — `find_open` と `BacklogItem` は 0.1.22 で削除した。残るのは
  `find_backlog_binary`（PATH → plugin cache の順）と `repo_project_path`（repo root の canonical 絶対パス）の
  2 関数だけで、これは `lock::backlog_driver_active` が「他セッションの driver が動いているか」を見るために
  使う。**撤退（stand-down）はキューの nudge ではない**ので残す。

### module 責務

- **`config`** — `Config`（`enabled`/`min_turns`/`min_tool_events`/`state_dir`/`stuck_threshold`/
  `resume_flow_on_compact`）を `base_dir("autoflow")/config.toml` から `FileConfig`（全 Option）で上書き読込。
  既定値と `disabled_env`（`AUTOFLOW_DISABLE=1`）を提供。
- **`state`** — `Phase` enum・`StopDecision`・`decide_progress`・`SessionState`
  （`phase`/`condukt_prev_pending`/`condukt_no_progress_streak`/`delegation_audit_warned`）の永続化
  （`harness_core::store::{load_json,save_json}`）と resume-flow marker（`resume_marker_path`/`write_resume_marker`/
  `consume_resume_marker`）。全パスは `safe_session` で sanitise。
- **`lock`** — backlog run lock（`~/.backlog/run.lock`）の read-only ビュー。`backlog_driver_active`（別プロセスが
  live で握るか、`pid_alive` は Linux で `/proc/<pid>`・fallback `kill -0`）と `this_session_holds_lock`
  （lock の `session_id` が現セッションと一致するか）。autoflow は lock を取得しない。
- **`condukt`** — condukt 最新 run の pending/failed タスク検出・stuck revert・running マーク（上記）。
- **`backlog`** — `backlog` バイナリの発見と repo project path の導出のみ（`lock` 用。キューは読まない・上記）。
  `compass` モジュールは 0.1.22 で削除（唯一の呼び出し元が撤去した backlog auto-drive ゲートだった）。
- **`insights`** — session-insights の `SessionMetrics{turns,tool_events}` を state ファイルから読む
  （record プロンプト発火の閾値判定材料）。
