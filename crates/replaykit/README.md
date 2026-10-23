`replaykit promote` turns a tracekit run trace (`spans.jsonl`) into an evalkit golden case whose `cmd` re-runs `replaykit verify` on the promoted fixture, and that `cmd` exits 0 on the fixture as promoted and 1 once a fixture step's status is changed to an error.

```sh
replaykit promote --run my-run-2026-06-28 --root . --dataset replayed
#    writes  evals/replay/fixtures/<id>.json      (the portable summary)
#       and  evals/replay/replayed.jsonl          (the golden; re-promoting the same run adds no line)
```

Without `--spans`, `replaykit extract --run <RID>` reads spans from `~/.tracekit/<sanitize(RID)>/spans.jsonl`.
`--spans <path>` reads the spans from that path instead.
`--out <path|->` defaults to `-`, which prints the pretty-JSON trajectory summary to stdout.
Malformed span lines are skipped.

`replaykit verify <fixture.json>` recomputes a committed fixture's phases, error count and total cost from its steps and checks them against the fixture's `expect` block.
The total cost is checked only when `expect` pins a cost cap.
Violations are printed to stderr.

`replaykit promote --run <RID> [--spans <path>] [--root <dir>] [--evals-dir <name>] [--dataset <name>] [--draft]` builds the summary, writes it to `<root>/<evals_dir>/replay/fixtures/<id>.json`, and appends a golden line to `<root>/<evals_dir>/replay/<sanitize(dataset)>.jsonl` (re-promoting the same run adds no line).
With the default `--evals-dir`, the golden's `cmd` is `["replaykit","verify","evals/replay/fixtures/<id>.json"]`, a fixture path relative to root.
`--root` defaults to the CWD, `--evals-dir` to `evals`, `--dataset` to `replayed`.
`--draft` is accepted but is currently a **no-op**.

- `0`: replay matched the pinned invariants (pass)
- `1`: a real regression / invariant violation
- `2`: harness error: a missing or unreadable fixture or spans file, or a fixture that is not valid summary JSON

This is a plain CLI **gate**, not a lifecycle hook.

Its `[dependencies]` table lists only `harness-core`, `serde`, `serde_json` and `clap`, and its own `src/` contains no `TcpStream`, `UdpSocket`, `reqwest`, `ureq`, `hyper` or `API_KEY` reference.

This repository has no `.github/` CI, and no file directly under `.githooks/` mentions `evalkit` or `replaykit`.
