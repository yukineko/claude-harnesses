---
name: ship
description: Use to ship the current work through the commit・merge・push ritual; never auto-run commit, merge or push without explicit user approval.
allowed-tools: Bash(ship:*)
---

**Commit, merge, and push REQUIRE explicit user approval — never auto-run them.**

Run the diagnostic, rebuild and rollout steps without asking for approval; only commit, merge and push need it.

### 1. Run the diagnostic

```sh
ship check
```

`ship check` prints a checklist of uncommitted changes, unmerged condukt/* branches, leftover worktrees, unpushed commits and stale plugin binaries [test: crates/ship/tests/prose_claims.rs::claim_check_lists_categories].
`ship check` changes neither the working tree, the staged content, nor `HEAD` [test: crates/ship/tests/prose_claims.rs::claim_check_does_not_modify].

### 2. Run the safe auto-rebuild

```sh
ship check --run-safe
```

`ship check --run-safe` runs `scripts/rebuild-plugins.sh --stage-repo` from the repo root, then reprints the GATED items [test: crates/ship/tests/prose_claims.rs::claim_run_safe_runs_rebuild_stage_repo].

### Run the explicit rollout (step 2b)

```sh
ship rollout
```

`ship rollout` runs `scripts/rollout-plugins.sh` from the repo root with no arguments [test: crates/ship/tests/prose_claims.rs::claim_rollout_runs_script_no_args].
`ship check --run-safe` does not itself run `scripts/rollout-plugins.sh`, and `ship rollout` does not itself run `scripts/rebuild-plugins.sh` [test: crates/ship/tests/prose_claims.rs::claim_run_safe_and_rollout_separate].
`scripts/rollout-plugins.sh` runs `scripts/rebuild-plugins.sh` without `--stage-repo`, and `scripts/rebuild-plugins.sh` copies into `crates/*/bin` only under `--stage-repo` [test: crates/ship/tests/prose_claims.rs::claim_rollout_script_no_stage_repo].
Neither `scripts/rebuild-plugins.sh` nor `scripts/rollout-plugins.sh` contains a state-mutating git command [test: crates/ship/tests/prose_claims.rs::claim_scripts_no_mutating_git].
Use the rollout step to advance the installed plugin version, not to fix stale plugin binaries.

### 3. Commit (GATED — requires user approval)

**Do NOT auto-run.** Get explicit user approval, then:

```sh
git add <files> && git commit -m "<message>"
```

### 4. Never auto-run merge (requires user approval)

**Do NOT auto-run.** Get explicit user approval, then:

```sh
git merge <branch>
```

Use this on a feature branch:
```sh
git checkout main && git merge <feature-branch>
```

### 5. Never auto-run push (requires user approval)

**Do NOT auto-run.** Get explicit user approval, then:

```sh
git push [origin <branch>]
```

## Never auto-run commit, merge, or push

Never auto-run commit, merge, or push. Never skip these steps; always do the following before executing any of these:

1. Make sure the user sees what will happen, for example the git log, the diff, and the git status.
2. Ask for explicit approval ("yes", "confirm", "go ahead", etc.). Do not proceed if the user says "no" or does not confirm.
3. Run the command only after confirmation.

Do not decide when to ship — the user decides. Keep to providing the checklist and preparing the commands.

```
/ship

→ outputs the ship check checklist

User: please rebuild
/ship check --run-safe

→ runs scripts/rebuild-plugins.sh --stage-repo

User: I also need the installed version pointer advanced (/plugin update)
/ship rollout

→ runs scripts/rollout-plugins.sh

User: commit and push
→ Agent: "I can commit with the following. Approve?"
→ Show: git status, git diff, proposed commit message
User: "Yes, commit."
→ Agent: git add <files> && git commit -m "..."
→ git push origin main  (with user approval)
```

Ask again if you are unsure whether the user has approved.
