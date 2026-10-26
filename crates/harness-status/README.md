It is **read-only** — it writes nothing under HOME, the working directory or TMPDIR, and only aggregates other plugins' stores.

It does **not** register a general-purpose `SessionStart` dashboard hook.

**Unified HOTL status dashboard for Claude Code**.

- **Budget** (budgetguard): today's spend and session count, from `~/.budgetguard/state/ledger.json`.
- **Recent sessions** (gauge): the last N sessions with turns, tokens, and USD cost, from `~/.gauge/store/sessions/`.
- **Progress**: the current `.claude/progress.md` preview.
- **Stop-hook latency**: per-session totals from `~/.harness/state/hook-latency.jsonl`.
- **UserPromptSubmit injection size**: per-turn totals from `~/.harness/state/inject-metrics.jsonl`.
- **Hook binary health**: hooks registered in `~/.claude/settings.json` whose binary is missing on disk.
- **PATH shadowing**: a stray binary on `PATH` that shadows a plugin-cache binary of the same name.

It registers exactly one hook: a `SessionStart` hook (`hooks/hooks.json`) that runs the hooks-binary health check (`harness-status session-start`).
It injects `additionalContext` when a hook registered in `~/.claude/settings.json` points at a binary missing from disk.
It injects `additionalContext` when a plugin-cache binary is shadowed by a stray `PATH` copy.
It injects `additionalContext` when the PATH-shadow scan itself could not complete (reported as undetermined).
Otherwise it is silent (no output, no injected context).

```
╔══════════════════════════════════════════════╗
║         harness-status  (2026-06-23)         ║
╚══════════════════════════════════════════════╝

── Budget (budgetguard) ──────────────────────────
  Today spend:  $1.8420  (3 session(s))

── Recent sessions (gauge) ───────────────────────
  Session          Project              Turns       Tokens  Cost USD
  ----------------------------------------------------------------------
  3c8d91a2         harness                 12        35000    0.1850

── Progress file ─────────────────────────────────
  cwd: /repo
  /repo/.claude/progress.md
  │ - specforge ⑤ worktree merge
```

```
/plugin install harness-status@yukineko
```

Then run `/status` in any session.

```sh
cargo install --path .
harness-status
```

```sh
harness-status                       # full dashboard
harness-status budget                # today's spend only
harness-status sessions --sessions 10  # recent sessions (limit N)
harness-status progress              # progress file only
harness-status hooks                 # Stop-hook latency aggregation (budget monitor)
harness-status inject                # UserPromptSubmit injection-size aggregation (budget monitor)
harness-status plugins               # classify every plugin by activation scope
harness-status hooks-health          # settings.json hooks whose binary is missing
harness-status path-shadow           # stray PATH binaries shadowing plugin-cache ones
harness-status --json                # machine-readable output (with the full dashboard, budget, sessions, progress, hooks, inject, plugins, hooks-health, path-shadow)
```

The `plugins` subcommand groups every plugin as **always-on** / **event-scoped** / **manual**.
Run it from a checkout; it classifies from the repo layout.

- Do not read a zero, an empty panel, or a silent hook health check as proof that everything is fine.
- When a plugin's store is absent, its section says "not installed?" and the command still exits 0.
- `sessions` and `path-shadow` exit 1 when they cannot read their source: an unreadable gauge store makes `sessions` print `sessions: unknown — …` on stderr (with `--json`, `{"status":"unknown",…}` on stdout), and an incomplete PATH scan makes `path-shadow` print `harness-status: PATH-shadow scan could not be completed: …` on stderr; the full dashboard shows both cases as `unknown — …`.
- `harness-status hooks` flags any session whose combined Stop-hook wall-time exceeds `HARNESS_HOOK_LATENCY_BUDGET_MS` (default `30000`; unparsable values fall back to the default).
- `harness-status inject` flags any turn whose combined UserPromptSubmit injection exceeds `HARNESS_INJECT_BUDGET_CHARS` (default `20000`).
- The date comes from the system clock.
- Override the date with `HARNESS_DATE=YYYY-MM-DD` for testing.

License: MIT
