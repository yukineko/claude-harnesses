- On **SessionStart** it injects the progress file as a top-level `additionalContext` [test: crates/taskprog/tests/prose_claims.rs::claim_session_start_injects].
- On **Stop** it prompts the agent to update the file [test: crates/taskprog/tests/prose_claims.rs::claim_stop_prompts_update].

taskprog registers two hooks, SessionStart and Stop, which call `taskprog session-start` and `taskprog stop` [test: crates/taskprog/tests/prose_claims.rs::claim_hooks_json_two_hooks].

`.claude/progress.md` at the **project root** — an ancestor of the session
cwd that contains a `.git` entry (a `.git` file, as in a git worktree root, counts) [test: crates/taskprog/src/config.rs::a_subdirectory_cwd_anchors_at_the_project_root] [test: crates/taskprog/src/config.rs::a_worktree_root_is_anchored_even_though_dot_git_is_a_file]. If no such root can be found, the Stop hook writes nothing and says so
rather than falling back to the cwd [test: crates/taskprog/src/update.rs::on_stop_writes_nothing_when_the_root_is_undeterminable] [test: crates/taskprog/tests/prose_claims.rs::claim_no_root_says_so]. The Stop-seeded skeleton has a `## Done` section [test: crates/taskprog/src/update.rs::on_stop_writes_skeleton_when_missing].

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
taskprog session-start   # SessionStart hook: inject the progress file [test: crates/taskprog/tests/prose_claims.rs::claim_session_start_injects]
taskprog stop            # Stop hook: prompt the agent to update the file [test: crates/taskprog/tests/prose_claims.rs::claim_stop_prompts_update]
taskprog show            # print the current progress file [test: crates/taskprog/tests/prose_claims.rs::claim_show_prints_file]
taskprog write --cwd .   # write progress.md from stdin (--cwd defaults to .) [test: crates/taskprog/tests/prose_claims.rs::claim_write_from_stdin_default_cwd]
taskprog init [TARGET]   # write a starter config (default TARGET: taskprog.toml) [test: crates/taskprog/tests/prose_claims.rs::claim_init_default_target]
taskprog install         # merge hooks into ~/.claude/settings.json (--dry-run) [test: crates/taskprog/tests/prose_claims.rs::claim_install_merges_hooks]
taskprog uninstall       # remove them (--dry-run) [test: crates/taskprog/tests/prose_claims.rs::claim_uninstall_removes_hooks]
taskprog status          # show resolved config [test: crates/taskprog/tests/prose_claims.rs::claim_config_lookup_order]
```

The `/taskprog` skill takes a `--reset` argument [test: crates/taskprog/tests/prose_claims.rs::claim_skill_reset_arg].

Config is read from `taskprog.toml` in the session cwd, else `~/.taskprog/config.toml` [test: crates/taskprog/tests/prose_claims.rs::claim_config_lookup_order]:

```toml
enabled = true
# progress_file = "~/.claude/progress.md"   # default: <project root>/.claude/progress.md [test: crates/taskprog/src/config.rs::a_subdirectory_cwd_anchors_at_the_project_root]
inject_limit = 4096                          # caps the bytes of the file injected at SessionStart (0 = all) [test: crates/taskprog/tests/prose_claims.rs::claim_inject_limit_bytes]
```

An explicit `progress_file` is used as-is with no project-root lookup [test: crates/taskprog/src/config.rs::an_explicit_progress_file_is_unchanged_and_needs_no_project_root].
A leading `~` in `progress_file` is expanded [test: crates/taskprog/tests/prose_claims.rs::claim_tilde_expanded].
`TASKPROG_DISABLED=1` (or `true`) disables both hooks for the session [test: crates/taskprog/tests/prose_claims.rs::claim_disabled_env].

License: MIT [test: crates/taskprog/tests/prose_claims.rs::claim_license_mit].
