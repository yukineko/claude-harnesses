# benchkit

External-benchmark runner for the harness monorepo, targeting **SWE-bench
Verified**. This is the skeleton slice: a typed instance model, a deterministic
fixture-based JSONL loader, and a gated `download` subcommand. The scorer /
dashboard / harness layers land in later tasks.

## Why

condukt and evalkit measure the harness against *our own* invariants. benchkit
measures it against an *external, industry-standard* benchmark — SWE-bench
Verified — so improvements are comparable to the published numbers other agents
report. The loading path is pure and deterministic (no network, no clock), so
tests are hermetic; only `download` touches the network, and only when you ask.

## Instance model

One `Instance` = one benchmark task: a repo pinned at `base_commit`, the gold
`patch` that fixes it, a `test_patch` that adds/updates the grading tests, and
the two named test sets that grade a candidate:

| field | meaning |
|---|---|
| `instance_id` | stable unique id, e.g. `astropy__astropy-12907` |
| `repo` | `owner/name` of the GitHub repo under test |
| `base_commit` | commit checked out before any patch |
| `patch` | gold solution patch (unified diff) |
| `test_patch` | patch introducing/updating the grading tests |
| `problem_statement` | the natural-language issue / task |
| `hints_text` | optional maintainer hints |
| `created_at` | upstream timestamp (verbatim) |
| `version` | project version label |
| `fail_to_pass` | tests that must go red→green (`FAIL_TO_PASS`) |
| `pass_to_pass` | tests that must stay green (`PASS_TO_PASS`) |
| `environment_setup_commit` | commit whose env/deps to set up against |

The upstream JSONL uses upper-case `FAIL_TO_PASS` / `PASS_TO_PASS` keys; the
model renames them onto snake_case Rust fields via serde. The normalized shape
(what `download` produces, and what the vendored fixture uses) encodes those as
a plain JSON list of strings.

## Usage

```sh
# Load a JSONL split into typed instances (deterministic, offline):
benchkit load crates/benchkit/tests/fixtures/instances.jsonl

# Fetch the real dataset into a local cache (idempotent; network only here):
benchkit download                 # → .benchkit-cache/swe-bench-verified.jsonl
benchkit download --dest data/verified.jsonl
benchkit download --force         # re-fetch even if cached
```

`download` is **gated**: its network path is reached only on explicit
invocation and is a no-op when the cache already exists (idempotent). It never
runs during `cargo test`. Following the beacon house pattern, network I/O shells
out to `curl` with a hard timeout rather than linking an HTTP client, keeping
the bundled binary tiny.

## Dataset source & license

- **Dataset:** SWE-bench Verified — a 500-task human-validated subset of
  SWE-bench.
- **Publisher:** Princeton NLP (`princeton-nlp/SWE-bench_Verified` on
  HuggingFace, <https://huggingface.co/datasets/princeton-nlp/SWE-bench_Verified>).
- **License:** the dataset is distributed under its upstream terms (MIT for the
  SWE-bench tooling; the referenced repositories carry their own licenses).
  benchkit only *fetches* the published split on explicit request and does not
  redistribute it — the vendored fixture under `tests/fixtures/` is a tiny
  hand-authored sample for offline tests, not a copy of the dataset.

## Determinism

The loader is pure: same file → same `Vec<Instance>`, in file order, with a
clear `path:line` error on a malformed row. Nothing outside `download` performs
network or environment I/O.
