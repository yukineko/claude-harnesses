# evalkit

Offline **golden-regression eval harness** for the harness monorepo — the
*offline sibling* of condukt's online Phase-6 verifier.

condukt's verifier runs *per task, online*: it spins up a sub-agent to judge a
fresh diff. That catches regressions in **new** work, but nothing re-checks the
guardrails already baked into the plugins — the hard rules in a `SKILL.md`, the
shape of a `--json` CLI contract. A careless prompt edit can quietly delete
"`盲目実行しない`" from flow's skill and no online verifier would ever notice.

evalkit closes that gap. It reads **golden `*.jsonl` cases**, asserts over each
subject deterministically, and exits non-zero when an invariant regresses — with
**no API key**, so it runs as a CI gate and a `/flow` pre-release check.

## Case format

One JSON object per line (`//` lines and blanks are skipped). A case names one
*subject* — a `file` (its contents) **or** a `cmd` (its stdout) — and assertions:

```jsonl
// a prompt invariant: flow must keep its hard rule
{"id":"flow-keeps-blind-exec","file":"crates/flow/skills/flow/SKILL.md","assert":{"contains":["盲目実行しない"]}}
// a CLI contract: compass nudge emits a machine verdict
{"id":"compass-nudge-json","cmd":["compass","nudge","--json"],"assert":{"exit":0,"regex":["\"fresh\"\\s*:\\s*(true|false)"]}}
```

| field | meaning |
|---|---|
| `id` | stable case name (required) |
| `describe` | one-line human label (optional) |
| `file` | read this file's contents as the subject (relative to `--root`) |
| `cmd` | run `cmd[0]` with the rest as args; capture stdout as the subject |
| `stdin` | optional stdin piped to a `cmd` subject |
| `assert.exit` | expected exit code (`cmd` only) |
| `assert.contains` / `not_contains` | substrings that must / must not appear |
| `assert.regex` / `not_regex` | regexes that must / must not match |
| `draft` | promotion draft awaiting a human-authored assertion; skipped at run time (never pass/fail) |

A case has **exactly one** of `file` or `cmd` — except a `draft: true` case, which
carries no subject yet and is exempt from that requirement.

## Usage

```sh
evalkit run                                   # discover ./evals/*.jsonl, assert, exit non-zero on failure
evalkit run --root . --bin-dir target/release # resolve `cmd` programs from a fresh build
evalkit run --json                            # machine-readable summary
evalkit list                                  # show discovered cases without running them
```

Exit codes — CI can tell a regression from a misconfigured path:

| code | meaning |
|---|---|
| `0` | all cases passed |
| `1` | a real regression (an assertion failed) |
| `2` | harness error (no cases found, unreadable eval file) |

`--bin-dir DIR` is prepended to `PATH` for `cmd` cases, so a just-built
`target/release/<tool>` is exercised without installing it.

A `draft` case is reported as `skip ... (draft — assertion pending)` and counted
separately — it never contributes to the pass/fail exit code.

## Canary: replay the same goldens across two versions

`evalkit canary` diffs two `evalkit run --json` outputs — the same golden set
replayed at two points (a PR's base vs head, an old vs new SKILL.md). It is the
side-by-side half of the skill-fingerprint loop: when a prompt edit changes
behaviour, the canary shows *which goldens moved*.

```sh
evalkit run --json > base.json        # at the old version
evalkit run --json > head.json        # at the new version
evalkit canary --baseline base.json --current head.json
evalkit canary --baseline base.json --current head.json --json              # machine-readable delta
evalkit canary --baseline base.json --current head.json --fail-on-regression # exit 1 on any pass→fail
```

It keys cases by `id` and classifies each as **regression** (pass→fail),
**fix** (fail→pass), **added**, or **dropped**, and prints the pass-rate
before → after with the delta.

| code | meaning |
|---|---|
| `0` | reported (or no regressions when `--fail-on-regression`) |
| `1` | `--fail-on-regression` set **and** ≥1 pass→fail transition |
| `2` | harness error (unreadable / unparseable result file) |

By default it is **informational** (exit 0) — `eval.yml`'s `canary` job annotates
PRs without gating. Pass `--fail-on-regression` to turn it into a hard gate.

## Where the goldens live

Repo-root `evals/`:

- `evals/skill-invariants.jsonl` — hard rules pinned in plugin `SKILL.md`s.
- `evals/cli-contracts.jsonl` — CLI output/exit contracts.
- `evals/replay/replayed.jsonl` — promoted condukt trajectory replays
  (`cmd` cases asserting `replaykit verify` against fixtures under
  `evals/replay/fixtures/`).

Add a line whenever you codify a new invariant. The keystone of the LLMOps eval
layer: future curation (`curate`) promotes high-signal fugu episodes/playbooks
into goldens here, and `/flow` can run `evalkit run` as a pre-release gate.

## CI

`.github/workflows/eval.yml` builds the workspace and runs `evalkit run
--bin-dir target/release` on every push/PR. A dropped invariant turns the job
red before it merges.

Subscription-native: one bundled Rust binary, no API key.
