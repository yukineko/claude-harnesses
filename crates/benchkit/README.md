`download` never reaches the network during `cargo test`. [test: crates/benchkit/tests/prose_claims.rs::claim_download_not_in_cargo_test] [test: crates/benchkit/tests/prose_claims.rs::claim_download_not_in_cargo_test_binary_runs].
benchkit does not redistribute the dataset: no dataset file is tracked outside `tests/fixtures/`, and `tests/fixtures/instances.jsonl` has 2 rows. [test: crates/benchkit/tests/prose_claims.rs::claim_no_redistribution].
Outside `download` and `run-instance --real`, benchkit's own code spawns no process and opens no socket, and `load`, `run-instance` without `--real` and `audit-sample` (with `--audits` or `--from-violations`) invoke no external tool. [test: crates/benchkit/tests/prose_claims.rs::claim_shell_io_only_download_and_real].
The ratify queue of `audit-sample` is never auto-applied. [test: crates/benchkit/src/auditsample.rs::ratify_queue_never_auto_applies] [test: crates/benchkit/tests/prose_claims.rs::claim_ratify_queue_cli_never_applies].

benchkit provides the subcommands `load`, `download`, `run-instance` and `audit-sample`. [test: crates/benchkit/tests/prose_claims.rs::claim_subcommands].
`dashboard` is a library module, not a subcommand; it appends one line per run to `runs.jsonl` and never rewrites earlier lines. [test: crates/benchkit/tests/prose_claims.rs::claim_subcommands] [test: crates/benchkit/tests/prose_claims.rs::claim_dashboard_append_only].
The patch generator behind `run-instance` is a stub that returns an empty candidate. [test: crates/benchkit/tests/prose_claims.rs::claim_stub_generator_empty].

The loader spawns no process and reads no clock or environment variable; loading the same file twice yields the same `Vec<Instance>`. [test: crates/benchkit/tests/prose_claims.rs::claim_loader_pure_deterministic].
The loader returns instances in file order. [test: crates/benchkit/src/loader.rs::skips_blank_lines_and_preserves_order].
It reports a `path:line` error on a malformed row. [test: crates/benchkit/src/loader.rs::malformed_line_reports_line_number] [test: crates/benchkit/tests/prose_claims.rs::claim_malformed_row_path_line].

`instance_id`, `repo`, `base_commit`, `patch`, `test_patch` and `problem_statement` are required: a row missing any of them fails to load. [test: crates/benchkit/tests/prose_claims.rs::claim_instance_required_fields].
`hints_text`, `created_at`, `version` and `environment_setup_commit` default to an empty string when absent, and `created_at` is kept verbatim. [test: crates/benchkit/tests/prose_claims.rs::claim_instance_required_fields].
`fail_to_pass` (JSONL key `FAIL_TO_PASS`) lists tests that must pass and `pass_to_pass` (JSONL key `PASS_TO_PASS`) lists tests that must stay passing; a failing id in either list leaves the instance unresolved. [test: crates/benchkit/tests/prose_claims.rs::claim_f2p_p2p_semantics].
The model renames the upper-case `FAIL_TO_PASS` / `PASS_TO_PASS` keys onto snake_case Rust fields via serde. [test: crates/benchkit/tests/loader.rs::loads_vendored_fixture_into_instances].
The vendored fixture encodes those as a plain JSON list of strings. [test: crates/benchkit/tests/loader.rs::loads_vendored_fixture_into_instances].
`download` writes the upstream `.parquet` file from huggingface.co to the cache path byte-for-byte; it does not convert it to JSONL. [test: crates/benchkit/tests/prose_claims.rs::claim_download_shape].

```sh
benchkit load crates/benchkit/tests/fixtures/instances.jsonl

benchkit download                 # writes .benchkit-cache/swe-bench-verified.jsonl [test: crates/benchkit/tests/prose_claims.rs::claim_download_default_dest]
benchkit download --dest data/verified.jsonl   # --dest sets the cache file path [test: crates/benchkit/tests/prose_claims.rs::claim_download_dest_flag]
benchkit download --force         # re-fetch even if cached [test: crates/benchkit/tests/prose_claims.rs::claim_download_force_refetch]

# Without --real this is a dry run: no git or pytest runs and no test results are produced. [test: crates/benchkit/tests/prose_claims.rs::claim_dry_run]
benchkit run-instance <instances.jsonl> <instance_id>
benchkit run-instance <instances.jsonl> <instance_id> --real   # git clone/checkout/apply + python -m pytest [test: crates/benchkit/tests/prose_claims.rs::claim_real_path]

benchkit audit-sample <changes.jsonl> [--audits <verdicts.jsonl> | --from-violations] \
    [--fraction 0.1] [--seed 0] [--json]
```

`run-instance --real` runs `git clone`, `git checkout <base_commit>`, `git apply` of `test_patch`, then `python -m pytest` over the `FAIL_TO_PASS` and `PASS_TO_PASS` ids, and scores the parsed pytest output. [test: crates/benchkit/tests/prose_claims.rs::claim_real_path].
A test id absent from the result map counts as not passing. [test: crates/benchkit/src/scorer.rs::missing_test_id_is_fail_closed].

`audit-sample` draws a deterministic seeded sample of changes. [test: crates/benchkit/src/auditsample.rs::sample_is_deterministic_for_a_seed].
Given verdicts (`--audits`) or the real overwatch violation stream (`--from-violations`), it routes misses into new-invariant candidates and a threshold-adjustment ratify queue. [test: crates/benchkit/src/auditsample.rs::a_miss_emits_both_feedback_paths] [test: crates/benchkit/src/auditsample.rs::real_audit_cross_reference_match_is_miss_nonmatch_ignored] [test: crates/benchkit/tests/prose_claims.rs::claim_audit_sample_cli] [test: crates/benchkit/tests/prose_claims.rs::claim_from_violations_flag_routes].
`--audits` and `--from-violations` are mutually exclusive. [test: crates/benchkit/tests/prose_claims.rs::claim_audit_sample_cli].

`download` is a no-op when the cache already exists, unless `--force` is given. [test: crates/benchkit/src/download.rs::existing_cache_is_a_noop] [test: crates/benchkit/tests/prose_claims.rs::claim_download_force_refetch].
It shells out to `curl` with a hard timeout (`--max-time 120`) and lists no HTTP client crate in its `[dependencies]`. [test: crates/benchkit/tests/prose_claims.rs::claim_download_curl_timeout].

<!-- record: 2026-07-08 rev: 999af47e -->
- **Dataset:** SWE-bench Verified — a 500-task human-validated subset of
  SWE-bench.
- **Publisher:** Princeton NLP (`princeton-nlp/SWE-bench_Verified` on
  HuggingFace, <https://huggingface.co/datasets/princeton-nlp/SWE-bench_Verified>).
- **License:** the dataset is distributed under its upstream terms (MIT for the
  SWE-bench tooling; the referenced repositories carry their own licenses).
<!-- /record -->
