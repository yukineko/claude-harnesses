`download` never reaches the network during `cargo test`..
benchkit does not redistribute the dataset: no dataset file is tracked outside `tests/fixtures/`, and `tests/fixtures/instances.jsonl` has 2 rows..
Outside `download` and `run-instance --real`, benchkit's own code spawns no process and opens no socket, and `load`, `run-instance` without `--real` and `audit-sample` (with `--audits` or `--from-violations`) invoke no external tool..
The ratify queue of `audit-sample` is never auto-applied..

benchkit provides the subcommands `load`, `download`, `run-instance` and `audit-sample`..
`dashboard` is a library module, not a subcommand; it appends one line per run to `runs.jsonl` and never rewrites earlier lines..
The patch generator behind `run-instance` is a stub that returns an empty candidate..

The loader spawns no process and reads no clock or environment variable; loading the same file twice yields the same `Vec<Instance>`..
The loader returns instances in file order..
It reports a `path:line` error on a malformed row..

`instance_id`, `repo`, `base_commit`, `patch`, `test_patch` and `problem_statement` are required: a row missing any of them fails to load..
`hints_text`, `created_at`, `version` and `environment_setup_commit` default to an empty string when absent, and `created_at` is kept verbatim..
`fail_to_pass` (JSONL key `FAIL_TO_PASS`) lists tests that must pass and `pass_to_pass` (JSONL key `PASS_TO_PASS`) lists tests that must stay passing; a failing id in either list leaves the instance unresolved..
The model renames the upper-case `FAIL_TO_PASS` / `PASS_TO_PASS` keys onto snake_case Rust fields via serde..
The vendored fixture encodes those as a plain JSON list of strings..
`download` writes the upstream `.parquet` file from huggingface.co to the cache path byte-for-byte; it does not convert it to JSONL..

```sh
benchkit load crates/benchkit/tests/fixtures/instances.jsonl

benchkit download                 # writes .benchkit-cache/swe-bench-verified.jsonl
benchkit download --dest data/verified.jsonl   # --dest sets the cache file path
benchkit download --force         # re-fetch even if cached

# Without --real this is a dry run: no git or pytest runs and no test results are produced.
benchkit run-instance <instances.jsonl> <instance_id>
benchkit run-instance <instances.jsonl> <instance_id> --real   # git clone/checkout/apply + python -m pytest

benchkit audit-sample <changes.jsonl> [--audits <verdicts.jsonl> | --from-violations] \
    [--fraction 0.1] [--seed 0] [--json]
```

`run-instance --real` runs `git clone`, `git checkout <base_commit>`, `git apply` of `test_patch`, then `python -m pytest` over the `FAIL_TO_PASS` and `PASS_TO_PASS` ids, and scores the parsed pytest output..
A test id absent from the result map counts as not passing..

`audit-sample` draws a deterministic seeded sample of changes..
Given verdicts (`--audits`) or the real overwatch violation stream (`--from-violations`), it routes misses into new-invariant candidates and a threshold-adjustment ratify queue..
`--audits` and `--from-violations` are mutually exclusive..

`download` is a no-op when the cache already exists, unless `--force` is given..
It shells out to `curl` with a hard timeout (`--max-time 120`) and lists no HTTP client crate in its `[dependencies]`..

<!-- record: 2026-07-08 rev: 999af47e -->
- **Dataset:** SWE-bench Verified — a 500-task human-validated subset of
  SWE-bench.
- **Publisher:** Princeton NLP (`princeton-nlp/SWE-bench_Verified` on
  HuggingFace, <https://huggingface.co/datasets/princeton-nlp/SWE-bench_Verified>).
- **License:** the dataset is distributed under its upstream terms (MIT for the
  SWE-bench tooling; the referenced repositories carry their own licenses).
<!-- /record -->
