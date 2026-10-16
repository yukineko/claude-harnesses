`replaykit promote` turns a tracekit run trace (`spans.jsonl`) into an evalkit golden case whose `cmd` re-runs `replaykit verify` on the promoted fixture, and that `cmd` exits 0 on the fixture as promoted and 1 once a fixture step's status is changed to an error [test: crates/replaykit/tests/prose_claims.rs::claim_intro_roundtrip].

```sh
replaykit promote --run my-run-2026-06-28 --root . --dataset replayed
#    writes  evals/replay/fixtures/<id>.json      (the portable summary)
#       and  evals/replay/replayed.jsonl          (the golden; re-promoting the same run adds no line) [test: crates/replaykit/tests/prose_claims.rs::claim_promote_writes_fixture_and_dedup]
```

Without `--spans`, `replaykit extract --run <RID>` reads spans from `~/.tracekit/<sanitize(RID)>/spans.jsonl` [test: crates/replaykit/tests/prose_claims.rs::claim_extract_default_spans_path].
`--spans <path>` reads the spans from that path instead [test: crates/replaykit/tests/prose_claims.rs::claim_extract_spans_override].
`--out <path|->` defaults to `-`, which prints the pretty-JSON trajectory summary to stdout [test: crates/replaykit/tests/prose_claims.rs::claim_extract_out_default_stdout].
Malformed span lines are skipped [test: crates/replaykit/src/trace.rs::load_skips_malformed_and_counts_them].

`replaykit verify <fixture.json>` recomputes a committed fixture's phases [test: crates/replaykit/src/verify.rs::phase_mismatch_fails], error count [test: crates/replaykit/src/verify.rs::error_step_over_ceiling_fails] and total cost [test: crates/replaykit/src/verify.rs::cost_over_cap_fails] from its steps and checks them against the fixture's `expect` block.
The total cost is checked only when `expect` pins a cost cap [test: crates/replaykit/src/verify.rs::no_cap_means_cost_is_not_checked].
Violations are printed to stderr [test: crates/replaykit/tests/prose_claims.rs::claim_verify_violations_to_stderr].

`replaykit promote --run <RID> [--spans <path>] [--root <dir>] [--evals-dir <name>] [--dataset <name>] [--draft]` builds the summary, writes it to `<root>/<evals_dir>/replay/fixtures/<id>.json`, and appends a golden line to `<root>/<evals_dir>/replay/<sanitize(dataset)>.jsonl` (re-promoting the same run adds no line) [test: crates/replaykit/tests/prose_claims.rs::claim_promote_paths_and_sanitized_dataset] [test: crates/replaykit/tests/prose_claims.rs::claim_promote_appends_distinct_runs].
With the default `--evals-dir`, the golden's `cmd` is `["replaykit","verify","evals/replay/fixtures/<id>.json"]`, a fixture path relative to root [test: crates/replaykit/tests/prose_claims.rs::claim_golden_cmd_relative_to_root].
`--root` defaults to the CWD, `--evals-dir` to `evals`, `--dataset` to `replayed` [test: crates/replaykit/tests/prose_claims.rs::claim_promote_defaults].
`--draft` is accepted but is currently a **no-op** [test: crates/replaykit/tests/prose_claims.rs::claim_draft_is_noop].

- `0`: replay matched the pinned invariants (pass) [test: crates/replaykit/tests/integration.rs::verify_healthy_fixture_passes]
- `1`: a real regression / invariant violation [test: crates/replaykit/tests/prose_claims.rs::claim_exit_1_regression]
- `2`: harness error: a missing or unreadable fixture or spans file, or a fixture that is not valid summary JSON [test: crates/replaykit/tests/prose_claims.rs::claim_exit_2_harness_error]

This is a plain CLI **gate**, not a lifecycle hook [test: crates/replaykit/tests/prose_claims.rs::claim_not_a_lifecycle_hook].

Its `[dependencies]` table lists only `harness-core`, `serde`, `serde_json` and `clap`, and its own `src/` contains no `TcpStream`, `UdpSocket`, `reqwest`, `ureq`, `hyper` or `API_KEY` reference [test: crates/replaykit/tests/prose_claims.rs::claim_deps_no_network_no_api_key].

This repository has no `.github/` CI, and no file directly under `.githooks/` mentions `evalkit` or `replaykit` [test: crates/replaykit/tests/prose_claims.rs::claim_no_ci_wiring].
