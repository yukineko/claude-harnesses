# ship

**Shipping ritual for Claude Code**, written in Rust.

On `SessionEnd`, it detects unshipped work (uncommitted git changes, stale plugin-cache) and prints a reminder.
Guides you through the shipping workflow: check, rebuild plugins (safe), commit, merge, push.

**GATED invariant**: commit, merge, and push require explicit user approval. `scripts/rebuild-plugins.sh`
(via `ship check --run-safe`) and `scripts/rollout-plugins.sh` (via `ship rollout`) are the only auto-runnable
steps — both mutate `~/.claude/plugins/` state only, never git. The shipping ritual is user-driven; the agent
provides the checklist and prompts for approval before each gate.

Subscription-native: one Rust binary, one hook (SessionEnd), no API key.

## Commands

```sh
ship check             # print unshipped state (dirty git, stale plugin-cache)
ship check --run-safe  # run scripts/rebuild-plugins.sh --stage-repo (SAFE: refresh cache AND the committed crates/*/bin; never git)
ship rollout            # run scripts/rollout-plugins.sh (heavier: automates the /plugin update step)
ship session-end       # SessionEnd hook (reads hook JSON from stdin, prints reminder if unshipped)
```

## Workflow

1. **Diagnostic**: `ship check` to see what is unshipped.
2. **Auto-rebuild**: `ship check --run-safe` to rebuild the plugin cache from source.
3. **Rollout** (as needed): `ship rollout` to fully clear stale plugin binaries by automating
   `/plugin update` (advances the installed version pointer + creates the cache version dir), then
   rebuild+sync. A separate, explicit operation from `--run-safe`.
4. **Commit** (user approval required): `git add && git commit -m "..."`
5. **Merge** (user approval required): `git merge <branch>`
6. **Push** (user approval required): `git push origin <branch>`

## SessionEnd hook

On SessionEnd, ship runs automatically and reminds you if there is unshipped work. The reminder is informational 
and never blocks anything.

## /ship skill

After the session, use `/ship` to walk through the shipping ritual step by step. The skill ensures you have 
visibility into what will be committed, merged, or pushed before any action is taken.

## GATED invariant — critical

- **commit, merge, push**: NEVER auto-run. ALWAYS get explicit user approval first. Show diffs, ask "approve?", wait for "yes".
- **rebuild-plugins.sh --stage-repo**: auto-runnable via `ship check --run-safe` (refreshes the cache AND the committed `crates/*/bin` binary — the artifact stale-detection measures; a file copy, never git). This is what clears a "stale plugin binaries" item; the refreshed binary then needs a GATED commit.
- **rollout-plugins.sh**: auto-runnable via `ship rollout` (heavier, explicit — automates `/plugin update`).

## Install (plugin)

```
/plugin install ship@yukineko
```

## Manual install

```sh
cargo install --path .
ship session-end  # test it
```

## License

MIT
