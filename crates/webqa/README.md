# webqa

Claude Code plugin enforcing Playwright-based web QA on Stop (v0.1.2).

- `webqa gate` — Stop hook. Reads `webqa.toml` from the hook cwd, maps the files touched
  (git diff vs HEAD plus untracked) to Playwright specs, runs
  `<command> --reporter=json <specs>`, and blocks the stop unless the run provably passed.
  No `webqa.toml` => "not configured", allowed (stderr says so). Every cannot-determine case
  blocks (bad config, git failure, missing/empty spec mapping, spawn failure, timeout,
  non-JSON stdout, nothing ran, exit/stats mismatch, runner errors); flaky tests block.
  Panics go through `harness_core::gate::run::run_guarded` (fail-closed, bounded). After
  `max_attempts` (default 3) blocked stops in one session the gate gives up with a loud
  `systemMessage` ("NOTHING WAS VERIFIED"). State: `$WEBQA_STATE_DIR`, else `~/.webqa`.
  Config format: `webqa.example.toml`.
- `webqa weaken-check [--diff-file PATH]` — scans a unified diff (default `git diff --cached`) of
  Playwright spec files (`.spec`/`.test` + ts/js/tsx/jsx/mjs/mts/cjs/cts; matched on the old or the new
  path, so a rename out of a spec name is still inspected). Rules: `skip-added`, `only-added`,
  `expect-removed`, `wait-timeout-added`, `timeout-raised` (file-level; evaluates `60 * 1000`;
  a new timeout above 30000 ms with none removed also counts), `matcher-weakened`.
  Exit 0 clean, 1 findings (one stdout line per finding with path and rule id), 2 undetermined
  (unreadable file, unparseable input, git failure, binary diff of a spec, a non-evaluable
  timeout expression replacing a literal). Errors never map to 0.
- `webqa-gen` skill — shipped (`skills/webqa-gen/SKILL.md`): spec/URL -> browser exploration ->
  test-first Playwright spec -> RED observation -> adoption.

See `README.ja.md` for details.
