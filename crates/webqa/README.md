# webqa

Claude Code plugin enforcing Playwright-based web QA on Stop (v0.1.6).

- `webqa gate` — Stop hook. Reads `webqa.toml` from the project root (`git rev-parse
  --show-toplevel` of the hook cwd; outside any repository the cwd itself), maps the files touched
  (git diff vs HEAD plus untracked) to Playwright specs, runs
  `<command> --reporter=json <specs>`, and blocks the stop unless the run provably passed.
  No `webqa.toml` => "not configured", allowed (stderr says so). A config that was never trusted,
  or changed since `webqa trust`, blocks with "untrusted" and the command is not spawned.
  Every selected spec must appear in the report's `suites[].file` (nested suites included;
  `/`-boundary suffix match) with at least one non-skipped test, else it blocks naming the spec.
  Every cannot-determine case blocks (bad config, `timeout_secs` above 540, unreadable trust
  store, git failure other than "not a repository" with no config, a config in a non-git dir,
  missing/empty spec mapping, spawn failure, timeout, non-JSON stdout, nothing ran,
  exit/stats mismatch, runner errors); flaky tests block.
  Panics go through `harness_core::gate::run::run_guarded` (fail-closed, bounded). After
  `max_attempts` blocked stops in one session the gate gives up with a loud
  `systemMessage` ("NOTHING WAS VERIFIED"). Only a trusted config sets `max_attempts` (clamped to
  1..=10) and `reset_after_secs`; an untrusted, unreadable or unparseable config never sets its own
  budget — the built-in default (3 attempts, 1800 s) applies. The config is read once and the same
  bytes are hashed for trust and parsed. State: `$WEBQA_STATE_DIR`, else `~/.webqa`.
  Config format: `webqa.example.toml`.
- `webqa trust` — records the sha256 of `<root>/webqa.toml` (root resolved like the gate) keyed by
  the canonical root in `<state>/trust.json`. Re-run after every config change. A corrupt store is
  never overwritten (the command fails).
- `webqa weaken-check [--diff-file PATH]` — scans a unified diff (default `git diff --cached`, run with `--no-ext-diff --no-textconv
  --no-color --no-relative`, fixed `a/`/`b/` prefixes and `core.quotePath=true`, so no external diff
  driver, textconv attribute or prefix config can change the patch) of
  Playwright spec files (`.spec`/`.test` + ts/js/tsx/jsx/mjs/mts/cjs/cts; matched on the old or the new
  path, so a rename out of a spec name is still inspected). Paths in `---`/`+++` headers are read
  up to git's trailing TAB; C-quoted paths (`"a/x\"y.spec.ts"`) are decoded (malformed quoting
  = undetermined). Rules: `skip-added`, `only-added`, `slow-added` (`test.slow()`),
  `expect-removed`, `wait-timeout-added`, `timeout-raised` (file-level; evaluates `60_000`,
  `60 * 1000`, `(5 * 60_000)`; `0` disables the timeout and counts as infinite; a new timeout
  above 30000 ms with none removed also counts, and so does removing a timeout below 30000 ms with
  none added, since it reverts to the default), `matcher-weakened`.
  Exit 0 clean, 1 findings (one stdout line per finding with path and rule id), 2 undetermined
  (unreadable file, unparseable input, git failure, binary diff of a spec, any non-evaluable
  added timeout expression — identifier, parenthesized, operator-leading or empty — whether it
  replaces a literal or is added with none removed (it may exceed the 30000 ms default), and any
  non-evaluable removed timeout expression, whether nothing or a literal is added in its place (its
  value is unknown, so the replacement or the 30000 ms default may be larger);
  only a `setTimeout(` callback is not treated as a timeout). Errors never map to 0.
- `webqa-gen` skill — shipped (`skills/webqa-gen/SKILL.md`): spec/URL -> browser exploration ->
  test-first Playwright spec -> RED observation -> adoption.

See `README.ja.md` for details.
