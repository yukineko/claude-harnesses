Offline **golden-regression eval harness**.

It reads **golden `*.jsonl` cases**, asserts over each subject, and exits non-zero when an invariant regresses.

One JSON object per line (`//` lines and blanks are skipped). A case names one
*subject* — a `file` (its contents) **or** a `cmd` (its stdout) — and assertions:

```jsonl
// a prompt invariant: flow must keep its hard rule
{"id":"flow-keeps-blind-exec","file":"crates/flow/skills/flow/SKILL.md","assert":{"contains":["盲目実行しない"]}}
// a CLI contract: compass nudge emits a machine verdict
{"id":"compass-nudge-json","cmd":["compass","nudge","--json"],"assert":{"exit":0,"regex":["\"fresh\"\\s*:\\s*(true|false)"]}}
```

- `id` — stable case name (required)
- `describe` — one-line human label (optional)
- `file` — read this file's contents as the subject (relative to `--root`)
- `cmd` — run `cmd[0]` with the rest as args; capture stdout as the subject
- `stdin` — optional stdin piped to a `cmd` subject
- `assert.exit` — expected exit code (`cmd` only)
- `assert.contains` / `not_contains` — substrings that must / must not appear
- `assert.regex` / `not_regex` — regexes that must / must not match
- `draft` — promotion draft awaiting a human-authored assertion; skipped at run time (never pass/fail)

A case has **exactly one** of `file` or `cmd` — except a `draft: true` case, which
carries no subject yet and is exempt from that requirement.

- Only **stdout** is the subject; text written only to stderr is not part of it.
- Acquisition failures (unreadable file, unspawnable cmd) are reported as case failures.
- `assert.exit` on a `file` case is itself a failure (a file has no exit code).

```sh
evalkit run
evalkit run --root . --bin-dir target/release
evalkit run --dir evals/replay
evalkit run --json
evalkit list
```

`--dir` is searched **recursively** for `*.jsonl`, so a subdir such
as `evals/replay/` is picked up with no config change.
`--dir` defaults to `evals`. `--root` (default: the CWD)
anchors `file` paths and `--dir`, and is the cwd of `cmd`
subprocesses. Blank lines and `//` lines are skipped; any other unparseable line
or a case with an empty `id` / both or neither subject makes the whole run a harness
error (exit 2). `list` accepts the same flags but always prints the plain list.

- `0` — all cases passed
- `1` — a real regression (an assertion failed)
- `2` — harness error (no cases found, unreadable eval file), or every case is a draft so nothing was checked

`--bin-dir DIR` is prepended to `PATH` for `cmd` cases.

A `draft` case is reported as `skip <label> (draft — assertion pending)` and counted
separately in the summary (`N draft(s) skipped`).

`evalkit canary` diffs two `evalkit run --json` outputs.

```sh
evalkit run --json > base.json
evalkit run --json > head.json
evalkit canary --baseline base.json --current head.json
evalkit canary --baseline base.json --current head.json --json
evalkit canary --baseline base.json --current head.json --report-only # always exit 0 unless the input is unusable.
```

It keys cases by their report label (the `case` field of `run --json`: `id`,
or `id — describe` when `describe` is set — so editing a `describe` shows up as one
dropped + one added) and classifies each as **regression** (pass→fail, neither side
a draft), **fix** (fail→pass), **added**, **dropped**, or **demoted** (pass→draft or fail→draft), and prints the pass-rate
before → after with the delta. The pass-rate excludes drafts from the denominator;
an all-draft or empty run counts as 0.0.

Read the `dropped` bucket yourself.

- `0` — no golden's verdict was lost, or `--report-only` was given
- `1` — a golden's verdict was lost: a pass→fail regression, a dropped golden, a pass→draft demotion, or a fail→draft demotion, which hides the failure
- `2` — harness error (unreadable / unparseable result file), or a side with no verdict-carrying case (all-draft or empty)

By default the canary **gates**: any lost golden exits 1.
`--report-only` is the explicit opt-in that makes it informational (exit 0).
`--report-only` does not soften exit 2: an all-draft side checked nothing.
The former `--fail-on-regression` flag is removed and is rejected.

- `evals/skill-invariants.jsonl` — hard rules pinned in plugin `SKILL.md`s.
- `evals/cli-contracts.jsonl` — CLI output/exit contracts.
- `evals/replay/replayed.jsonl` — promoted condukt trajectory replays
  (`cmd` cases asserting `replaykit verify` against fixtures under
  `evals/replay/fixtures/`).

Add a line whenever you codify a new invariant.

There is **no CI wiring**: the former `.github/workflows/eval.yml` no longer exists
(GitHub Actions is not used in this repo), and evalkit ships no hook. Run
`evalkit run` yourself after a build, for example `evalkit run --bin-dir target/release`.
