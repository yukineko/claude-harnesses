Never auto-run commit, merge, or push; get explicit user approval first.

`ship check`, `ship check --run-safe` and `ship rollout` run only read-only git subcommands.
Neither `scripts/rebuild-plugins.sh` nor `scripts/rollout-plugins.sh` contains a state-mutating git command.

```sh
ship check             # print a checklist: uncommitted changes, unmerged condukt/* branches, leftover worktrees, unpushed commits, stale plugin binaries
ship check --run-safe  # also run scripts/rebuild-plugins.sh --stage-repo from the repo root, then reprint the GATED items
ship rollout           # run scripts/rollout-plugins.sh from the repo root (no arguments)
ship session-end       # the SessionEnd hook command
```

`ship check` changes neither the working tree, the staged content, nor `HEAD`.
`ship check --run-safe` does not itself run `scripts/rollout-plugins.sh`, and `ship rollout` does not itself run `scripts/rebuild-plugins.sh`.
`scripts/rollout-plugins.sh` runs `scripts/rebuild-plugins.sh` without `--stage-repo`, and `scripts/rebuild-plugins.sh` copies into `crates/*/bin` only under `--stage-repo`.

The plugin registers one hook: `SessionEnd` runs `${CLAUDE_PLUGIN_ROOT}/bin/ship session-end`.
After the session, use `/ship` to walk through the shipping ritual step by step.

Stale detection compares the newest mtime under each crate's `src/` against a local
`crates/<name>/bin/<name>-linux-x86_64` file.
Per-platform binaries (`crates/*/bin/*-linux-x86_64`) are git-ignored in this repo.

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

License: MIT
