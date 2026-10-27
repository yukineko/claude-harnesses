# webqa

Claude Code plugin enforcing Playwright-based web QA on Stop. v0.1.0: only `weaken-check` works.

- `webqa weaken-check [--diff-file PATH]` — scans a unified diff (default `git diff --cached`) of
  Playwright spec files (`.spec`/`.test` + ts/js/tsx/mjs) for rules `skip-added`, `only-added`,
  `expect-removed`, `wait-timeout-added`, `timeout-raised`, `matcher-weakened`.
  Exit 0 clean, 1 findings (one stdout line per finding with path and rule id), 2 undetermined
  (unreadable file, unparseable input, git failure). Errors never map to 0.
- `webqa gate` — Stop hook. **Not implemented yet** (task t2); currently always blocks (fail-closed).
  Intended: `webqa.toml` maps changed code to specs, tri-state result, undetermined blocks.
- `webqa-gen` skill — planned; not shipped yet.

See `README.ja.md` for details.
