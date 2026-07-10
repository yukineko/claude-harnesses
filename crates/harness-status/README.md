# harness-status

**Unified HOTL status dashboard for Claude Code**, written in Rust.

The harness plugins each keep their own state. harness-status reads them all and
prints one human-on-the-loop view — what you'd glance at to decide whether to
step in:

- **Budget** (budgetguard): today's spend and session count, from
  `~/.budgetguard/state/ledger.json`.
- **Recent sessions** (gauge): the last N sessions with turns, tokens, and USD
  cost, from `~/.gauge/store/sessions/`.
- **Progress** (taskprog): the current `.claude/progress.md` preview.

It is **read-only** — it never writes, only aggregates other plugins' stores. No
API key: a single binary you (or a `/status` command) run on demand.

**Activation scope: manual (CLI-only) dashboard, plus one narrow observability
hook.** harness-status is the unified manual human-on-the-loop inspection
dashboard, and does **not** register a general-purpose `SessionStart` dashboard
hook — auto-injecting the full dashboard every session would grow the always-on
injection/hook budget that this very tool (`hooks` / `inject`) and
[ADR 0001](../ctxrot/docs/adr/0001-cross-harness-injection-budget.md) exist to
curb. It registers exactly one lightweight exception: a `SessionStart` hook
(`hooks/hooks.json`) that runs the hooks-binary health check
(`harness-status session-start`) and injects `additionalContext` **only** when a
registered hook's binary is missing from disk — a silently-broken hook is the
kind of thing a human can't notice without checking, so it's surfaced
unconditionally rather than left to a manual `/status` run. When healthy it is
silent (no output, no injected context) — fail-soft, never blocks the turn. See
[`docs/plugin-activation-scopes.md`](../../docs/plugin-activation-scopes.md) for
the full three-scope taxonomy and the current classification of every plugin.

## Output

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

── Progress file (taskprog) ──────────────────────
  cwd: /repo
  /repo/.claude/progress.md
  │ # Progress
  │ ## Pending
  │ - specforge ⑤ worktree merge
```

## Install (plugin)

```
/plugin install harness-status@yukineko
```

Then run `/status` in any session.

## Manual install

```sh
cargo install --path .
harness-status            # full dashboard
```

## Commands

```sh
harness-status                       # full dashboard
harness-status budget                # today's spend only
harness-status sessions --sessions 10  # recent sessions (limit N)
harness-status progress              # progress file only
harness-status hooks                 # Stop-hook latency aggregation (budget monitor)
harness-status inject                # UserPromptSubmit injection-size aggregation (budget monitor)
harness-status plugins               # classify every plugin by activation scope
harness-status --json                # machine-readable output (any subcommand)
```

The `plugins` subcommand scans the monorepo and groups every plugin as
**always-on** / **event-scoped** / **manual** (see
[`docs/plugin-activation-scopes.md`](../../docs/plugin-activation-scopes.md)). It
is a dev/HOTL tool: it classifies from the repo layout, so run it from a checkout.

## Notes

- A section that reports "not installed" just means that plugin's store is
  absent — not an error.
- `harness-status hooks` flags any Stop-hook whose latency exceeds
  `HARNESS_HOOK_LATENCY_BUDGET_MS` (default `30000`).
- `harness-status inject` flags any UserPromptSubmit injection larger than
  `HARNESS_INJECT_BUDGET_CHARS` (default `20000`).
- The date is derived without a clock dependency; override with `HARNESS_DATE=YYYY-MM-DD`
  for testing.

## License

MIT
