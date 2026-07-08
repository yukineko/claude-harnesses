> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# replaykit 仕様

## 概要

`replaykit` は tracekit が記録した condukt 実行トレースを、evalkit が消費できる golden replay
ケースへ蒸留する回帰テストハーネスである。curate（playbook→golden）の兄弟で、curate が *playbook* を
golden に変えるのに対し replaykit は *実行トレース* を golden に変える。1 実行の span
（`~/.tracekit/<sanitize(run_id)>/spans.jsonl`）を、`end_unix_ms` 昇順の `steps` と、その phase 集合・
エラー件数・総コストを固定した `expect` ブロックからなる移植可能な `TrajectorySummary` へ蒸留する。
`verify` は committed fixture の集計値を steps から**再計算**して `expect` と照合するため、後の変更で
steps が drift（新エラー・コスト暴騰・phase 欠落）すると golden が失敗し、回帰が CI に表面化する。
サブスクリプションネイティブ（std + serde + clap の単一バイナリ、API キー・ネットワーク不要）で、
ライフサイクル hook ではなく人間 / CI が直接叩く**素の CLI** である。skills / hooks / agents は同梱しない。

## 不変条件

- **exit code は evalkit / trajectoryeval と同じ 0/1/2 ゲート方針** — `0` = 固定不変条件と一致（pass）、
  `1` = 本物の回帰 / 不変条件違反、`2` = ハーネスエラー（入力の欠落・読み取り不能・不正）。`main.rs` の
  doc-comment が「plain CLI gate であり lifecycle hook ではない、real error は exit 2 に出す」と宣言する。
- **`expect` を信用せず再計算する（検証の核）** — `verify::verify` は fixture 内の `expect` の帳簿を
  そのまま読まず、`TrajectorySummary::{phases,error_count,total_cost_usd}` で steps から再導出して照合する。
  よって fixture は集計ロジック自体の自己テストになる（静的スナップショット読み出しではない）。
- **span モデルは harness_core が単一正典** — `trace.rs` は `harness_core::spans::{load_from as load_spans, Span}`
  を re-export するのみ。tracekit（span を *書く*）と replaykit（*読む*）が同じ `Span` schema を共有し、
  byte-for-byte で round-trip する。error 判定は `Span::is_error`（status が大小無視で `ok`/`verified` の
  いずれでもなければ error。空・`failed`・`error`・`timeout` は error）で、`Step::is_error` はその写し。
- **fail-soft なローダ** — `load_spans` は壊れた JSONL 行をスキップして件数を stderr に報告し（空行は
  malformed に数えない）、ファイル欠落のみ IO error として exit 2 に写す。
- **決定論** — steps は `end_unix_ms` の**安定ソート**で並び、同一 timestamp は記録順を保つ。`phases` は
  first-seen 順の distinct 集合。`slug_id` は curate と byte-for-byte 一致の ASCII slug + 6 桁 hash suffix
  で、非 ASCII でも安定・衝突耐性を持つ。run id の on-disk 解決は `sanitize`（`[A-Za-z0-9_-]` 以外→`_`）。
- **コスト cap は観測時のみ pin** — `total_cost_usd` は cost を持つ step が 1 つも無ければ `None` を返し、
  値付けされなかった run には cap を pin しない。cap 比較には `COST_EPSILON`(1e-9) を足し IEEE-754 の
  加算 drift（0.1+0.2≠0.3）を吸収するので、不変な run が自身の pin した cap を踏まない。
- **golden の移植性** — `promote` が書く golden の `cmd` は `["replaykit","verify",<root 相対 fixture パス>]`。
  fixture を root 相対で参照するため、committed golden は root の位置に依らず repo と共に移動する。
- **promote は id で冪等** — dataset に同じ id の golden 行が既にあれば skip して exit 0（`existing_ids`）。

## 振る舞い

サブコマンドは clap の `Command` enum で定義（`extract` / `verify` / `promote`）。

- **`extract --run <RID> [--spans <path>] [--out <path|->]`** — span をロード（`--spans` 指定、無ければ
  `default_spans_path`）→ `TrajectorySummary::from_spans` で要約を組み立て → 整形 JSON を `--out`
  （既定 `-` = stdout）へ出力（`cmd_extract`）。span 数・エラー数・総 ms を stderr に報告。
- **`verify <fixture.json>`** — committed fixture を読み `TrajectorySummary` に deserialize →
  `verify::verify` で phase 集合（完全一致）・エラー件数（`max_error_count` 以下）・総コスト
  （cap があれば `+ε` 以内）を照合（`cmd_verify`）。pass=exit 0、違反=全項目を stderr 列挙して exit 1、
  read/parse 失敗=exit 2。
- **`promote --run <RID> [--spans <path>] [--root <dir>] [--evals-dir <name>] [--dataset <name>] [--draft]`** —
  要約を組み立て、`<root>/<evals_dir>/replay/fixtures/<id>.json` に整形 fixture を書き（`write_fixture`）、
  golden 行を `<root>/<evals_dir>/replay/<sanitize(dataset)>.jsonl` に追記する（`append_golden`、id で
  重複排除）。既 promote 済みは skip exit 0、write 失敗は exit 2。`--draft` は curate との parity のため
  予約された **no-op フラグ**（`let _ = a.draft;` で明示的に無視。現状は何もしない）。

## module 責務

- **`main`** — CLI 定義（clap）・3 サブコマンドの dispatch・exit code 決定。path/IO helper
  （`default_spans_path`/`sanitize`/`existing_ids`/`write_fixture`/`append_golden`）を持つ。
- **`trace`** — tracekit span モデルの薄い再エクスポート層。`harness_core::spans::{load_from,Span}` を
  `load_spans`/`Span` として公開するのみ（型・ローダの実体は harness_core が所有）。
- **`summary`** — committed fixture 形式 `TrajectorySummary`（`run_id`/`steps`/`expect`）と `Step`/`Expect`
  を定義。`from_spans`（steps 安定ソート + expect 導出）と再計算アクセサ `phases`/`error_count`/
  `total_cost_usd`/`total_ms` を提供する。
- **`verify`** — `verify(summary) -> Result<(),Vec<String>>`。steps から再導出した集計を `expect` と照合し、
  違反した不変条件を全件返す（一度に全部レポートできるよう）。`COST_EPSILON` を保持。
- **`promote`** — run の fixture から evalkit golden を導出。`derive_golden`（`id`/`describe`/`cmd`/`assert.exit=0`
  の JSON）と `slug_id`（curate 互換の安定 slug）を提供する。
