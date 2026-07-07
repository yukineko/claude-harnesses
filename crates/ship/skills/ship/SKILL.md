---
name: ship
description: Ship the current work through the commit・merge・push ritual. Detects unshipped git and plugin-cache state; gated on explicit user approval.
allowed-tools: Bash(ship:*)
---

# /ship — shipping ritual

The shipping ritual has stages: `ship check` (diagnostic), `ship check --run-safe` (auto-rebuild),
`ship rollout` (auto-rollout, as needed), commit, merge, push.
**Only the rebuild and rollout steps are automatic. Commit, merge, and push REQUIRE explicit user approval — never auto-run them.**

## Stages

### 1. Diagnostic: `ship check`

Run this to see the unshipped state (dirty git, uncommitted plugin-cache changes, stale plugins):

```sh
ship check
```

This prints a checklist of what must be done before the repo is clean. It does NOT modify anything.

### 2. Auto-rebuild (safe): `ship check --run-safe`

An automatic operation:

```sh
ship check --run-safe
```

This runs `scripts/rebuild-plugins.sh --stage-repo` to rebuild plugins from source. `--stage-repo` refreshes
BOTH the live plugin cache (existing cache version dirs) AND the committed `crates/<name>/bin/` binary — the
latter is exactly what the "stale plugin binaries" check measures (committed bin older than its `src/`), and
what the directory marketplace ships to `/plugin install`. So `--run-safe` DOES clear the "stale plugin
binaries" item: on re-detect it's gone, and the only residue is that the refreshed binary now shows up as an
uncommitted change (its commit is GATED — step 3). The refresh is a plain file copy, never a git command, so
ship's git invariant is preserved (it does dirty the working tree with the rebuilt binary).

### 2b. Auto-rollout (heavier, explicit): `ship rollout`

```sh
ship rollout
```

This runs `scripts/rollout-plugins.sh`, which fully automates the `/plugin update` step for the local-directory
`yukineko` marketplace: it creates the cache `<name>/<version>/` dir, repoints
`~/.claude/plugins/installed_plugins.json` to it, then runs rebuild-plugins.sh + each plugin's
`scripts/sync-plugin-assets.sh`. It is a **separate, explicit** step from `--run-safe` (not folded into it)
because it is heavier and more consequential — it advances what version of a plugin is actually installed and
running, not just the binary bits inside an existing cache dir. It is still not git-mutating: it only writes
into `~/.claude/plugins/`. NOTE: rollout runs rebuild-plugins.sh WITHOUT `--stage-repo`, so it is cache-only
w.r.t. the repo and does NOT touch the committed `crates/*/bin` binary — it therefore does NOT clear a "stale
plugin binaries" item (that is committed-bin staleness, cleared by `--run-safe` in step 2 + a GATED commit).
Use rollout when you need to advance the installed version pointer (the `/plugin update` step), not to fix
stale committed binaries.

### 3. Commit (GATED — requires user approval)

**Do NOT auto-run.** Get explicit user approval, then:

```sh
git add <files> && git commit -m "<message>"
```

Example:
```sh
git add -A && git commit -m "feat: ...commit message..."
```

### 4. Merge (GATED — requires user approval)

**Do NOT auto-run.** Get explicit user approval, then:

```sh
git merge <branch>
```

or (on a feature branch):
```sh
git checkout main && git merge <feature-branch>
```

### 5. Push (GATED — requires user approval)

**Do NOT auto-run.** Get explicit user approval, then:

```sh
git push [origin <branch>]
```

## GATED invariant

commit, merge, and push are NEVER auto-run. Before executing any of these, you MUST:

1. Show the user what will happen (e.g., `git log --oneline`, `git diff`, `git status`).
2. Get explicit approval ("yes", "confirm", "go ahead", etc.). If the user says "no" or does not confirm, stop and do not proceed.
3. Only after confirmation, run the command.

This gate is non-negotiable. The shipping ritual is user-driven. You provide the checklist and prepare the commands, but the user decides when to ship.

## SessionEnd hook

On every SessionEnd, the ship hook runs `ship session-end` to remind you if there is unshipped work. The reminder is informational only and does not block anything.

## Example flow

```
/ship

→ outputs unshipped state (dirty git, stale plugin-cache)

User: please rebuild
/ship check --run-safe

→ runs scripts/rebuild-plugins.sh --stage-repo (refreshes cache AND committed crates/*/bin)
→ "stale plugin binaries" now clears; the refreshed binary shows as an uncommitted change (commit is GATED)

User: I also need the installed version pointer advanced (/plugin update)
/ship rollout

→ runs scripts/rollout-plugins.sh (advances installed version pointer, then rebuild+sync;
   cache-only w.r.t. the repo — does NOT itself clear committed-bin staleness)

User: commit and push
→ Agent: "I can commit with the following. Approve?"
→ Show: git status, git diff, proposed commit message
User: "Yes, commit."
→ Agent: git add -A && git commit -m "..."
→ git push origin main  (with user approval)
```

## Note

The agent's role is to guide and prepare. The user controls the shipping ritual. If you are unsure whether the user has approved, ask again.
