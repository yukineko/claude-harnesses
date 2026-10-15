It is **read-only** — it writes nothing under HOME, the working directory or TMPDIR, and only aggregates other plugins' stores [test: crates/harness-status/tests/prose_claims.rs::claim_read_only].

It does **not** register a general-purpose `SessionStart` dashboard hook [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_json_single_session_start].

**Unified HOTL status dashboard for Claude Code** [test: crates/harness-status/tests/prose_claims.rs::claim_full_dashboard].

- **Budget** (budgetguard): today's spend and session count, from `~/.budgetguard/state/ledger.json` [test: crates/harness-status/tests/prose_claims.rs::claim_budget_from_ledger].
- **Recent sessions** (gauge): the last N sessions with turns, tokens, and USD cost, from `~/.gauge/store/sessions/` [test: crates/harness-status/tests/prose_claims.rs::claim_sessions_from_gauge_limit].
- **Progress** (taskprog): the current `.claude/progress.md` preview [test: crates/harness-status/tests/prose_claims.rs::claim_progress_preview].
- **Stop-hook latency**: per-session totals from `~/.harness/state/hook-latency.jsonl` [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_latency_per_session].
- **UserPromptSubmit injection size**: per-turn totals from `~/.harness/state/inject-metrics.jsonl` [test: crates/harness-status/tests/prose_claims.rs::claim_inject_per_turn].
- **Hook binary health**: hooks registered in `~/.claude/settings.json` whose binary is missing on disk [test: crates/harness-status/tests/integration.rs::session_start_warns_via_additional_context_when_binary_missing] [test: crates/harness-status/src/hooks_health.rs::read_at_reports_missing_binaries_and_tolerates_absent_file].
- **PATH shadowing**: a stray binary on `PATH` that shadows a plugin-cache binary of the same name [test: crates/harness-status/src/path_shadow.rs::earlier_dir_with_same_name_is_flagged_as_shadowing] [test: crates/harness-status/tests/prose_claims.rs::claim_path_shadow_cli].

It registers exactly one hook: a `SessionStart` hook (`hooks/hooks.json`) that runs the hooks-binary health check (`harness-status session-start`) [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_json_single_session_start].
It injects `additionalContext` when a hook registered in `~/.claude/settings.json` points at a binary missing from disk [test: crates/harness-status/tests/integration.rs::session_start_warns_via_additional_context_when_binary_missing].
It injects `additionalContext` when a plugin-cache binary is shadowed by a stray `PATH` copy [test: crates/harness-status/tests/prose_claims.rs::claim_session_start_shadow_injects].
It injects `additionalContext` when the PATH-shadow scan itself could not complete (reported as undetermined) [test: crates/harness-status/tests/prose_claims.rs::claim_session_start_undetermined_injects].
Otherwise it is silent (no output, no injected context) [test: crates/harness-status/tests/integration.rs::session_start_is_silent_when_all_hook_binaries_present].

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
  │ - specforge ⑤ worktree merge
```

```
/plugin install harness-status@yukineko
```

Then run `/status` in any session [test: crates/harness-status/tests/prose_claims.rs::claim_status_command].

```sh
cargo install --path .
harness-status
```

```sh
harness-status                       # full dashboard [test: crates/harness-status/tests/prose_claims.rs::claim_full_dashboard]
harness-status budget                # today's spend only [test: crates/harness-status/tests/prose_claims.rs::claim_budget_from_ledger]
harness-status sessions --sessions 10  # recent sessions (limit N) [test: crates/harness-status/tests/prose_claims.rs::claim_sessions_from_gauge_limit]
harness-status progress              # progress file only [test: crates/harness-status/tests/prose_claims.rs::claim_progress_preview]
harness-status hooks                 # Stop-hook latency aggregation (budget monitor) [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_latency_per_session]
harness-status inject                # UserPromptSubmit injection-size aggregation (budget monitor) [test: crates/harness-status/tests/prose_claims.rs::claim_inject_per_turn]
harness-status plugins               # classify every plugin by activation scope [test: crates/harness-status/tests/prose_claims.rs::claim_plugins_cli]
harness-status hooks-health          # settings.json hooks whose binary is missing [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_health_cli]
harness-status path-shadow           # stray PATH binaries shadowing plugin-cache ones [test: crates/harness-status/src/path_shadow.rs::earlier_dir_with_same_name_is_flagged_as_shadowing] [test: crates/harness-status/tests/prose_claims.rs::claim_path_shadow_cli]
harness-status --json                # machine-readable output (with the full dashboard, budget, sessions, progress, hooks, inject, plugins, hooks-health, path-shadow) [test: crates/harness-status/tests/prose_claims.rs::claim_json_any_subcommand]
```

The `plugins` subcommand groups every plugin as **always-on** / **event-scoped** / **manual** [test: crates/harness-status/src/plugins.rs::classify_always_on_on_high_frequency_event] [test: crates/harness-status/src/plugins.rs::classify_event_scoped_on_only_low_frequency_events] [test: crates/harness-status/src/plugins.rs::classify_manual_when_no_hooks] [test: crates/harness-status/tests/prose_claims.rs::claim_plugins_cli].
Run it from a checkout; it classifies from the repo layout [test: crates/harness-status/src/plugins.rs::find_repo_root_walks_up_to_marketplace] [test: crates/harness-status/tests/prose_claims.rs::claim_plugins_cli].

- Do not read a zero, an empty panel, or a silent hook health check as proof that everything is fine.
- When a plugin's store is absent, its section says "not installed?" and the command still exits 0 [test: crates/harness-status/tests/prose_claims.rs::claim_not_installed].
- `sessions` and `path-shadow` exit 1 when they cannot read their source: an unreadable gauge store makes `sessions` print `sessions: unknown — …` on stderr (with `--json`, `{"status":"unknown",…}` on stdout), and an incomplete PATH scan makes `path-shadow` print `harness-status: PATH-shadow scan could not be completed: …` on stderr; the full dashboard shows both cases as `unknown — …` [test: crates/harness-status/tests/prose_claims.rs::claim_unknown_exit1].
- `harness-status hooks` flags any session whose combined Stop-hook wall-time exceeds `HARNESS_HOOK_LATENCY_BUDGET_MS` (default `30000`; unparsable values fall back to the default) [test: crates/harness-status/tests/prose_claims.rs::claim_hooks_budget_default_and_env].
- `harness-status inject` flags any turn whose combined UserPromptSubmit injection exceeds `HARNESS_INJECT_BUDGET_CHARS` (default `20000`) [test: crates/harness-status/tests/prose_claims.rs::claim_inject_budget_default_and_env].
- The date comes from the system clock [test: crates/harness-status/tests/prose_claims.rs::claim_date_system_clock].
- Override the date with `HARNESS_DATE=YYYY-MM-DD` for testing [test: crates/harness-status/tests/integration.rs::budget_subcommand_reports_pinned_date].

License: MIT [test: crates/harness-status/tests/prose_claims.rs::claim_license_mit]
