# sitecraft

Claude Code plugin for building site / web-app prototypes (v0.1.0). Japanese details: [README.ja.md](README.ja.md).

The `/sitecraft` skill asks the user every design decision the purpose needs, records them in
`sitecraft/decisions.toml`, and only then scaffolds (Vite by default), implements, delegates E2E to
`/webqa-gen`, and checks. This crate is the deterministic half.

- `sitecraft decide next|record|purpose|check|add-question|classify` — the decision record.
  - `decide purpose --text T` records `[purpose] text` (trimmed; blank text is exit 1, record unchanged).
    `decide next` never asks it; `decide check` is exit 1 until it is recorded.
  - `decide record … --answer V [--answer V2]… [--note T]`: repeated `--answer` only for multi questions
    (`notification.channel` in the catalog, `add-question --multi` for domain questions), stored as an
    array; `--note` stores a remark and is kept when omitted. A changed value appends
    `{area, question, old, new, at}` (UTC RFC 3339) to `[[history]]` and prints
    `changed: <old> -> <new>`; answers dropped by follow-up pruning are kept as `pruned = true` rows.
  - `decide check` applies the closed rule table in `src/decide/rules.rs` to catalog answers only:
    W1 (auth relevant, exposure irrelevant) is a stderr `warning: W1: …` that never changes the exit
    code; V1-V5 are violations (exit 1). Domain questions are not judged.
  - `decide classify` adds `description` and `reuse` (`unconfirmed` / `confirmed` / `rejected` /
    `not-applicable`) to its JSON; `--confirm-fit yes|no` records the user's fit answer, only when
    `existing` is true.
- `sitecraft init <dir>` — `git init` plus `<dir>/sitecraft/` for a missing or empty `<dir>` (exit 0);
  a non-empty directory, a file, or a path inside another git work tree is exit 1 with nothing changed;
  git missing / failing / over 30 s in total, or an IO failure, is exit 2. The scaffold target is
  `<dir>/app/`.
- `sitecraft serve start|stop|status` — dev-server manager for the trusted `sitecraft.toml` `command`.
  The ready probe asks both `127.0.0.1` and `[::1]` and records the address that answered as `url`.
- `sitecraft check pages` — opens the configured pages with the bundled Playwright script and reports
  console errors, page errors, asset HTTP errors and broken links. node runs in `<root>/<dir>` when
  `command` is `npm --prefix <dir> …` (Playwright resolves from there and its ancestors), else in the
  root. Then each `api_paths` entry gets one `GET` (redirects not followed): non-2xx or no connection
  is exit 1, more than 10 s for one path is exit 2.
- `sitecraft check security` — secrets (printed redacted) and `.env` values in build output, dangerous
  HTML sinks in sources, `npm audit --json` high/critical (judged from the JSON shape, not the exit code).
- `sitecraft trust` — a human records the sha256 of `sitecraft.toml`; an untrusted or changed file is never run.
- `sitecraft gate` — Stop hook under `harness_core::gate::run::run_guarded` (gate name `sitecraft`).
  No `sitecraft.toml`: allowed, one `not-configured` line appended to the state dir. Otherwise it
  combines trust, `decide check` (pending answers and V1-V5; the gate does **not** require
  `[purpose]`, only the CLI `decide check` does), `check security` and — only when the transcript shows a
  Write/Edit to a `web_globs` file, or when that cannot be determined — `serve start` + `check pages` +
  `serve stop` with `Verdict::worst_of`. Bounded give-up after `max_attempts` with a
  `NOTHING WAS VERIFIED` systemMessage.

Exit codes for every subcommand except `gate`: `0` clean, `1` violation, `2` undetermined. `2` is
never clean. `gate` speaks the Stop-hook protocol (exit 0 + `{"decision":"block",…}`).

`bin/sitecraft` (100755) execs `sitecraft-<os>-<arch>`; without that binary, `gate` blocks (bounded
like `run_guarded`) and every other subcommand exits non-zero with an empty stdout.

Example config: [`sitecraft.example.toml`](sitecraft.example.toml).
