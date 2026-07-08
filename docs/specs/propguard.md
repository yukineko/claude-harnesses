> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# propguard 仕様

## 概要

`propguard` は Claude Code の **property gate**（プロパティゲート）である。`Stop` hook の
たびに現タスクの `done_criteria` から 3〜5 個の semantic property（意味的不変条件）を
**決定論的に導出**（`derive::derive_properties`）し、生成コードの diff がそれらを満たすかを
検査してからターン完了を許可する。PGS（Property-Generated Solver, arXiv:2506.18315）に倣い、
自由記述の done_criteria を小さな検査可能プロパティ集合へ変換する。`tdd`（具体テストが通るか）
に対する「意味的不変条件が保たれるか」の相補ゲート。subscription-native で、バイナリは
プロパティの *導出* と count→threshold のブロック判定のみを決定論的に担い、各プロパティが実際に
*成り立つか* の意味判断は inject モード（動いているエージェント自身）か subprocess モード
（設定した独立 `checker_cmd`）に委ねる。**API キー不要**。

## 不変条件

- **単一の閾値判定点** — ブロックは `gate::below_threshold(satisfied, threshold)` = `satisfied < threshold`
  の一点でのみ決まる。inject / subprocess 両モードがこの関数を通る。実効閾値は
  `effective_threshold`（`cfg.threshold` を導出プロパティ数へ `.min().max(1)` クランプ）で、
  恒久的に達成不能にならない（`config::sanitize` でも threshold=0→1、threshold>max_properties→max）。
- **fail-closed だが有界・脱出可能** — 検査前の環境エラー（git 無し／done_criteria 無し／
  checkable な変更無し／空 diff）は常に **許可**し勝手に指摘を作らない。checker の失敗
  （crash/timeout/解析不能出力）は無言許可せず **ブロック**（`CheckOutcome::Error`）するが、
  `max_attempts`（既定 2）連続で失敗すると警告して通過する（`checker-error-giveup`）ため
  壊れた checker がバイパス（無言通過）にも永久トラップにもならない。
- **truncated diff は握りつぶさない** — diff が `max_diff_bytes` を超え末尾が欠落した場合
  （`git::truncate_on_boundary` が `truncated=true`）は `decide_truncated` が bounded に
  ブロックし、`last_hash` を空のままにして未検査の tail を「検証済み」に certify しない。
  `max_attempts` 超過で警告のうえ通過。
- **never-break-a-turn** — `check_command` は常に exit 0 を Claude へ返す（ブロックは exit code
  ではなく JSON の `decision:"block"` フィールドで表現）。panic は
  `harness_core::gate::run::run_guarded` が握りつぶし exit 0 にする。
- **収束（同一 diff を再検査しない）** — `(diff, properties)` を `hash_props`（`DefaultHasher`）で
  ハッシュし、直前に検査を強制した停止と一致すれば `already-verified` で許可。inject モードでは
  新しい diff は未検証（`satisfied=0 < threshold`）なので一度ブロックし、エージェントが対応した後の
  同一 diff は許可される。
- **untrusted config は無視（RCE 防止）** — project `propguard.toml` は `harness_core::trust::is_trusted`
  が真のときのみ honored（`Config::load`）。その `checker_cmd` は Stop hook から subprocess 実行される
  ため、未 trust なら repo 同梱値を arbitrary code execution とみなし無視して home config→既定へ
  fall back する。TOML の parse エラーも黙って fall back（ゲートはターンを crash させない）。
- **subprocess 出力の fail-closed パース** — `parse_checker_output` は `PROP <id>: PASS` を明示した
  プロパティのみ satisfied に数える。導出プロパティ id を1つも含まない出力は「全 PASS」ではなく
  `Error`（fail closed）とする。
- **導出の決定論と上限** — `CATALOG` 順を保存し、キーワード一致プロパティを先頭に、`min_properties`
  未満なら `universal`（`error-path`/`output-schema`/`determinism` の3つ）で補完、`max_properties`
  （上限 5）で `truncate`。同一入力は同一出力。

## 振る舞い

サブコマンドは `clap` の `Command` enum（`main`）。

- **`check`（Stop hook）** — `read_stdin`→`HookInput::parse`。`PROPGUARD_DISABLE` / config `enabled=false` /
  `.propguard-skip`（1回限り・理由1行、`consume_skip`）で早期許可。それ以外は `gate::evaluate`→
  `state::save`/`reset`→`log_event`（JSONL 1行）。非対話時 block は `{"decision":"block","reason":…}` を
  stdout へ、対話時は stderr。`harness_core::hook_latency::record` で latency 記録。
- **`gate::evaluate`（コア）** — (1) `source_criteria` で done_criteria 解決（無→`no-criteria` 許可）、
  (2) `derive_properties`（空→`no-properties` 許可）、(3) `git::changed_files`（None→`no-git` 許可）→
  `checkable_files`（`min_changed_files` 未満→`no-code-changes` 許可）→`git::diff_text`（空→`empty-diff` 許可）、
  (4) idle gap（`reset_after_secs`）超過で attempt カウンタ reset、(5) truncated ガード、(6) hash 一致で
  `already-verified`、(7) モード分岐 →`decide_from_count`。
- **`derive <criteria>`** — オフラインで done_criteria から導出プロパティ・既定 threshold を表示（判定なし）。
- **`status`** — 解決済み config（source path / mode / threshold 等）と現 cwd/タスクの導出プロパティを表示。
- **`install [--dry-run]` / `uninstall [--dry-run]`** — `~/.claude/settings.json` の Stop hook を冪等に
  merge/remove（`harness_core::install`、既存 propguard グループを strip してから再追加、書込前バックアップ）。
- **`init [--force]`** — 雛形 `./propguard.toml`（`STARTER`）を生成。既存時は `--force` 必須。
- **`trust`** — 現 project root を `harness_core::trust::add` で trust 登録し、以後 `propguard.toml` を honor。

done_criteria の取得元（`source_criteria`、優先順）: (1) 環境変数 `PROPGUARD_CRITERIA`、
(2) project root の `criteria_file`（既定 `.propguard-criteria`。condukt/エージェントが書き出す）、
(3) `propguard.toml` の inline `done_criteria`。いずれも無ければ全停止を許可。

## module 責務

- **`config`** — `Config`/`FileConfig`（全 field optional・`deny_unknown_fields` 相当は無し）・`Mode`
  （`Inject`/`Subprocess`, `Mode::parse`）。project（trust 済みのみ）→home→既定の3層 load、`sanitize` で
  全ノブを非トラップ範囲へクランプ。`DEFAULT_CRITERIA_FILE`、`default_include`/`default_exclude`
  （ソース拡張子・lockfile/vendored/generated グロブ）、`disabled_env`。
- **`derive`** — done_criteria→プロパティの決定論導出。`Property`（`id`/`title`/`check_hint`/`keywords`/
  `universal`）、bilingual キーワード `CATALOG`（6件）、`derive_properties`（一致→padding→truncate）、
  `source_criteria`（3段 fallback）。プロパティが *成り立つか* の意味判断は導出しない（delegate）。
- **`gate`** — ゲート本体。`below_threshold`（唯一の閾値点）、`Decision`（`Allow`/`Block`）、
  `CheckOutcome`（`Verified{satisfied,findings}`/`Error`）、`evaluate`、`decide_from_count`
  （閾値 enforcement＋`max_attempts` bounded）、`decide_truncated`、`checkable_files`、`run_checker`
  （subprocess 起動・`wait_timeout`）、`parse_checker_output`（fail-closed）、`build_command`
  （metachar 検出時のみ shell 経由）、各 block reason（日本語、脱出口を明示）。
- **`git`** — `git` への read-only subprocess のみ。`changed_files`（diff/diff --cached/ls-files --others
  を union、None=非 git repo）、`diff_text`（unstaged+staged hunks＋untracked ファイル全文、`max_bytes`
  で truncate）、`truncate_on_boundary`（char boundary 尊重、`DiffText{text,truncated}`）。
- **`install`** — Stop hook の merge/remove。`harness_core::install` に委譲、marker=`"propguard"`、冪等。
- **`model`** — `harness_core::hook::HookInput` の re-export（`parse`/`cwd_or_current`/`session_key`）。
- **`state`** — `harness_core::gate::state`（`SessionState{attempts,last_hash,last_ts}`・`load`/`save`/`reset`）
  の re-export。round カウンタは `gate::evaluate` がインラインで駆動する。
