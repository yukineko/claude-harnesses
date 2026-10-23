On `SessionStart`, difflog snapshots the current HEAD SHA into
`~/.difflog/logs/sessions/<session_id>.json`.
On `SessionEnd` it runs `git diff --stat -- <start>..HEAD`; git reads the range
after `--` as a pathspec, the stat comes back empty, and no log file is written.

The plugin registers two hooks: SessionStart and SessionEnd.

No file is written at `~/.difflog/logs/<YYYY-MM-DD>-<session8>.md`.

## Install (plugin)

```
/plugin install difflog@yukineko
```

```sh
cargo install --path .
difflog install
```

```sh
difflog session-start   # SessionStart hook (reads stdin JSON)
difflog session-end     # SessionEnd hook (writes no log: the range is passed as a pathspec)
difflog last            # print the most recent log
difflog list            # list log files, newest first (--limit N, default 10)
difflog init            # write a starter ./difflog.toml (--force to overwrite)
difflog install         # merge hooks into ~/.claude/settings.json (--dry-run)
difflog uninstall       # remove them (--dry-run)
difflog status          # show resolved config
```

`./difflog.toml` (project), else `~/.difflog/config.toml` — the first that exists
wins; the files are not layered.
`log_dir` also holds the SessionStart state `sessions/<session_id>.json`.
Keys (see [`difflog.example.toml`](difflog.example.toml)): `enabled` (default
`true`), `log_dir` (default `~/.difflog/logs`), `diff_body_limit` (default 4096
bytes), `exclude_globs` (built-in default `Cargo.lock`, `package-lock.json`, `*.min.js`,
`*.min.css`).
`diff_body_limit` is printed by `difflog status` but has no effect on output, because no log is written.
`exclude_globs` is parsed and printed by `difflog status` but never read by the
diff code.
`DIFFLOG_DISABLE=1` disables the SessionStart snapshot.

Do not rewrite log files when generating the narrative; only generate it.

After the session, run the `difflog` skill of the `difflog` plugin (`/difflog:difflog`):

```
/difflog:difflog
```
