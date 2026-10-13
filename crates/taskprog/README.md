# taskprog

**Multi-session progress file for Claude Code**, written in Rust.

A long task rarely fits one session. taskprog keeps a single `.claude/progress.md`
current across sessions so the agent always resumes with full context:

- On **SessionStart** it injects the progress file as `additionalContext`, so a
  fresh session immediately knows what's done, what's pending, and what's blocked.
- On **Stop** it prompts the agent to update the file with what just happened.

This closes the HOTL handoff loop: the human sits at the boundaries (review the
progress file, redirect), and each session picks up exactly where the last left
off — no manual re-briefing.

Subscription-native: one Rust binary, two hooks (SessionStart + Stop), no API key.

## What it manages

`.claude/progress.md` at the **project root** — the nearest ancestor of the session
cwd that contains a `.git` entry (a git worktree root counts, so each worktree keeps
its own file). If no such root can be found, taskprog writes nothing and says so
rather than falling back to the cwd. Committed with the code if you like:

```markdown
# Progress

Updated: 2026-06-23

## Completed
- budgetguard cost gate wired into Stop

## Pending
- specforge ⑤ parallel-impl worktree merge

## Blockers
- (none)

## Notes
- harness-status reads gauge from ~/.gauge/store, not /state
```

## Install (plugin)

```
/plugin install taskprog@yukineko
```

## Manual install

```sh
cargo install --path .
taskprog install
```

## Commands

```sh
taskprog session-start   # SessionStart hook: inject progress file (reads stdin JSON)
taskprog stop            # Stop hook: prompt the agent to update the file
taskprog show            # print the current progress file
taskprog write --cwd .   # write progress.md from stdin (--cwd defaults to .)
taskprog init [TARGET]   # write a starter config (default TARGET: taskprog.toml)
taskprog install         # merge hooks into ~/.claude/settings.json (--dry-run)
taskprog uninstall       # remove them (--dry-run)
taskprog status          # show resolved config
```

## Update with `/taskprog`

Run `/taskprog` any time to have the agent refresh the progress file with the
current Completed / Pending / Blockers state (those three sections are required;
empty ones are written as `(none)`). Pass `--reset` to blank the file before
rewriting it.

## Config (`taskprog.toml`)

Read from `taskprog.toml` in the session cwd, else `~/.taskprog/config.toml` (see
[`taskprog.example.toml`](taskprog.example.toml)):

```toml
enabled = true
# progress_file = "~/.claude/progress.md"   # default: <project root>/.claude/progress.md
inject_limit = 4096                          # bytes injected at SessionStart (0 = all)
```

An explicit `progress_file` is used as-is (`~` expanded) with no project-root lookup.
Disable per-session with `TASKPROG_DISABLED=1` (or `true`).

## License

MIT
