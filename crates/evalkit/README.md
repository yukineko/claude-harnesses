Offline **golden-regression eval harness** [test: crates/evalkit/tests/integration.rs::help_exits_zero_and_describes_the_harness].

It reads **golden `*.jsonl` cases**, asserts over each subject, and exits non-zero when an invariant regresses [test: crates/evalkit/tests/prose_claims.rs::claim_run_exit_on_regress].

One JSON object per line (`//` lines and blanks are skipped) [test: crates/evalkit/src/case.rs::parses_file_and_cmd_cases_skipping_comments_and_blanks]. A case names one
*subject* — a `file` (its contents) **or** a `cmd` (its stdout) — and assertions [test: crates/evalkit/src/case.rs::parses_file_and_cmd_cases_skipping_comments_and_blanks] [test: crates/evalkit/tests/prose_claims.rs::claim_file_subject_rel_root] [test: crates/evalkit/tests/prose_claims.rs::claim_cmd_subject_stdout]:

```jsonl
// a prompt invariant: flow must keep its hard rule
{"id":"flow-keeps-blind-exec","file":"crates/flow/skills/flow/SKILL.md","assert":{"contains":["盲目実行しない"]}}
// a CLI contract: compass nudge emits a machine verdict
{"id":"compass-nudge-json","cmd":["compass","nudge","--json"],"assert":{"exit":0,"regex":["\"fresh\"\\s*:\\s*(true|false)"]}}
```

- `id` — stable case name (required) [test: crates/evalkit/tests/prose_claims.rs::claim_id_required]
- `describe` — one-line human label (optional) [test: crates/evalkit/tests/prose_claims.rs::claim_describe_label]
- `file` — read this file's contents as the subject (relative to `--root`) [test: crates/evalkit/tests/prose_claims.rs::claim_file_subject_rel_root]
- `cmd` — run `cmd[0]` with the rest as args; capture stdout as the subject [test: crates/evalkit/tests/prose_claims.rs::claim_cmd_subject_stdout]
- `stdin` — optional stdin piped to a `cmd` subject [test: crates/evalkit/tests/prose_claims.rs::claim_stdin_piped]
- `assert.exit` — expected exit code (`cmd` only) [test: crates/evalkit/src/run.rs::exit_mismatch_reported] [test: crates/evalkit/tests/prose_claims.rs::claim_cmd_subject_stdout]
- `assert.contains` / `not_contains` — substrings that must / must not appear [test: crates/evalkit/src/run.rs::contains_and_not_contains]
- `assert.regex` / `not_regex` — regexes that must / must not match [test: crates/evalkit/src/run.rs::regex_match_and_forbidden]
- `draft` — promotion draft awaiting a human-authored assertion; skipped at run time (never pass/fail) [test: crates/evalkit/src/run.rs::draft_case_is_skipped_not_failed]

A case has **exactly one** of `file` or `cmd` — except a `draft: true` case, which
carries no subject yet and is exempt from that requirement [test: crates/evalkit/src/case.rs::rejects_case_with_both_subjects] [test: crates/evalkit/src/case.rs::rejects_case_with_no_subject] [test: crates/evalkit/src/case.rs::draft_case_is_exempt_from_subject_requirement].

- Only **stdout** is the subject; text written only to stderr is not part of it [test: crates/evalkit/tests/prose_claims.rs::claim_stdout_only].
- Acquisition failures (unreadable file, unspawnable cmd) are reported as case failures [test: crates/evalkit/tests/prose_claims.rs::claim_acquire_fail].
- `assert.exit` on a `file` case is itself a failure (a file has no exit code) [test: crates/evalkit/src/run.rs::exit_on_file_subject_is_a_failure].

```sh
evalkit run
evalkit run --root . --bin-dir target/release
evalkit run --dir evals/replay
evalkit run --json
evalkit list
```

`--dir` is searched **recursively** for `*.jsonl`, so a subdir such
as `evals/replay/` is picked up with no config change [test: crates/evalkit/src/run.rs::discover_recurses_into_subdirs].
`--dir` defaults to `evals` [test: crates/evalkit/tests/prose_claims.rs::claim_dir_default_evals]. `--root` (default: the CWD)
anchors `file` paths and `--dir`, and is the cwd of `cmd`
subprocesses [test: crates/evalkit/tests/prose_claims.rs::claim_root_anchors]. Blank lines and `//` lines are skipped; any other unparseable line
or a case with an empty `id` / both or neither subject makes the whole run a harness
error (exit 2) [test: crates/evalkit/tests/prose_claims.rs::claim_parse_errors_exit2]. `list` accepts the same flags but always prints the plain list [test: crates/evalkit/tests/prose_claims.rs::claim_list_plain].

- `0` — all cases passed [test: crates/evalkit/tests/prose_claims.rs::claim_exit_codes_run]
- `1` — a real regression (an assertion failed) [test: crates/evalkit/tests/prose_claims.rs::claim_exit_codes_run]
- `2` — harness error (no cases found, unreadable eval file) [test: crates/evalkit/tests/prose_claims.rs::claim_exit_codes_run]

`--bin-dir DIR` is prepended to `PATH` for `cmd` cases [test: crates/evalkit/tests/prose_claims.rs::claim_bin_dir_path].

A `draft` case is reported as `skip <label> (draft — assertion pending)` and counted
separately in the summary (`N draft(s) skipped`) [test: crates/evalkit/tests/prose_claims.rs::claim_draft_reported].

`evalkit canary` diffs two `evalkit run --json` outputs [test: crates/evalkit/tests/prose_claims.rs::claim_canary_label_keys].

```sh
evalkit run --json > base.json
evalkit run --json > head.json
evalkit canary --baseline base.json --current head.json
evalkit canary --baseline base.json --current head.json --json
evalkit canary --baseline base.json --current head.json --fail-on-regression # exit 1 on any pass→fail [test: crates/evalkit/src/canary.rs::fail_on_regression_exit_policy].
```

It keys cases by their report label (the `case` field of `run --json`: `id`,
or `id — describe` when `describe` is set — so editing a `describe` shows up as one
dropped + one added) and classifies each as **regression** (pass→fail, neither side
a draft) [test: crates/evalkit/src/canary.rs::skipped_transitions_are_not_regressions], **fix** (fail→pass), **added**, or **dropped**, and prints the pass-rate
before → after with the delta [test: crates/evalkit/tests/prose_claims.rs::claim_canary_label_keys]. The pass-rate excludes drafts from the denominator;
an all-draft or empty run counts as 0.0 [test: crates/evalkit/src/canary.rs::pass_rate_math_and_divide_by_zero].

Read the `dropped` bucket yourself [test: crates/evalkit/tests/prose_claims.rs::claim_canary_dropped_bucket].

- `0` — reported (or no regressions when `--fail-on-regression`) [test: crates/evalkit/src/canary.rs::fail_on_regression_exit_policy]
- `1` — `--fail-on-regression` set **and** ≥1 pass→fail transition [test: crates/evalkit/src/canary.rs::fail_on_regression_exit_policy]
- `2` — harness error (unreadable / unparseable result file) [test: crates/evalkit/tests/prose_claims.rs::claim_canary_exit2]

By default it is **informational** (exit 0) [test: crates/evalkit/src/canary.rs::fail_on_regression_exit_policy].
Pass `--fail-on-regression` to exit 1 on any pass→fail transition [test: crates/evalkit/src/canary.rs::fail_on_regression_exit_policy].

- `evals/skill-invariants.jsonl` — hard rules pinned in plugin `SKILL.md`s [test: crates/evalkit/tests/prose_claims.rs::claim_evals_files].
- `evals/cli-contracts.jsonl` — CLI output/exit contracts [test: crates/evalkit/tests/prose_claims.rs::claim_evals_files].
- `evals/replay/replayed.jsonl` — promoted condukt trajectory replays
  (`cmd` cases asserting `replaykit verify` against fixtures under
  `evals/replay/fixtures/`) [test: crates/evalkit/tests/prose_claims.rs::claim_evals_files].

Add a line whenever you codify a new invariant.

There is **no CI wiring**: the former `.github/workflows/eval.yml` no longer exists
(GitHub Actions is not used in this repo), and evalkit ships no hook [test: crates/evalkit/tests/prose_claims.rs::claim_no_ci_no_hook]. Run
`evalkit run` yourself after a build, for example `evalkit run --bin-dir target/release` [test: crates/evalkit/tests/prose_claims.rs::claim_nothing_runs_it].
