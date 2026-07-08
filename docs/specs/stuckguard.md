> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# stuckguard 仕様

## 概要

`stuckguard` は、エージェントが同じ失敗を繰り返す／編集を行ったり来たりして収束しない「行き詰まりループ」を
決定論的に検出し、アプローチ変更やユーザーへのエスカレーションを促す **PostToolUse** フックである。判定に LLM を
使わず、ツール呼び出しの履歴パターンだけから機械的にループを見抜くため API キーは不要（subscription-native）。
主コマンド `watch` は各ツール呼び出しごとに、呼び出しを安定した**シグネチャ**（`sig::build`）へ正規化し、
セッションごとの**リングバッファ**（`state::SessionState`、`window` 件）へ追記し、その窓に対して 2 種の検出器
（`detect::detect`）を走らせる。発火時は `additionalContext` 経由で助言（`message`）を1行注入し、同一パターンが
`escalate_after` 回ナッジされると「止めてユーザーに尋ねよ」へ**昇格**する。stuckguard は**助言を注入するのみ**で、
ツール呼び出しのブロックやターンの終了はできない。ゆえに誤検出のコストは余計な1行の context にとどまる。

## 不変条件

- **advise-only（ブロック不可）** — `watch` の出力は `hookSpecificOutput.additionalContext` の文字列注入だけであり、
  deny/block/stop の decision を返さない。ツール呼び出しや turn を止める手段を持たない（`main::watch` 参照）。
- **fail-soft** — stdin が `HookInput::parse` できない、`sig::build` がシグネチャを作れない（ツール名が空等）、
  config が読めない、いずれの場合も `watch` は何も出さず静かに return する。`STUCKGUARD_DISABLE`（`Config::disabled_env`、
  空でも `0` でもない値）で即無効化。`cfg.enabled=false` や ignore 対象ツール（`Config::is_ignored`、既定 `["TodoWrite"]`）も
  return。
- **決定論** — シグネチャは `DefaultHasher`（固定シード）でハッシュし、プロセスをまたいで同一入力→同一 `sig`。
  `sig::norm` が空白ランを単一スペースに畳み trim するので、`cargo  test` と `cargo test` は衝突する。crypto 強度ではなく
  衝突等価性のみが目的。
- **検出は入力から** — 両検出器は主にツール**入力**から計算し、脆い結果パースに依存しない（`error` 判定のみ
  `sig::looks_error` が response の best-effort）。
- **oscillation が repeat に優先** — `detect::detect` は oscillation を先に評価し、成立すればそれを返す。両立時は
  oscillation を採用。
- **クールダウンと昇格の bookkeeping** — 同一パターン key は `cooldown_events` 以内では再ナッジしない
  （`SessionState::in_cooldown`、`saturating_sub`）。ナッジ回数は per-key に累積（`record_nudge`）し、`escalate_after`
  到達で escalated。
- **状態はローカルかつ per-session** — 状態は `~/.stuckguard/state/`（`Config::state_dir`）配下。セッションファイルは
  `sessions/<safe(session)>.json`（`state::safe` が英数・`-`・`_` 以外を `-` に）。ナッジ1件につき `log.jsonl` に JSONL 1行
  追記（unix では mode `0o600`、`main::log_event`。テスト `log_event_creates_file_with_0o600` が保証）。
- **ring buffer は window 件で prune** — `SessionState::push` は seq を採番し、`events.len() > window` なら先頭から
  drain して直近 `window` 件だけ保持。config は `window>=2` / `repeat_threshold>=2` / `oscillation_threshold>=1` /
  `escalate_after>=1` に sanitize（`Config::load`）。

## 振る舞い

サブコマンドは clap の `Command` enum（`main`）。

- **`watch`（PostToolUse フック）** — 上記パイプライン。stdin の `HookInput` を読み、`cwd_or_current` で root 解決 →
  `Config::load` → `sig::build` で Event 生成 → `state::load`/`push` → `detect::detect` → 非クールダウンなら
  `record_nudge` + `log_event` + `message` を `additionalContext` に出力 → `state::save`。exit code は常に 0（`run_hook` 経由）。
- **`install [--dry-run]`** — `~/.claude/settings.json` に PostToolUse フックをマージ（`install::install`、
  `harness_core::install` 経由で backup→write）。matcher は `Bash|Edit|MultiEdit|Write|Read|Grep|Glob`、timeout 10s。
  冪等（既存の `stuckguard` group を `MARKERS` で strip して置換）。
- **`uninstall [--dry-run]`** — 自身の hook group を settings から除去し件数を報告（`install::uninstall`）。
- **`init [--force]`** — 雛形 `./stuckguard.toml`（`STARTER`）を書き出す。既存かつ `--force` 無しなら bail。
- **`status`** — 解決済み config（採用ファイル・各閾値・`state_dir`）を表示。

検出器（`detect.rs`）:

- **repeat** — 窓内で現行イベントと同一 `sig` の件数が `repeat_threshold` 以上で発火。key は `repeat:<sig>`。
  対象が全て `error=true` なら `all_errored` を立て、メッセージに「（毎回失敗しています）」を付す。
- **oscillation** — 現行編集対象ファイルの編集列（`old_h`/`new_h` を持つもの）で、後発編集の `(old_h,new_h)` が先行編集の
  `(new_h,old_h)` を反転する「revert thrash」を数え、`reversals >= oscillation_threshold` で発火。key は `osc:<file>`。
  各後発編集は最大1反転として計上（`break`）。

シグネチャ生成（`sig::build`）はツール別: `Bash`=正規化 command、`Edit`/`MultiEdit`=`file_path`+old/new（file と
old_h/new_h も抽出）、`Write`=file+content（old_h は空文字ハッシュ＝全置換扱い）、`Read`/`Grep`/`Glob`=各キー、
その他はフォールバックで input 全体を JSON 化。`looks_error` は response の `is_error=true` / 非 null `error` /
`exit_code!=0` を失敗とみなす。

## module 責務

- **`main`** — clap ディスパッチ、`watch` パイプライン、`message`（非昇格＝別アプローチ提案 / 昇格＝ツール停止して
  ユーザーへ報告・指示要請）、`log_event`（JSONL 追記、0o600）、`status`/`init`。
- **`config`** — `Config`（`enabled`/`window`/`repeat_threshold`/`oscillation_threshold`/`cooldown_events`/
  `escalate_after`/`ignore_tools`/`state_dir`）を project `stuckguard.toml` > `~/.stuckguard/config.toml` > 既定の順で解決し
  sanitize。`disabled_env`/`is_ignored`/`base_dir`。
- **`sig`** — ツール呼び出し → 安定 `Event`（`seq`/`tool`/`sig`/`file`/`old_h`/`new_h`/`error`）。`hash`（DefaultHasher）/
  `norm`/`looks_error`。edit 系のみ file+before/after ハッシュを載せ thrash 検出に供する。
- **`detect`** — 窓に対する 2 検出器（`repeat`/`oscillation`）と `Trip`（`key`/`kind`/`count`/`all_errored`/`detail`）。
  純粋関数。oscillation 優先。
- **`state`** — per-session リングバッファ + per-pattern ナッジ台帳の永続化（`SessionState`/`Nudge`、
  `harness_core::store` の JSON load/save）。`push`/`record_nudge`/`in_cooldown`/`safe`/`path`。
- **`install`** — settings.json への hook merge/remove（`harness_core::install` 委譲、冪等・backup 付き）。
- **`model`** — `harness_core::hook::HookInput` の re-export（薄いエイリアス。独自ロジック無し）。
