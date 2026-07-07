# curate

Promote **fugu-router playbooks** into versioned **golden eval datasets** for
[evalkit](../evalkit) — the supply side of the offline eval loop.

evalkit consumes goldens; until now nothing produced them from real verified
work. fugu-router records one playbook per verified task (`title`,
`done_criteria`, `touched_files`) to `~/.fugu-router/playbooks.jsonl` — but that
log is policy-search only: append-only, never holdout-curated, no `input→expected`
test. curate closes the gap: it distils a chosen entry into an evalkit golden
case, fixed into a versioned, deduplicated dataset.

## The honest mapping

A playbook is a *procedure*, not a test. So curate auto-derives a runnable case
only when the acceptance criterion is **mechanical**, and drafts the rest:

| `done_criteria` | promoted golden |
|---|---|
| `` `cargo test --workspace` passes `` | `{"cmd":["cargo","test","--workspace"],"assert":{"exit":0}}` |
| "cargo test -p evalkit is green" | `{"cmd":["cargo","test","-p","evalkit"],"assert":{"exit":0}}` |
| "auth handles token refresh" | `{"draft":true,"describe":"… — TODO assert done_criteria: …"}` |

Mechanical signals: an explicit backticked command, or a recognised test runner
(`cargo test`, `npm test`, `pytest`, `go test`). A **draft** is a valid golden
that evalkit *skips* (never pass, never fail) until a human writes its assertion
— so it can sit in the repo as visible pending work without breaking CI.

## Usage

```sh
curate candidates                       # list promotable playbooks (mech | draft)
curate promote "add login" --dataset auth   # → evals/curated/auth.jsonl
curate promote --latest                 # the most recent playbook
curate promote "x" --draft              # force a draft even if mechanical
curate promote --latest --store /path/to/playbooks.jsonl  # override the playbook store
```

Both `candidates` and `promote` accept `--store` to override the playbook store
path (default `~/.fugu-router/playbooks.jsonl`). Promotions append to
`evals/curated/<name>.jsonl` (deduplicated by case id) under `--root` (default
CWD), inside the `--evals-dir` (default `evals`). evalkit discovers `evals/`
**recursively**, so a promoted case is picked up by `evalkit run` and the
`eval.yml` CI gate with no config change.

## The loop it closes

```
condukt verifies a task ─▶ fugu-router record (playbook)
        ▲                              │
        │                    curate promote
   evalkit run  ◀── evals/curated/*.jsonl ◀┘   (eval.yml gates every push)
```

After `record`, condukt's Phase 6 is HOTL-wired to `curate promote`: when a
verified task's `done_criteria` is mechanical and `curate` is on PATH, the main
loop asks **exactly one** confirmation ("promote this verified run to an eval
golden?") and, only on an affirmative answer, actually shells out to
`curate promote` — declining writes nothing. Subscription-native: one bundled
Rust binary, no API key.
