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
  The audit runs in the same package directory as `check pages`: `<root>/<dir>` when `command` is
  `npm --prefix <dir> …`, else the root. No lockfile there is exit 2 (the root's lockfile is not used
  instead); a `--prefix` that does not resolve to a directory inside the root (symlinks resolved) is
  exit 2 and npm is not run. The Stop gate audits the same directory.
- `sitecraft decide capability --name N --relevance relevant|irrelevant [--how H --done-when D
  --verify-kind K …]` — records `[capabilities.N]`, a capability beyond the web app. The verify kinds
  are exactly `builtin:blender-headless` (`--script --glb --render`), `builtin:threejs-render`
  (`--page /… [--selector S]`, default `canvas`) and `command` (repeated `--argv`, at least one
  `--input` glob). Invalid input is exit 2 with nothing changed. `decide check` is exit 1 for a
  capability without a relevance, or a relevant one missing how / done_when / verify, and exit 2 for an
  unknown key, a wrong type or an unknown kind.
- `sitecraft capability probe N` — prints one `{"capability","result","reason"}` line: `feasible` 0 /
  `infeasible` 1 / `undetermined` 2 (`timeout` after 120 s). Writes nothing; not completion evidence.
  Blender is `$SITECRAFT_BLENDER` when set (only that), else `blender` on `PATH`, else
  `/Applications/Blender.app/Contents/MacOS/Blender`.
- `sitecraft check capability N` — runs a relevant capability's verify (exit 2 `timeout` after 300 s):
  the blender kind deletes the old outputs, runs Blender headless and checks the GLB header (`glTF`,
  version 2, length = file size) and the render; the threejs kind needs a trusted `sitecraft.toml`,
  starts the dev server if it is stopped (and stops only a server it started) and screenshots the
  selector's element; the command kind runs the trusted argv in the root (exit 0 → 0, 2 → 2, else 1).
  Pixel rule: background = most frequent corner colour, a pixel is drawn when a channel differs by more
  than 16, at least 1 % must be drawn. Every run writes
  `<state>/capability-results/<sha256(root)>/<name>.json` (`{name, exit, input_hash,
  sitecraft_version, at}`) by temp file + rename; an unwritable record makes the check exit 2.
- `sitecraft trust` — a human records the sha256 of `sitecraft.toml`; an untrusted or changed file is never run.
  In the same write it records, under `<root>#capabilities`, the sha256 of the name-sorted
  `[[name, argv], …]` JSON of the `command` capabilities (removed when there is none). A changed argv,
  or an unreadable trust store, makes `check capability` exit 2 without running anything.
- Check history: `decide check`, `check pages`, `check security` and `check capability N` (from the
  CLI and from the gate) append one line per run to `<state>/check-history/<sha256(root)>.jsonl`:
  `{name, exit, fingerprint, elapsed_ms, timed_out, at}` (`name` = `decide` / `pages` / `security` /
  `capability:N`). An unappendable line (no state dir included) makes that check exit 2. The
  fingerprint is null on exit 0, `sha256("timeout")` on a time-out, else the sha256 of stdout+stderr
  with times → `<time>`, absolute paths → `<path>`, digit runs → `0`.
- `sitecraft status` — prints `{"checks":[{name, last_exit, consecutive_same_failure, elapsed_ms,
  state}]}`; state, first match: `waiting-human` (pages / threejs-render capability with an untrusted
  `sitecraft.toml`, command capability with an untrusted argv digest), `stuck` (3+ consecutive failures
  with the newest fingerprint), `failing`, `slow` (exit 0 in at least half the check's limit; never for
  `decide`), `ok`. Exit 0 all ok/slow, 1 any failing/stuck/waiting-human, 2 (empty stdout) for a
  missing, unreadable or unparseable history or an undeterminable state dir or trust.
- `sitecraft gate` — Stop hook under `harness_core::gate::run::run_guarded` (gate name `sitecraft`).
  No `sitecraft.toml`: allowed, one `not-configured` line appended to the state dir. Otherwise it
  combines trust, `decide check` (pending answers and V1-V5; the gate does **not** require
  `[purpose]`, only the CLI `decide check` does), `check security`, the result records of every relevant
  capability (missing, failed, other sitecraft version, changed inputs or unreadable → block naming
  `sitecraft check capability <name>`; the gate never runs Blender, Chromium or a capability command)
  and — only when the transcript shows a Write/Edit to a `web_globs` file or a Bash command naming one,
  or when that cannot be determined (including any Bash command not provably read-only) —
  `serve start` + `check pages` +
  `serve stop` with `Verdict::worst_of`. Bounded give-up after `max_attempts` with a
  `NOTHING WAS VERIFIED` systemMessage.

Exit codes for every subcommand except `gate`: `0` clean, `1` violation, `2` undetermined. `2` is
never clean. `gate` speaks the Stop-hook protocol (exit 0 + `{"decision":"block",…}`).

`bin/sitecraft` (100755) execs `sitecraft-<os>-<arch>`; without that binary, `gate` blocks (bounded
like `run_guarded`) and every other subcommand exits non-zero with an empty stdout.

Example config: [`sitecraft.example.toml`](sitecraft.example.toml).
