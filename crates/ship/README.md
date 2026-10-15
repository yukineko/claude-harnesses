Never auto-run commit, merge, or push; get explicit user approval first.

`ship check`, `ship check --run-safe` and `ship rollout` run only read-only git subcommands [test: crates/ship/tests/prose_claims.rs::claim_binary_runs_only_readonly_git] [test: crates/ship/tests/integration.rs::binary_never_mutates_git].
Neither `scripts/rebuild-plugins.sh` nor `scripts/rollout-plugins.sh` contains a state-mutating git command [test: crates/ship/tests/prose_claims.rs::claim_scripts_no_mutating_git].

```sh
ship check             # print a checklist: uncommitted changes, unmerged condukt/* branches, leftover worktrees, unpushed commits, stale plugin binaries [test: crates/ship/tests/prose_claims.rs::claim_check_lists_categories]
ship check --run-safe  # also run scripts/rebuild-plugins.sh --stage-repo from the repo root, then reprint the GATED items [test: crates/ship/tests/prose_claims.rs::claim_run_safe_runs_rebuild_stage_repo]
ship rollout           # run scripts/rollout-plugins.sh from the repo root (no arguments) [test: crates/ship/tests/prose_claims.rs::claim_rollout_runs_script_no_args]
ship session-end       # the SessionEnd hook command [test: crates/ship/tests/prose_claims.rs::claim_hooks_json_sessionend]
```

`ship check` changes neither the working tree, the staged content, nor `HEAD` [test: crates/ship/tests/prose_claims.rs::claim_check_does_not_modify].
`ship check --run-safe` does not itself run `scripts/rollout-plugins.sh`, and `ship rollout` does not itself run `scripts/rebuild-plugins.sh` [test: crates/ship/tests/prose_claims.rs::claim_run_safe_and_rollout_separate].
`scripts/rollout-plugins.sh` runs `scripts/rebuild-plugins.sh` without `--stage-repo`, and `scripts/rebuild-plugins.sh` copies into `crates/*/bin` only under `--stage-repo` [test: crates/ship/tests/prose_claims.rs::claim_rollout_script_no_stage_repo].

The plugin registers one hook: `SessionEnd` runs `${CLAUDE_PLUGIN_ROOT}/bin/ship session-end` [test: crates/ship/tests/prose_claims.rs::claim_hooks_json_sessionend].
After the session, use `/ship` to walk through the shipping ritual step by step [test: crates/ship/tests/prose_claims.rs::claim_skill_ship_exists].

Stale detection compares the newest mtime under each crate's `src/` against a local
`crates/<name>/bin/<name>-linux-x86_64` file [test: crates/ship/src/stale.rs::test_stale_crates_newer_src] [test: crates/ship/src/stale.rs::test_stale_crates_newer_bin] [test: crates/ship/tests/prose_claims.rs::claim_stale_newest_nested_src].
Per-platform binaries (`crates/*/bin/*-linux-x86_64`) are git-ignored in this repo [test: crates/ship/tests/prose_claims.rs::claim_platform_bins_gitignored].

## Never auto-run commit, merge, or push

- **commit, merge, push**: NEVER auto-run. ALWAYS get explicit user approval first. Ask "approve?" with the diffs shown, wait for "yes".

## Install (plugin)

```
/plugin install ship@yukineko
```

## Install from source

```sh
cargo install --path .
```

License: MIT [test: crates/ship/tests/prose_claims.rs::claim_license_mit]
