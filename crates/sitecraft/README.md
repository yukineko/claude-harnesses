# sitecraft

Claude Code plugin for building site / web-app prototypes (v0.1.0). Japanese details: [README.ja.md](README.ja.md).

The `/sitecraft` skill asks the user every design decision the purpose needs, records them in
`sitecraft/decisions.toml`, and only then scaffolds (Vite by default), implements, delegates E2E to
`/webqa-gen`, and checks. This crate is the deterministic half.

- `sitecraft decide next|record|check|add-question|classify` — the decision record.
- `sitecraft serve start|stop|status` — dev-server manager for the trusted `sitecraft.toml` `command`.
- `sitecraft check pages` — opens the configured pages with the bundled Playwright script and reports
  console errors, page errors, asset HTTP errors and broken links.
- `sitecraft check security` — secrets (printed redacted) and `.env` values in build output, dangerous
  HTML sinks in sources, `npm audit --json` high/critical (judged from the JSON shape, not the exit code).
- `sitecraft trust` — a human records the sha256 of `sitecraft.toml`; an untrusted or changed file is never run.
- `sitecraft gate` — Stop hook under `harness_core::gate::run::run_guarded` (gate name `sitecraft`).
  No `sitecraft.toml`: allowed, one `not-configured` line appended to the state dir. Otherwise it
  combines trust, `decide check`, `check security` and — only when the transcript shows a
  Write/Edit to a `web_globs` file, or when that cannot be determined — `serve start` + `check pages` +
  `serve stop` with `Verdict::worst_of`. Bounded give-up after `max_attempts` with a
  `NOTHING WAS VERIFIED` systemMessage.

Exit codes for every subcommand except `gate`: `0` clean, `1` violation, `2` undetermined. `2` is
never clean. `gate` speaks the Stop-hook protocol (exit 0 + `{"decision":"block",…}`).

`bin/sitecraft` (100755) execs `sitecraft-<os>-<arch>`; without that binary, `gate` blocks (bounded
like `run_guarded`) and every other subcommand exits non-zero with an empty stdout.

Example config: [`sitecraft.example.toml`](sitecraft.example.toml).
