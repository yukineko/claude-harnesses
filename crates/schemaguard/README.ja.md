- `check --schema <name>`: JSON 値 (`--file <path>` または stdin) を名前付きスキーマで検証する [test: crates/schemaguard/tests/prose_claims.rs::claim_check_file_input]。
  入力がパースできたときの出力は `{valid, schema, errors[], undetermined[], not_checked[]}` (妥当な場合は `undetermined` を省く) [test: crates/schemaguard/tests/prose_claims.rs::claim_check_output_keys]。
  終了コードは `0` 妥当 / `1` 違反 / `2` パース失敗・未知スキーマ・適用不能な制約 (違反なしの場合) [test: crates/schemaguard/tests/prose_claims.rs::claim_check_exit_codes] [test: crates/schemaguard/tests/prose_claims.rs::claim_undetermined_exit_codes]。
- `metrics`: スキーマ別の reject 件数を `~/.schemaguard/rejects.jsonl` から出力する (`--json` で機械可読) [test: crates/schemaguard/tests/prose_claims.rs::claim_metrics_reads_store]。
  件数は reject された検査の回数ではない: 違反 2 件の `check` 1 回で `2`、パース失敗で `1` 増える [test: crates/schemaguard/tests/prose_claims.rs::claim_metrics_sums_violations]。
  ストアが無いか読めるときは終了コード `0`、ストアが存在するのに読めない・不正な行があるときは `2` (`--json` では stdout に `{"status":"unknown",...}`) [test: crates/schemaguard/tests/prose_claims.rs::claim_metrics_exit]。
- `list`: 既知のスキーマ名を一覧表示し、終了コードは `0` [test: crates/schemaguard/tests/integration.rs::list_prints_known_schema_names]。

- `0`: JSON がパースでき、実行された検査はすべて合格 (実行されなかった検査は `not_checked[]` に列挙される) [test: crates/schemaguard/tests/integration.rs::check_valid_decomposition_exits_zero] [test: crates/schemaguard/tests/integration.rs::valid_verdict_names_the_declared_checks_it_did_not_perform]
- `1`: パースは成功したがスキーマ違反あり (→ 再依頼) [test: crates/schemaguard/tests/integration.rs::check_schema_violation_exits_one]
- `2`: 判定不能 — JSON のパース失敗、未知のスキーマ指定、または宣言された制約を渡された値に適用できなかった (`undetermined[]`、違反なしの場合) [test: crates/schemaguard/tests/prose_claims.rs::claim_undetermined_exit_codes] [test: crates/schemaguard/tests/integration.rs::check_unparseable_json_exits_two] [test: crates/schemaguard/tests/integration.rs::check_unknown_schema_exits_two] [test: crates/schemaguard/tests/undetermined_probe.rs::undetermined_items_value_exits_two]

- **`errors[]`** — 検査が**実行され、失敗した**もの [test: crates/schemaguard/tests/undetermined_probe.rs::missing_required_name_exits_one]。
  各要素は `{path, problem}` (例: `tasks[0].id` と空でない問題文) [test: crates/schemaguard/tests/prose_claims.rs::claim_errors_entry_path_problem]。
- **`undetermined[]`** — 宣言された制約を**適用できなかった**もの (例: `items` サブスキーマを配列でない値に適用しようとした場合) [test: crates/schemaguard/tests/prose_claims.rs::claim_undetermined_exit_codes]。
- 「検査できなかった」は「合格」ではない: `undetermined[]` がある判定は `0` で終了しない [test: crates/schemaguard/tests/prose_claims.rs::claim_undetermined_exit_codes]。
  `errors[]` が空なら `2`、`errors[]` も空でなければ `1` で終了する [test: crates/schemaguard/tests/prose_claims.rs::claim_undetermined_exit_codes]。
- **`not_checked[]`** — **意図的に実行しなかった**検査が `{path, reason}` (空でない根拠) として入る: [test: crates/schemaguard/tests/prose_claims.rs::claim_not_checked_entry_reason]
  - 欠落した `required: false` フィールド (その型/enum/items 制約は一度も評価されていない) [test: crates/schemaguard/tests/integration.rs::valid_verdict_names_the_declared_checks_it_did_not_perform]
  - `Ty::Any` フィールド (型検査なし) [test: crates/schemaguard/src/schema.rs::any_type_is_waived_not_undetermined]
  - `items` を宣言していない配列 (要素は検査されない) [test: crates/schemaguard/tests/prose_claims.rs::claim_array_without_items_not_checked]
  - スキーマが宣言していない余剰キー (許容される) [test: crates/schemaguard/src/schema.rs::waived_checks_do_not_block]
- `not_checked[]` の項目は**ブロックしない** [test: crates/schemaguard/src/schema.rs::waived_checks_do_not_block]。

- 入力が読めない・パースできないときは `{"valid": false, "error": "<message>"}` を出力する (`errors` 配列ではなく単数の `error` 文字列) [test: crates/schemaguard/tests/prose_claims.rs::claim_input_failure_shape]。
- スキーマ名が解決されれば、読めない・パースできない入力も含め stdout に JSON 判定オブジェクトを 1 つ出力する [test: crates/schemaguard/tests/prose_claims.rs::claim_check_single_json_stdout]。
- 未知のスキーマは入力を読む前に stderr へ平文で報告され、stdout には何も出ず、終了コードは `2` [test: crates/schemaguard/tests/prose_claims.rs::claim_unknown_schema_stderr]。
- パース失敗・フィールド違反・判定不能の検査はいずれも `metrics` の reject に数えられ、妥当な検査は数えられない [test: crates/schemaguard/tests/prose_claims.rs::claim_rejects_counted]。
- 宣言済みスキーマは `decomposition` / `episode` / `playbook` / `scout-measure` / `verdict` / `undetermined-probe` の 6 つ (`schemaguard list`) [test: crates/schemaguard/tests/prose_claims.rs::claim_list_exact_names]。
- プラグインは `bin/schemaguard` ランチャーを同梱し、lifecycle hook を宣言しない [test: crates/schemaguard/tests/prose_claims.rs::claim_plugin_no_hooks]。

```sh
cargo install --path .
schemaguard list
echo '{...}' | schemaguard check --schema decomposition   # stdin を検証 [test: crates/schemaguard/tests/integration.rs::check_valid_decomposition_exits_zero]
schemaguard check --schema episode --file out.json
schemaguard metrics --json
```

受け渡し箇所で `schemaguard check --schema <name>` を呼び、終了コードで分岐すること (`1`/`2` なら再依頼) [test: crates/schemaguard/tests/prose_claims.rs::claim_check_exit_codes]。

```sh
cargo test
```

コミットされているのは `bin/schemaguard` ランチャーだけで、プラットフォーム別の `bin/schemaguard-<os>-<arch>` のうち `bin/schemaguard-darwin-*` と `bin/schemaguard-linux-*` は git-ignore されており、`bin/schemaguard-windows-*.exe` は git-ignore されていない [test: crates/schemaguard/tests/prose_claims.rs::claim_launcher_only_committed]。
それらは `scripts/rollout-plugins.sh` が呼ぶ `scripts/rebuild-plugins.sh` がビルドしてコピーする [test: crates/schemaguard/tests/prose_claims.rs::claim_launcher_only_committed]。
