---
description: harness 全プラグインの HOTL ステータスを集約表示する。今日のコスト (budgetguard)、直近セッション (gauge)、進捗ファイル (.claude/progress.md) を一画面にまとめる。
argument-hint: [budget|sessions|progress] [--json]
---

You are running the **harness-status `/status`** command: surface a unified
human-on-the-loop view across the harness plugins. The bundled binary does all
the aggregation; YOUR job is to run it and, if useful, add a one-line read of
what the numbers mean.

## Steps

1. **Run the dashboard.** Run:
   ```
   ${CLAUDE_PLUGIN_ROOT}/bin/harness-status
   ```
   (If `CLAUDE_PLUGIN_ROOT` is unset, fall back to `bin/harness-status` from the
   plugin dir.) It prints today's spend, recent sessions with cost, and the
   project progress file in one view.

2. **Scope it if the user asked.** With an argument, run only that section:
   - `/status budget`   → `harness-status budget`
   - `/status sessions` → `harness-status sessions`
   - `/status progress` → `harness-status progress`
   - Append `--json` for machine-readable output (e.g. `/status --json`).

3. **Interpret briefly (optional).** After the table, add at most 1–2 lines:
   - If today's spend is approaching a known budget limit, say so.
   - If a recent session has unusually high cost/turns, flag it.

## Notes

- Read-only. The binary never writes; it only reads other plugins' state stores
  (`~/.budgetguard`, `~/.gauge`, `<cwd>/.claude/progress.md`).
- If a section reports "not installed", that plugin isn't set up — mention it
  but don't treat it as an error.
