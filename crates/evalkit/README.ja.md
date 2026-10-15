オフライン **ゴールデン回帰テスト harness**[test: crates/evalkit/tests/integration.rs::help_exits_zero_and_describes_the_harness]。

ゴールデンとして固定した `*.jsonl` ケースを読み込み、各 subject に対してアサートし、不変条件が回帰したら非ゼロで終了する[test: crates/evalkit/tests/prose_claims.rs::claim_run_exit_on_regress]。

1 ケースは 1 つの *subject* を指し、subject は `file`（そのファイルの内容）または `cmd`（その標準出力）のどちらか一方であり、それに対してアサーションを並べる[test: crates/evalkit/src/case.rs::parses_file_and_cmd_cases_skipping_comments_and_blanks] [test: crates/evalkit/tests/prose_claims.rs::claim_file_subject_rel_root] [test: crates/evalkit/tests/prose_claims.rs::claim_cmd_subject_stdout] [test: crates/evalkit/src/case.rs::rejects_case_with_both_subjects]。

```jsonl
// プロンプト不変条件: flow はハードルールを保持し続けなければならない
{"id":"flow-keeps-blind-exec","file":"crates/flow/skills/flow/SKILL.md","assert":{"contains":["盲目実行しない"]}}
// CLI 契約: compass nudge は機械可読な判定を出す
{"id":"compass-nudge-json","cmd":["compass","nudge","--json"],"assert":{"exit":0,"regex":["\"fresh\"\\s*:\\s*(true|false)"]}}
```

- `id` — 安定したケース名（必須） [test: crates/evalkit/tests/prose_claims.rs::claim_id_required]
- `describe` — 人間向けの 1 行ラベル（任意） [test: crates/evalkit/tests/prose_claims.rs::claim_describe_label]
- `file` — このファイルの内容を subject として読む（`--root` からの相対パス） [test: crates/evalkit/tests/prose_claims.rs::claim_file_subject_rel_root]
- `cmd` — `cmd[0]` を残りを引数として実行し、標準出力を subject として捕捉する [test: crates/evalkit/tests/prose_claims.rs::claim_cmd_subject_stdout]
- `stdin` — `cmd` subject に流す標準入力（任意） [test: crates/evalkit/tests/prose_claims.rs::claim_stdin_piped]
- `assert.exit` — 期待する終了コード（`cmd` のみ）[test: crates/evalkit/src/run.rs::exit_mismatch_reported] [test: crates/evalkit/tests/prose_claims.rs::claim_cmd_subject_stdout]
- `assert.contains` / `not_contains` — 現れる／現れてはならない部分文字列 [test: crates/evalkit/src/run.rs::contains_and_not_contains]
- `assert.regex` / `not_regex` — マッチする／してはならない正規表現 [test: crates/evalkit/src/run.rs::regex_match_and_forbidden]
- `draft` — 人手のアサーションを待つ昇格ドラフトであり、実行時はスキップされる（pass/fail どちらにもならない）[test: crates/evalkit/src/run.rs::draft_case_is_skipped_not_failed]

1 ケースは `file` か `cmd` の**ちょうど一方**を持つ——ただし `draft: true` の
ケースはまだ subject を持たないため、この要件を免除される[test: crates/evalkit/src/case.rs::rejects_case_with_both_subjects] [test: crates/evalkit/src/case.rs::rejects_case_with_no_subject] [test: crates/evalkit/src/case.rs::draft_case_is_exempt_from_subject_requirement]。

- subject は**標準出力だけ**である[test: crates/evalkit/tests/prose_claims.rs::claim_stdout_only]。標準エラーにだけ書かれたテキストは subject に含まれない[test: crates/evalkit/tests/prose_claims.rs::claim_stdout_only]。
- 取得失敗（ファイルが読めない／cmd が spawn できない）はケースの失敗として報告される[test: crates/evalkit/tests/prose_claims.rs::claim_acquire_fail]。
- `file` ケースに `assert.exit` を書くと、それ自体が失敗になる（ファイルに終了コードは無い）[test: crates/evalkit/src/run.rs::exit_on_file_subject_is_a_failure]。

- `0` — 全ケース通過 [test: crates/evalkit/tests/prose_claims.rs::claim_exit_codes_run]
- `1` — 真の回帰（アサーション失敗） [test: crates/evalkit/tests/prose_claims.rs::claim_exit_codes_run]
- `2` — harness エラー（ケースが見つからない、eval ファイルが読めない等） [test: crates/evalkit/tests/prose_claims.rs::claim_exit_codes_run]

```sh
evalkit run
evalkit run --root . --bin-dir target/release
evalkit run --dir evals/replay
evalkit run --json
evalkit list
```

`--dir` は `*.jsonl` を**再帰的に**探索するので、`evals/replay/` のようなサブディレクトリも設定なしで拾われる[test: crates/evalkit/src/run.rs::discover_recurses_into_subdirs]。`--dir` の既定は `evals` である[test: crates/evalkit/tests/prose_claims.rs::claim_dir_default_evals]。`--root`（既定は CWD）は `file` パスと `--dir` の基点であり、`cmd` サブプロセスの cwd でもある[test: crates/evalkit/tests/prose_claims.rs::claim_root_anchors]。空行と `//` 行は読み飛ばされるが、それ以外のパースできない行や、`id` が空・subject が両方／どちらも無いケースが 1 つでもあると、実行全体が harness エラー（exit 2）になる[test: crates/evalkit/tests/prose_claims.rs::claim_parse_errors_exit2]。`list` も同じフラグを受け付けるが、常に素の一覧を出す[test: crates/evalkit/tests/prose_claims.rs::claim_list_plain]。

`--bin-dir DIR` は `cmd` ケースの解決時に `PATH` の先頭へ追加される[test: crates/evalkit/tests/prose_claims.rs::claim_bin_dir_path]。

`draft` ケースは `skip <label> (draft — assertion pending)` として報告され、サマリで別枠に集計される（`N draft(s) skipped`）[test: crates/evalkit/tests/prose_claims.rs::claim_draft_reported]。

`evalkit canary` は 2 つの `evalkit run --json` 出力を差分比較する[test: crates/evalkit/tests/prose_claims.rs::claim_canary_label_keys]。

```sh
evalkit run --json > base.json
evalkit run --json > head.json
evalkit canary --baseline base.json --current head.json
evalkit canary --baseline base.json --current head.json --json
evalkit canary --baseline base.json --current head.json --fail-on-regression # pass→fail があれば exit 1 [test: crates/evalkit/src/canary.rs::fail_on_regression_exit_policy]
```

ケースはレポート上のラベル（`run --json` の `case` フィールド：`id`、`describe` があれば `id — describe`）でキーにする — そのため `describe` を編集すると dropped 1 件 + added 1 件として現れる[test: crates/evalkit/tests/prose_claims.rs::claim_canary_label_keys]。それぞれを **regression**（pass→fail、どちらも draft でない）[test: crates/evalkit/src/canary.rs::skipped_transitions_are_not_regressions]、**fix**（fail→pass）、**added**、**dropped** に分類し、合格率を before → after と delta で表示する[test: crates/evalkit/tests/prose_claims.rs::claim_canary_label_keys]。合格率の分母から draft は除外され、全件 draft または空なら 0.0 になる[test: crates/evalkit/src/canary.rs::pass_rate_math_and_divide_by_zero]。既定では**情報提供のみ**（exit 0）である[test: crates/evalkit/src/canary.rs::fail_on_regression_exit_policy]。`--fail-on-regression` を渡すと pass→fail が 1 件でもあれば exit 1 になる[test: crates/evalkit/src/canary.rs::fail_on_regression_exit_policy]。

`dropped` は自分で確認すること[test: crates/evalkit/tests/prose_claims.rs::claim_canary_dropped_bucket]。

- `0` — 報告済み（`--fail-on-regression` 時は回帰なし）[test: crates/evalkit/src/canary.rs::fail_on_regression_exit_policy]
- `1` — `--fail-on-regression` 指定かつ pass→fail が 1 件以上 [test: crates/evalkit/src/canary.rs::fail_on_regression_exit_policy]
- `2` — harness エラー（結果ファイルが読めない／パースできない） [test: crates/evalkit/tests/prose_claims.rs::claim_canary_exit2]

ゴールデンはリポジトリルートの `evals/` にある[test: crates/evalkit/tests/prose_claims.rs::claim_evals_files]。

- `evals/skill-invariants.jsonl` — プラグイン `SKILL.md` に固定したハードルール[test: crates/evalkit/tests/prose_claims.rs::claim_evals_files]。
- `evals/cli-contracts.jsonl` — CLI の出力／終了コード契約[test: crates/evalkit/tests/prose_claims.rs::claim_evals_files]。
- `evals/replay/replayed.jsonl` — 昇格済みの condukt トラジェクトリ再生ケース
  （`evals/replay/fixtures/` 配下のフィクスチャに対して `replaykit verify` を
  アサートする `cmd` ケース）[test: crates/evalkit/tests/prose_claims.rs::claim_evals_files]。

新しい不変条件を成文化するたびに 1 行を足すこと。

**CI 配線は無い**: かつての `.github/workflows/eval.yml` は存在しない（このリポジトリは GitHub Actions を使わない）し、evalkit はフックも持たない[test: crates/evalkit/tests/prose_claims.rs::claim_no_ci_no_hook]。ビルド後に自分で `evalkit run --bin-dir target/release` などと実行すること[test: crates/evalkit/tests/prose_claims.rs::claim_nothing_runs_it]。
