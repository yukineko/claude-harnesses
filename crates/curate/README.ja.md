`curate promote` は選んだ playbook エントリ（`title` と `done_criteria` を持つ JSON 行）を、`id` 付きの golden ケース行に変換する [test: crates/curate/tests/integration.rs::promote_writes_curated_golden_end_to_end]。

curate は受け入れ基準（`done_criteria`）が**機械的（mechanical）**なときだけ、実行可能なケースを自動導出する [test: crates/curate/src/derive.rs::backticked_command_becomes_cmd_golden] [test: crates/curate/src/derive.rs::non_mechanical_criterion_becomes_draft] [test: crates/curate/src/derive.rs::backtick_ignored_when_not_a_command]。それ以外は draft になる [test: crates/curate/src/derive.rs::non_mechanical_criterion_becomes_draft]。

- `` `cargo test --workspace` passes `` は `"cmd":["cargo","test","--workspace"]` と `"assert":{"exit":0}` を持つ [test: crates/curate/src/derive.rs::backticked_command_becomes_cmd_golden]。
- "cargo test -p evalkit is green" は `"cmd":["cargo","test","-p","evalkit"]` と `"assert":{"exit":0}` を持つ [test: crates/curate/tests/prose_claims.rs::claim_cargo_test_scoped_golden]。
- "auth handles token refresh" は `{"draft":true,"describe":"… — TODO assert done_criteria: …"}` になる [test: crates/curate/tests/prose_claims.rs::claim_non_mechanical_draft_describe]。

機械的とみなすシグナルは、明示的にバッククォートで囲まれたコマンド [test: crates/curate/src/derive.rs::backticked_command_becomes_cmd_golden]、または認識済みのテストランナー（`cargo test` [test: crates/curate/src/derive.rs::cargo_test_with_scope_is_captured]、`npm test` と `pytest` [test: crates/curate/src/derive.rs::pytest_and_npm_recognised]）。`go test` も認識済みのテストランナーである [test: crates/curate/tests/prose_claims.rs::claim_go_test_recognised]。

evalkit は draft ケースをスキップする（pass にも fail にもならない） [test: crates/evalkit/src/run.rs::draft_case_is_skipped_not_failed]。

```sh
curate candidates                            # 昇格可能な playbook を新しい順に一覧（mech | draft）; --k N で件数を絞る（既定 20） [test: crates/curate/tests/prose_claims.rs::claim_candidates_limit_default_20]
curate promote "add login" --dataset auth    # → evals/curated/auth.jsonl [test: crates/curate/tests/integration.rs::promote_writes_curated_golden_end_to_end]
curate promote --latest                      # 直近に記録された playbook を昇格 [test: crates/curate/tests/prose_claims.rs::claim_promote_latest_cli]
curate promote "x" --draft                   # 機械的でも強制的に draft にする [test: crates/curate/tests/prose_claims.rs::claim_promote_draft_flag]
curate promote --latest --store /path/to/playbooks.jsonl  # playbook ストアを明示指定 [test: crates/curate/tests/integration.rs::promote_writes_curated_golden_end_to_end]
```

`promote` は `--store` で別パスの playbook ストアを読める [test: crates/curate/tests/integration.rs::promote_writes_curated_golden_end_to_end]。`candidates` も `--store` で指定したパスの playbook ストアを読む [test: crates/curate/tests/prose_claims.rs::claim_candidates_store_reads_file]。既定のストアパスは `~/.fugu-router/playbooks.jsonl` である [test: crates/curate/tests/prose_claims.rs::claim_default_store_path]。

`promote` は `title` の部分一致（大文字小文字を無視）で選び、最新の一致が優先される [test: crates/curate/tests/prose_claims.rs::claim_promote_selector_cli]。`promote --latest` は最新の playbook を選ぶ [test: crates/curate/tests/prose_claims.rs::claim_promote_latest_cli]。

昇格は `<root>/<evals-dir>/curated/<name>.jsonl` に追記され、`--evals-dir` の既定は `evals` である [test: crates/curate/tests/integration.rs::promote_writes_curated_golden_end_to_end] [test: crates/curate/tests/prose_claims.rs::claim_promote_appends] [test: crates/curate/tests/prose_claims.rs::claim_evals_dir_flag]。`--dataset` の既定は `promoted` である [test: crates/curate/tests/prose_claims.rs::claim_dataset_default_promoted]。昇格はケース id で重複排除される [test: crates/curate/tests/prose_claims.rs::claim_dedup_by_case_id]。`--root` の既定は CWD である [test: crates/curate/tests/prose_claims.rs::claim_root_defaults_to_cwd]。

evalkit は `*.jsonl` の golden を再帰的に探索するため、`curated/` 配下のケースも見つかる [test: crates/evalkit/src/run.rs::discover_recurses_into_subdirs]。
