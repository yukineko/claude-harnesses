`curate promote` turns a chosen playbook entry (a JSON line with `title` and `done_criteria`) into a golden case line with an `id`.

curate auto-derives a runnable case only when the acceptance criterion (`done_criteria`) is **mechanical**. It drafts the rest.

- `` `cargo test --workspace` passes `` gets `"cmd":["cargo","test","--workspace"]` and `"assert":{"exit":0}`.
- "cargo test -p evalkit is green" gets `"cmd":["cargo","test","-p","evalkit"]` and `"assert":{"exit":0}`.
- "auth handles token refresh" becomes `{"draft":true,"describe":"… — TODO assert done_criteria: …"}`.

Mechanical signals: an explicit backticked command, or a recognised test runner (`cargo test`, `npm test` and `pytest`). `go test` is also a recognised test runner.

evalkit skips a draft case: it is neither a pass nor a fail.

```sh
curate candidates                       # list promotable playbooks (mech | draft), newest first; --k N limits the count (default 20)
curate promote "add login" --dataset auth   # → evals/curated/auth.jsonl
curate promote --latest                 # the most recent playbook
curate promote "x" --draft              # force a draft even if mechanical
curate promote --latest --store /path/to/playbooks.jsonl  # override the playbook store
```

`promote` accepts `--store` to read the playbook store from another path. `candidates` reads the playbook store from `--store` too. The default store path is `~/.fugu-router/playbooks.jsonl`.

`promote` selects by case-insensitive title substring, and the most recent match wins. `promote --latest` selects the newest playbook.

Promotions append to `<root>/<evals-dir>/curated/<name>.jsonl`, where `--evals-dir` defaults to `evals`. `--dataset` defaults to `promoted`. A promotion is deduplicated by case id. `--root` defaults to the CWD.

evalkit discovers `*.jsonl` goldens recursively, so cases under `curated/` are found.
