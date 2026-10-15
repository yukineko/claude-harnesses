On `SessionStart`, difflog snapshots the current HEAD SHA into
`~/.difflog/logs/sessions/<session_id>.json` [test: crates/difflog/tests/prose_claims.rs::claim_session_start_snapshots_head].
On `SessionEnd` it runs `git diff --stat -- <start>..HEAD`; git reads the range
after `--` as a pathspec, the stat comes back empty, and no log file is written [test: crates/difflog/tests/prose_claims.rs::claim_session_end_writes_log].

The plugin registers two hooks: SessionStart and SessionEnd [test: crates/difflog/tests/prose_claims.rs::claim_two_hooks_registered].

No file is written at `~/.difflog/logs/<YYYY-MM-DD>-<session8>.md` [test: crates/difflog/tests/prose_claims.rs::claim_log_path_format].

## Install (plugin)

```
/plugin install difflog@yukineko
```

```sh
cargo install --path .
difflog install
```

```sh
difflog session-start   # SessionStart hook (reads stdin JSON) [test: crates/difflog/tests/prose_claims.rs::claim_session_start_snapshots_head]
difflog session-end     # SessionEnd hook (writes no log: the range is passed as a pathspec) [test: crates/difflog/tests/prose_claims.rs::claim_session_end_writes_log]
difflog last            # print the most recent log [test: crates/difflog/tests/prose_claims.rs::claim_last_prints_newest]
difflog list            # list log files, newest first (--limit N, default 10) [test: crates/difflog/tests/prose_claims.rs::claim_list_newest_first_limit]
difflog init            # write a starter ./difflog.toml (--force to overwrite) [test: crates/difflog/tests/prose_claims.rs::claim_init_writes_starter]
difflog install         # merge hooks into ~/.claude/settings.json (--dry-run) [test: crates/difflog/tests/prose_claims.rs::claim_install_merges_hooks]
difflog uninstall       # remove them (--dry-run) [test: crates/difflog/tests/prose_claims.rs::claim_uninstall_removes_hooks]
difflog status          # show resolved config [test: crates/difflog/tests/integration.rs::status_prints_resolved_config] [test: crates/difflog/tests/prose_claims.rs::claim_config_project_wins] [test: crates/difflog/tests/prose_claims.rs::claim_config_defaults]
```

`./difflog.toml` (project), else `~/.difflog/config.toml` — the first that exists
wins; the files are not layered [test: crates/difflog/tests/prose_claims.rs::claim_config_project_wins].
`log_dir` also holds the SessionStart state `sessions/<session_id>.json` [test: crates/difflog/tests/prose_claims.rs::claim_log_dir_holds_session_state].
Keys (see [`difflog.example.toml`](difflog.example.toml)): `enabled` (default
`true`), `log_dir` (default `~/.difflog/logs`), `diff_body_limit` (default 4096
bytes), `exclude_globs` (built-in default `Cargo.lock`, `package-lock.json`, `*.min.js`,
`*.min.css`) [test: crates/difflog/tests/prose_claims.rs::claim_config_defaults].
`diff_body_limit` is printed by `difflog status` but has no effect on output, because no log is written [test: crates/difflog/tests/prose_claims.rs::claim_diff_body_limit_no_log_effect].
`exclude_globs` is parsed and printed by `difflog status` but never read by the
diff code [test: crates/difflog/tests/prose_claims.rs::claim_exclude_globs_effect].
`DIFFLOG_DISABLE=1` disables the SessionStart snapshot [test: crates/difflog/tests/prose_claims.rs::claim_disable_env].

Do not rewrite log files when generating the narrative; only generate it.

After the session, run the `difflog` skill of the `difflog` plugin (`/difflog:difflog`) [test: crates/difflog/tests/prose_claims.rs::claim_skill_slash_command] [test: crates/difflog/tests/prose_claims.rs::claim_skill_invocation_namespace]:

```
/difflog:difflog
```
