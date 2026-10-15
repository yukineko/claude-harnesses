`curate promote` turns a chosen playbook entry (a JSON line with `title` and `done_criteria`) into a golden case line with an `id` [test: crates/curate/tests/integration.rs::promote_writes_curated_golden_end_to_end].

curate auto-derives a runnable case only when the acceptance criterion (`done_criteria`) is **mechanical** [test: crates/curate/src/derive.rs::backticked_command_becomes_cmd_golden] [test: crates/curate/src/derive.rs::non_mechanical_criterion_becomes_draft] [test: crates/curate/src/derive.rs::backtick_ignored_when_not_a_command]. It drafts the rest [test: crates/curate/src/derive.rs::non_mechanical_criterion_becomes_draft].

- `` `cargo test --workspace` passes `` gets `"cmd":["cargo","test","--workspace"]` and `"assert":{"exit":0}` [test: crates/curate/src/derive.rs::backticked_command_becomes_cmd_golden].
- "cargo test -p evalkit is green" gets `"cmd":["cargo","test","-p","evalkit"]` and `"assert":{"exit":0}` [test: crates/curate/tests/prose_claims.rs::claim_cargo_test_scoped_golden].
- "auth handles token refresh" becomes `{"draft":true,"describe":"… — TODO assert done_criteria: …"}` [test: crates/curate/tests/prose_claims.rs::claim_non_mechanical_draft_describe].

Mechanical signals: an explicit backticked command [test: crates/curate/src/derive.rs::backticked_command_becomes_cmd_golden], or a recognised test runner (`cargo test` [test: crates/curate/src/derive.rs::cargo_test_with_scope_is_captured], `npm test` and `pytest` [test: crates/curate/src/derive.rs::pytest_and_npm_recognised]). `go test` is also a recognised test runner [test: crates/curate/tests/prose_claims.rs::claim_go_test_recognised].

evalkit skips a draft case: it is neither a pass nor a fail [test: crates/evalkit/src/run.rs::draft_case_is_skipped_not_failed].

```sh
curate candidates                       # list promotable playbooks (mech | draft), newest first; --k N limits the count (default 20) [test: crates/curate/tests/prose_claims.rs::claim_candidates_limit_default_20]
curate promote "add login" --dataset auth   # → evals/curated/auth.jsonl [test: crates/curate/tests/integration.rs::promote_writes_curated_golden_end_to_end]
curate promote --latest                 # the most recent playbook [test: crates/curate/tests/prose_claims.rs::claim_promote_latest_cli]
curate promote "x" --draft              # force a draft even if mechanical [test: crates/curate/tests/prose_claims.rs::claim_promote_draft_flag]
curate promote --latest --store /path/to/playbooks.jsonl  # override the playbook store [test: crates/curate/tests/integration.rs::promote_writes_curated_golden_end_to_end]
```

`promote` accepts `--store` to read the playbook store from another path [test: crates/curate/tests/integration.rs::promote_writes_curated_golden_end_to_end]. `candidates` reads the playbook store from `--store` too [test: crates/curate/tests/prose_claims.rs::claim_candidates_store_reads_file]. The default store path is `~/.fugu-router/playbooks.jsonl` [test: crates/curate/tests/prose_claims.rs::claim_default_store_path].

`promote` selects by case-insensitive title substring, and the most recent match wins [test: crates/curate/tests/prose_claims.rs::claim_promote_selector_cli]. `promote --latest` selects the newest playbook [test: crates/curate/tests/prose_claims.rs::claim_promote_latest_cli].

Promotions append to `<root>/<evals-dir>/curated/<name>.jsonl`, where `--evals-dir` defaults to `evals` [test: crates/curate/tests/integration.rs::promote_writes_curated_golden_end_to_end] [test: crates/curate/tests/prose_claims.rs::claim_promote_appends] [test: crates/curate/tests/prose_claims.rs::claim_evals_dir_flag]. `--dataset` defaults to `promoted` [test: crates/curate/tests/prose_claims.rs::claim_dataset_default_promoted]. A promotion is deduplicated by case id [test: crates/curate/tests/prose_claims.rs::claim_dedup_by_case_id]. `--root` defaults to the CWD [test: crates/curate/tests/prose_claims.rs::claim_root_defaults_to_cwd].

evalkit discovers `*.jsonl` goldens recursively, so cases under `curated/` are found [test: crates/evalkit/src/run.rs::discover_recurses_into_subdirs].
