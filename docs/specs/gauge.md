> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# gauge 仕様

## 概要

`gauge` は Claude Code 向けのローカル LLMOps テレメトリ・ハーネスである。フックは 1 本・仕事は 1 つ —
毎ターン（Stop）ごとにセッションのトランスクリプト（JSONL）を読み直し、累積のトークン使用量・キャッシュ
ヒット・ツール呼び出し・レイテンシ・推定コストをローカルストアに記録する（`main.rs` doc-comment）。
記録はセッションごとに 1 レコード（`<state_dir>/sessions/<session_id>.json`）として毎ターン上書きされる。
その後 `gauge report` がプロジェクト・モデル・日付・エージェント（main vs sub-agent）別に集計し、
`gauge subagents` はサブエージェント（Task）ごとのトランスクリプトからコストを個別按分する。
subscription-native（フック＋同梱バイナリのみ、API キー不要、マシンの外にデータは出ない）。集計の実体
（トランスクリプト推定・store・pricing・HookInput）は `harness_core` に委譲し、gauge は書き込み経路
（Stop hook）と CLI/レポート表示を owns する。

## 不変条件

- **観測のみ（fail-soft ハード不変条件）** — `record` は `harness_core::hook::run_hook` 配下で走り、panic を
  捕捉して常に exit 0。`Config::disabled_env`（`GAUGE_DISABLE` が非空かつ `"0"` 以外）、`HookInput::parse`
  失敗、`cfg.enabled=false`、`estimate_transcript_cost` の `None` のいずれでも早期 return し、不正 stdin・
  トランスクリプト欠落・ストア書き込み失敗があっても「その分を記録しないだけ」でターンを壊さない。
- **単一推定器で source を統一** — トランスクリプトは共有の `harness_core::estimate_transcript_cost` を通す。
  gauge/budgetguard/session-insights が同一ソースで集計するため数値が drift しない。
- **コストは read 時に再計算（persist しない）** — レコードには token 集計のみを保存し、コストは保存済み
  token 数から `harness_core::pricing::{cost, session_cost}` でレポート/表示のたびに再計算する（`report.rs`
  doc-comment）。ゆえに pricing 表を編集すれば過去分も再評価される。
- **pricing の決定性** — 組み込みレート（USD/1M, in/out）: Opus 5/25・Sonnet 3/15・Haiku 1/5・Fable/Mythos
  10/50。キャッシュ書き込みは input の 1.25倍(5m)/2倍(1h)、読み込みは 0.1倍。`pattern` は model id への部分一致で
  最初の一致が勝つ。認識できないモデルの寄与は 0。
- **設定はレイヤ非マージ・最初の 1 ファイルが勝つ** — `./gauge.toml` > `~/.gauge/config.toml` > 組み込み
  デフォルトの順で、存在した最初のファイルのみを採用する（`Config::load`）。
- **install の非破壊性** — `install`/`uninstall` は `harness_core::install` 経由で settings.json を load→backup→
  write し、`MARKERS=["gauge"]` に一致する group のみ操作、他プラグインの hook group は保持する（冪等）。

## 振る舞い

サブコマンドは `clap` の `Command` enum（`main.rs`）で定義する。

- **`record`（Stop hook）** — stdin の `HookInput` を parse → `cwd_or_current` で root 解決 → `Config::load` →
  `estimate_transcript_cost` で aggregate 化 → `SessionRecord::from_aggregate`（session_id/project/cwd/aggregate/
  `track_tools`/現在時刻）を作り `store::upsert` で毎ターン上書き保存。`GAUGE_DISABLE` と `enabled` で無効化可。
- **`report [--project <s>] [--since <YYYY-MM-DD>]`** — 全レコードを `store::load_all` で読み、`--project` は
  project 名の部分一致（小文字化）、`--since` は `day()`（`last_ts` 由来）で絞り込み、`report::render` が
  合計＋プロジェクト別（上位15）・モデル別・エージェント別・日別（直近14日）の内訳を出力。
- **`session [--json] [--session <ID>]`** — 直近更新（`updated_at` 最大）または指定 ID のレコードを表示。人間
  向けは models/agents/tools 内訳＋duration（`first_ts`〜`last_ts`）。`--json` は `{session_id, cost_usd,
  models, agents}` を出す（レコード無しは `null` / `no sessions recorded yet.`）。
- **`subagents [--json] [--session <ID>]`** — `session` がサブエージェントを 1 バケットに束ねるのと異なり、
  `<session>/subagents/agent-<id>.jsonl` を **live で読み**（`usage::subagent_usage`）各 Task 呼び出しに
  コストを按分する。`--json` は `[{agent_id, agent_type, description, cost_usd, turns}]`。呼び出し元（condukt 等）は
  Task の sidecar に記録された `description` で相関し、タスク単位の実コストを記録できる。トランスクリプト解決は
  `find_transcript`（`~/.claude/projects/*/<id>.jsonl`、ID 無しは全 project dir で最新 mtime の `*.jsonl`）。
- **`status`** — 解決済み config source・enabled・track_tools・state_dir・記録セッション数・合計コスト・pricing
  override 件数を表示。
- **`install|uninstall [--dry-run]`** — スタンドアロン（`cargo install`）用に Stop hook を settings.json へ
  merge/remove。プラグイン経路では代わりに `hooks/hooks.json`（`${CLAUDE_PLUGIN_ROOT}/bin/gauge record`,
  timeout 10）が使われる。
- **`init [--force]`** — スターター `./gauge.toml`（`STARTER` 定数）を書き出す。既存かつ `--force` 無しは Err。
- **`config set-window --hours <f64> --last-reset <RFC3339>` / `config show`** — アカウントのレートリミット窓
  （長さ＋直近リセット時刻）を人間が手動登録する（`window.rs`）。残り時間を取得する API が存在しないため
  自動検出不可。`~/.gauge/store` 配下の `window.json` に永続化し、`set-window` は上書き、`show` は現在値を
  表示（未設定は「no window registered」）。`approx_reset_in_secs` が登録値から次リセットまでの概算秒数を
  算出するが、リセット自体は観測できず登録済み窓長からの推測に過ぎない。未設定時は既存の
  `first_ts`/`last_ts` ベースの連続稼働時間表示にフォールバックする（fail-soft・エラーにしない）。
  `condukt shadow-run`（同じく自動発火を持たない投機実行モード）をいつ有効化するかの目安として使う。

### module 責務

- **`main`** — CLI（`Cli`/`Command`）ディスパッチ、`record_hook`（書き込み経路の本体）、`report_cmd`/
  `session_cmd`/`subagents_cmd`/`status`/`init`、トランスクリプト解決 `find_transcript`、duration 整形
  （`duration_secs`/`fmt_duration`）、`STARTER` テンプレート。
- **`config`** — `Config`（enabled/track_tools/state_dir/pricing）と `FileConfig`/`FilePrice` の TOML
  deserialize、レイヤ解決（`load`/`config_source`/`project_path`/`home_path`）、`disabled_env`、`base_dir`。
  `PriceOverride`/`expand_tilde` は `harness_core` から re-export。
- **`report`** — レコード群を人間向けレポート文字列へ集計（`render`）。整形ヘルパ `commas`/`money`/
  `tokens_short`/`pct`/`truncate`、集計ヘルパ `cache_hit_rate`（`cache_read /(input+cache_read)`、0 除算ガード）・
  `cache_write`（5m+1h）・`record_cost`。日本語ラベル。
- **`install`** — スタンドアロン用に Stop hook を settings.json へ merge/remove。設定ファイル機構
  （load/backup/write/strip/group 一致）は `harness_core::install` に委譲、gauge は `MARKERS=["gauge"]` と
  `EVENTS=[("Stop","record")]`・`TIMEOUT_SECS=10` を owns。
- **`model`（薄い再export）** — `harness_core::hook::HookInput` を re-export するのみ。全プラグインで parse 契約を共有。
- **`store`（薄い再export）** — `harness_core::session::{load_all, load_one, upsert, SessionRecord, Usage}` を
  re-export するのみ。レコード型と read/write は共有基盤にあり、gauge は Stop hook で write 経路を owns する。
