> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# donegate 仕様

## 概要

`donegate` は、エージェントの「完了」宣言を*検証済み*の意味に変える完了検証ゲートである。Claude Code の
**Stop** フック（`hooks/hooks.json` が `${CLAUDE_PLUGIN_ROOT}/bin/donegate gate` を `Stop` に配線、timeout 600s）
に配線された決定論的な Rust バイナリで、停止のたびにプロジェクトの受け入れコマンド（`build`/`test`/`lint`/
`typecheck` 等の `[[check]]`）をサブプロセスとして実行し、必須チェックがすべてグリーンになるまでターンの終了を
許さない。失敗したチェックの出力末尾を会話へ差し戻し、エージェントを自動修正ループに入れる。役割は明確で、
実行して終了コードを読むのは donegate、失敗の修正（LLM 労働）は Claude Code のサブスクリプション内で行われる
（API キー不要）。README が位置づけるとおり、`precommit-audit`（静的）・`specguard`（LLM）に対する*動的*な
兄弟分＝「実際にビルドが通り、パスするか?」を実行して確かめるゲートである。

## 不変条件

- **never-break-a-turn（ハード不変条件）** — *harness* エラー（設定ミス・チェック未設定・donegate 自身のバグ）は
  常に exit 0 で停止を通す。意図的にブロックするのは実際に失敗した *check* だけ（`main.rs` doc-comment）。panic は
  `harness_core::gate::run::run_guarded` が hook モードで握り潰し exit 0、manual CLI モードでのみ exit 1 に浮上させる。
- **exit code はブロック手段ではない** — Stop フックでは JSON の `{"decision":"block","reason":…}` を stdout に出す
  ことで停止をブロックし、exit code 自体は常に 0（`gate_run`）。exit 1 を返すのは interactive（manual CLI）モードのみ。
- **default-safe（オプトイン）** — `[[check]]` が 1 つも無い、または `enabled=false` なら、ゲートは全停止を通す
  （`gate_run`：`!cfg.enabled || cfg.checks.is_empty()` で exit 0）。フックを入れただけでは決してブロックしない。
- **project config はトラストゲート付き** — project の `donegate.toml` が honor されるのは root が信頼済み
  （`harness_core::trust::is_trusted`）のときだけ。未信頼なら無視して home config（`~/.donegate/config.toml`）へ
  フォールバックし、無ければ built-in defaults（`Config::load`）。project の `cmd` は後で `sh -c` で実行されるため、
  敵対的リポジトリが Stop 時に任意コマンドを紛れ込ませるのを防ぐトラスト境界。`HARNESS_TRUST_ALL` は全体エスケープ。
- **fail-soft パース** — config の read/parse エラーは黙って built-in defaults へフォールバック（ターンを crash させない）。
  `git::changed_files` は非 git・git 失敗時に `None` を返し、その場合は全チェックを適用対象とする（`gate::applies`）。
- **無限ループ防止（試行カウンタ）** — セッションごとの連続ブロック回数を `state::bump` で数え、`max_attempts`（既定 3）を
  超えたら諦めて停止を通す（`giveup`、exit 0）。カウンタは `reset_after_secs`（既定 600s）アイドル、またはグリーン到達で
  `state::reset`。`session_key` が session を識別。
- **エスケープハッチ / キルスイッチ** — project root の `.donegate-skip`（1 行理由）は
  `harness_core::gate::run::consume_skip` で一度だけ消費され次の停止を通す（`skip`）。`DONEGATE_DISABLE=1`
  （`Config::disabled_env`、空/`0` は無効扱い）で完全停止。
- **config sanitize** — `max_attempts==0`→1、`default_timeout_secs==0`→300、`output_tail_lines==0`→40 に補正し、
  `name`/`cmd` が空白のみの check は除去（`Config::load`）。

## 振る舞い

サブコマンドは `clap` の `Command` enum（`main`）で定義。

- **`gate`（Stop フック本体）** — stdin から hook JSON（`session_id`/`cwd`/`stop_hook_active`、`HookInput::parse`）を
  読み、stdin が無ければ interactive（manual）モード。手順は `gate_run`：(1) `DONEGATE_DISABLE` チェック → (2) config
  load・enabled/checks 空チェック → (3) `.donegate-skip` 消費チェック → (4) `gate::evaluate` で `git::changed_files` に
  基づき適用チェックを実行 → (5) `all_green` なら reset+`green` log+exit 0、必須失敗があれば `bump` し
  `attempt>max_attempts` で `giveup`、そうでなければ `blocked` を log し `block_reason` を JSON `decision:block` で出力。
  各判定は `~/.donegate/state/log.jsonl` に JSONL 1 行追記（`verdict` は `green`/`blocked`/`giveup`/`skip`）、
  latency は `harness_core::hook_latency::record` に記録。
- **`init [--force]`** — プロジェクト種別を auto-detect（`Cargo.toml`→Rust / `package.json`→Node /
  `pyproject.toml`|`setup.py`|`requirements.txt`→Python / `go.mod`→Go / それ以外→generic make）して starter
  `./donegate.toml` を書く。既存時は `--force` 無しで bail。
- **`install [--dry-run]` / `uninstall [--dry-run]`** — `~/.claude/settings.json` の `Stop` フックへ
  `<bin> gate`（timeout 600s）をマージ／削除。冪等（既存 donegate group を strip してから再追加）、書込前に backup。
  機構は `harness_core::install`（MARKER=`donegate`）に委譲。
- **`status`** — 解決済み config の出典（trusted project / home / defaults）と、UNTRUSTED 警告、`enabled`/
  `max_attempts`/`state_dir`/checks 数、および現在の changed files に対して各 check が `when`/`always`/`[optional]`
  で走るかを表示。agent 非関与。
- **`trust`** — 現在の project root を `harness_core::trust::add` でワークスペース信頼リストに追加し、project-local
  `donegate.toml` の `[[check]]` を Stop 時に honor させる。

### module 責務

- **`main`** — CLI ディスパッチ、`gate` フックの制御フロー（`gate_command`/`gate_run`）、`init` の starter config 生成
  （stack 別テンプレート `RUST_CHECKS`/`NODE_CHECKS`/`PY_CHECKS`/`GO_CHECKS`/`GENERIC_CHECKS`）、`trust`/`status`/JSONL
  log（`log_event`）。ブロック判定・試行カウンタ・skip/disable の順序を保持する。
- **`config`** — `Config`（実効値）と `FileConfig`（全フィールド optional な on-disk 形）、`Check`（`name`/`cmd`/
  `when_changed`/`timeout_secs`/`optional`/`workdir`）を TOML から deserialize。`load` が project→home→defaults の
  レイヤリングとトラストゲート・sanitize を担う。`disabled_env`・`base_dir`・path helper。トラスト境界の要。
- **`gate`** — 適用判定と実行。`applies`（`when_changed` グロブ vs changed files、glob なし=常に適用、git なし=適用）、
  `evaluate`（適用 check を `runner::run_check` で実行し `Verdict{ran,skipped,git_unscoped}` を返す）、`Verdict`
  （`blocking`＝失敗した非 optional、`warnings`＝失敗した optional、`all_green`）、`block_reason`（モデル差戻し文＝
  失敗チェック＋出力末尾＋skip/disable の逃げ道案内）、`human_report`（manual 用）。
- **`runner`** — `harness_core::gate::runner` の薄いアダプタ。1 check をサブプロセス実行し `Outcome`（`passed`/
  `exit_code`/`timed_out`/`spawn_error`/`duration_secs`/`output_tail`/`optional`）へマップ。危険な spawn/timeout/
  bounded-tail は harness-core 側、ここは型変換・per-check log path・timeout フォールバック（`max(1)`）・`workdir` 解決のみ。
- **`git`** — 変更ファイル収集（`changed_files`＝`diff --name-only`＋`diff --cached --name-only`＋
  `ls-files --others --exclude-standard`、sort+dedup）。非 git は `None`（純 read-only な git サブプロセス呼び出し）。
- **`state`** — セッション別試行カウンタ。`harness_core::gate::state::{bump,reset}` の再エクスポートのみ（`last_hash` は未使用）。
- **`model`** — hook stdin ペイロード。`harness_core::hook::HookInput`（`parse`/`cwd_or_current`/`session_key`/
  `stop_hook_active`）の再エクスポートのみ。
- **`install`** — `~/.claude/settings.json` への Stop フック merge/remove。機構は `harness_core::install` に委譲、
  donegate 固有は MARKER・EVENT(`Stop`)・SUB(`gate`)・TIMEOUT(600s) の定数のみ。
