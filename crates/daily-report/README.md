# daily-report — synthesize a daily work report from git + Obsidian

`/daily-report` aggregates the day's **git commits** and the **Obsidian session records**
(written by `session-insights` under `<vault>/records/`) into a single scannable **daily report
(Markdown)**, then writes it back to `<vault>/daily/<date>.md`.

```
materials (deterministic)                 synthesis (LLM)          output
  git log (what changed in this repo)  ─┐
  Obsidian records (cross-project      ├─▶  daily narrative  ─▶  <vault>/daily/<date>.md
    summaries, learnings, cost/tokens) ─┘                        (or --stdout)
```

- **git** — first-hand "what changed and why" (commits, change size) for this repo.
- **Obsidian records** — cross-project completion summaries, learnings, remaining tasks, plus the
  machine-filled turns/tokens/cost numbers.
- The command holds **no state of its own** — it reads the materials and synthesizes; it never
  rewrites the record notes.

## Usage

```
/daily-report                        # today → <vault>/daily/<today>.md
/daily-report yesterday              # previous day
/daily-report 2026-07-01             # a specific date
/daily-report --since "3 days ago"   # range mode
/daily-report --repo ../other-repo   # also scan extra repos (repeatable)
/daily-report --stdout               # print only, don't write to the vault
```

## Vault resolution

1. If `~/.session-insights/config.toml` exists, use its `obsidian_vault` / `record_dir` (with `~` expansion).
2. Otherwise fall back to `~/Documents/vault/yukineko` + `records` (same defaults as session-insights).

## Guarantees (fail-soft)

- Missing config, non-git dirs, or zero records never stop it — it reports from whatever it can gather.
- Numbers (turns/tokens/cost, change size) come only from real record/git values; nothing is fabricated.
- Output goes only to `<vault>/daily/<date>.md`; it never writes into `records/`.

## Related

- `session-insights` (`/record`) — writes the per-session records that this report consumes.
- `difflog` — single-session git diff narrative (this command is the day-level cross-session aggregator).

Subscription-native (skill only, no binary, no API key).
