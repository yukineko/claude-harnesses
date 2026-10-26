Both hooks print **nothing** (stdout and stderr empty, exit 0):

- On **SessionStart** it does nothing; the progress file is not injected into the session.
- On **Stop** it seeds a skeleton if the file is missing or empty, and leaves an existing file untouched. It shows no message to the user or the model.

taskprog registers two hooks, SessionStart and Stop, which call `taskprog session-start` and `taskprog stop`.

`.claude/progress.md` at the **project root** — an ancestor of the session
cwd that contains a `.git` entry (a `.git` file, as in a git worktree root, counts). If no such root can be found, the Stop hook writes nothing (silently)
rather than falling back to the cwd. The Stop-seeded skeleton has a `## Done` section.
Use `taskprog show` or the `/taskprog` skill to read or update the file.

## Install the plugin

```
/plugin install taskprog@yukineko
```

## Install manually

```sh
cargo install --path .
taskprog install
```

```sh
taskprog session-start   # SessionStart hook: prints nothing
taskprog stop            # Stop hook: seed a skeleton if missing; prints nothing
taskprog show            # print the current progress file
taskprog write --cwd .   # write progress.md from stdin (--cwd defaults to .)
taskprog init [TARGET]   # write a starter config (default TARGET: taskprog.toml)
taskprog install         # merge hooks into ~/.claude/settings.json (--dry-run)
taskprog uninstall       # remove them (--dry-run)
taskprog status          # show resolved config
```

The `/taskprog` skill takes a `--reset` argument.

Config is read from `taskprog.toml` in the session cwd, else `~/.taskprog/config.toml`:

```toml
enabled = true
# progress_file = "~/.claude/progress.md"   # default: <project root>/.claude/progress.md
inject_limit = 4096                          # no effect (nothing is injected); kept for compatibility, shown by `status`
```

An explicit `progress_file` is used as-is with no project-root lookup.
A leading `~` in `progress_file` is expanded.
`TASKPROG_DISABLED=1` (or `true`) disables both hooks for the session.

License: MIT.
