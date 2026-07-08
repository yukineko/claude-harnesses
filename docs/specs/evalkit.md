> **REVIEW-NEEDED** — この仕様は実装から逆算生成した draft。人間レビューまで正典として扱わない。

# evalkit 仕様

## 概要

`evalkit` は harness モノレポ向けの **オフライン・ゴールデン回帰テスト harness** である。condukt の
オンライン Phase-6 検証器の「オフラインの兄弟」にあたり、`SKILL.md` のハードルールや `--json` CLI 契約
といった *すでにプラグインへ焼き込まれたガードレール* を決定論的にアサートし、回帰したら CI を赤くして
マージ前に止める。1 バイナリの plain CLI で、**API キー不要**（subscription-native）。`main.rs` の
doc-comment が宣言するとおり、これは lifecycle hook ではなく plain CLI なので `run_hook` で包まず、
失敗を握り潰して exit 0 する「無害化されたゲート」を意図的に避けている。

ゴールデンはリポジトリルート相対の `evals/*.jsonl`（既定 `--dir evals`）にあり、1 行 1 ケース。1 ケースは
1 つの *subject*（`file` の内容 xor `cmd` の stdout）を指し、それに対する部分文字列・正規表現・exit-code の
アサーションを並べる。`run` が全ケースをアサートし、`canary` が 2 つの `run --json` 出力を差分して
プロンプト編集で *どのゴールデンが動いたか* を見せる。

## 不変条件

- **subject は file xor cmd（draft 免除）** — `case::validate` は非 draft ケースに `file`/`cmd` の
  **ちょうど一方**と非空 `id` を要求し、両方あり／どちらも無し／空 `cmd` を parse 時に `bail!` する。
  `draft: true` のケースはまだ subject を持たないため、この要件を免除される。
- **draft は pass でも fail でもない** — `run::run_case` は `draft` ケースを `skipped=true` で返し、
  `Outcome::passed()` は skipped を pass に数えない。`execute` の gating カウントは failure のみを見るので、
  未記入のゴールデンが repo に居座っても CI を壊さない（可視な pending work として残る）。
- **アサーション核は純関数** — `run::check_assert` は subject 文字列だけに作用し、FS もプロセス spawn も
  触れないので単体テスト可能。`canary::diff`/`pass_rate` も 2 つの case-map に対する純関数で、
  parse から独立している。
- **取得失敗は沈黙合格でなく failure** — `run_case` は subject 取得失敗（読めない `file`・spawn 不能な
  `cmd`）を failure として記録し、壊れたケースが黙って通ることを防ぐ。無効な正規表現も panic せず failure。
- **exit code の区別** — `run`/`list` は `0`=全通過 / `1`=真の回帰（アサーション失敗）/ `2`=harness エラー
  （ケース 0 件・eval ファイル読めず等）。CI が「真の回帰」と「設定ミス」を判別できる。
- **canary の回帰は厳密定義かつ既定 non-gating** — `canary::diff` の regression は *厳密に*
  `baseline.pass && current.fail`（両側とも非 skipped）のみ。skip 遷移（pass→skip 等）は regression に
  数えず `unchanged` へ落ちる。既定は情報提供のみ exit 0 で、`--fail-on-regression` 指定時のみ regression
  1 件以上で exit 1 のハードゲートになる。
- **決定論** — `run::discover` は `*.jsonl` を再帰収集し `sort()` してから parse するため、ケース列挙は
  ファイル順に決定的。`canary` は `BTreeMap` キー（ソート済み）で各 bucket を安定順にする。

## 振る舞い

サブコマンドは `clap` の `Command` enum で定義（`main`）。

- **`run [--dir] [--bin-dir] [--root] [--json]`** — `run::execute(list_only=false)`。`--dir`（既定 `evals`）
  下の `*.jsonl` を `discover` で再帰探索・parse し、各ケースを `run_case` で subject 取得→アサート。ケース
  0 件は exit 2。`--json` は `{total, passed, failed, skipped, cases:[{case, pass, skipped, failures}]}`
  を出す（`report_json`）。失敗 0 で exit 0、1 件以上で exit 1。
- **`list [--dir] [--root] ...`** — 同じ `execute(list_only=true)`。ケースを実行せず `path\tlabel` を
  列挙して exit 0（ケース 0 件なら discover 段で exit 2）。
- **`canary --baseline <f> --current <f> [--json] [--fail-on-regression]`** — `canary::execute`。2 つの
  `run --json` 出力（`parse_report` で `cases[]` を label キーの case-map 化）を `diff` し、各ケースを
  **regression**(pass→fail) / **fix**(fail→pass) / **added** / **dropped** / **unchanged** に分類、
  合格率を before→after と delta で表示。`--json` は機械可読差分。読めない／`cases` 配列の無い入力は exit 2。

`--bin-dir DIR` は `cmd` ケース解決時に PATH の先頭へ prepend され（相対なら `--root` に解決、
`run::path_with_prefix`）、ビルドしたての `target/release/<tool>` をインストールせず走らせられる。`cmd`
subject は `--root` を cwd に spawn され、`stdin` があれば流し込まれ、stdout と exit code を捕捉する
（`run::run_cmd`）。CI 配線は `.github/workflows/eval.yml`。

### module 責務

- **`main`** — clap CLI 定義（`Cli`/`Command`/`RunArgs`/`CanaryArgs`）と dispatch のみ。plain CLI として
  `run_hook` を使わないことをコメントで明言。exit code をそのまま `process::exit`。
- **`case`** — ゴールデンケースの schema + JSONL parse。`Case`（`id`/`describe`/`file`/`cmd`/`stdin`/
  `assert`/`draft`）、`Assert`（`exit`/`contains`/`not_contains`/`regex`/`not_regex`、全て省略可）、
  `parse_jsonl`（空行・`//` コメント行スキップ、`path:line` でエラー位置付け）、`validate`（subject xor +
  非空 id、draft 免除）、`Case::label`。
- **`run`** — ケースランナー。`RunCfg`（`root`/`bin_dir`）、`Outcome`（`label`/`failures`/`skipped`、
  `passed()`）、`run_case`/`acquire`/`run_cmd`/`path_with_prefix`、純アサーション核 `check_assert` と
  `check_exit`、`discover`/`collect_jsonl`（再帰・ソート）、`execute`（オーケストレーション・exit code）、
  `report_human`/`report_json`。
- **`canary`** — 2 run の純差分層。`CaseResult`（`pass`/`skipped`）、`Diff`（`baseline_pass_rate`/
  `current_pass_rate`/`regressions`/`fixes`/`added`/`dropped`/`unchanged`、`delta()`）、純関数
  `pass_rate`（skipped を分母から除外・0 割回避）と `diff`（厳密な regression/fix 判定）、`parse_report`、
  `execute`（exit policy）、`report_human`/`report_json`。
