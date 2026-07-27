#!/usr/bin/env python3
"""pre-commit gate: refuse a non-merge commit made from the MAIN working tree.

This is the ROUTE-INDEPENDENT enforcement of CLAUDE.md 最上位の方針 8. The
edit-time PreToolUse guards (guard-maintree-edit.py, guard-maintree-bash.py)
close the common ways to dirty the main tree, but they are per-tool and a
determined route around them exists (an interpreter wrapper, a second process,
a tool the guards do not model). This check does not care HOW the main tree came
to be dirty: a commit is the moment a change becomes durable and shared, and a
commit taken from the main checkout — rather than from a worktree — is refused
outright. The only thing you can do to the main tree is INTEGRATE (merge /
conflict-resolution); everything else happens in a worktree and arrives on main
through a merge.

How "main checkout vs worktree" is decided, and why it is reliable: a linked
worktree's `--git-dir` is `<common>/worktrees/<name>`, which is never equal to
its `--git-common-dir`. The main checkout is the one place they are equal. So:

    git-dir == git-common-dir   ->  main checkout   ->  refuse (unless a merge)
    git-dir != git-common-dir   ->  a worktree      ->  allow

A merge commit is the ALLOWED operation on main, and it is recognised two ways:

  1. `MERGE_HEAD` existing in the git dir at commit time — checked with a
     bounded retry (see `MERGE_HEAD_RETRY_*` below), because git writing the
     file and this script observing it are not the same instant. Measured
     live (backlog, 3 reproductions): a clean `git merge --no-edit <branch>`
     was blocked here, and re-running this script standalone moments later,
     with no other change, returned allow — a filesystem-visibility race, not
     an absence of a merge. A single unretried `os.path.exists` cannot tell
     "no merge in progress" apart from "a merge in progress whose marker has
     not become visible yet".
  2. The invocation context declaring it, corroborated by git's own record —
     see `declared_merge_integration` below. This is the same fix already
     applied to the Rust main-tree guard
     (`crates/condukt/src/maintree.rs::declared_integration`, exercised by
     `crates/condukt/tests/main_tree_guard_merge.rs`) for the same class of
     gap: git does not always write `MERGE_HEAD` for a merge that is
     proceeding (a `--no-ff` merge that succeeds never writes it at all), so
     the on-disk marker alone is insufficient on more than one path. This
     signal does not depend on filesystem timing, so it needs no retry.

Neither signal is a blanket environment-variable escape hatch: (2) requires
`.githooks/pre-merge-commit`'s `CONDUKT_GIT_HOOK=pre-merge-commit` declaration
to be corroborated by `GIT_REFLOG_ACTION`, which git itself — not the caller —
sets to `merge <ref>` (or `pull <ref>`) while running that hook, and leaves
unset for an ordinary `git commit`. A hand-forged `CONDUKT_GIT_HOOK` on a plain
commit does not unlock it (`GIT_REFLOG_ACTION` stays unset); see
`test_check_worktree_isolation.py`'s
`test_forged_hook_declaration_without_git_reflog_action_still_blocks`. And (1)'s
retry is bounded and short (`MERGE_HEAD_RETRY_ATTEMPTS` x
`MERGE_HEAD_RETRY_DELAY_S`, well under one second total) — it absorbs a visible
lag, it never waits indefinitely, and it never falls back to "allow" merely
because it gave up looking; giving up still means the on-disk half of the
check answers "no".

Fail-closed (CLAUDE.md 3): if git cannot answer where this commit is being made,
the commit is BLOCKED. "Cannot determine whether this is the main tree" is not
"this is a worktree, proceed".

    exit 0   allow the commit
    exit 1   block; stderr explains why
"""

from __future__ import annotations

import os
import subprocess
import sys
import time


def _git(*args: str) -> str | None:
    try:
        out = subprocess.run(
            ("git", *args), capture_output=True, text=True, timeout=10
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    return out.stdout.strip()


# Bounded retry budget for the MERGE_HEAD visibility race. Small and finite on
# purpose (CLAUDE.md 3: never an unbounded wait, never "gave up -> allow"): 5
# attempts x 100ms = at most ~0.5s added to a commit that is NOT a merge (the
# common case), and comfortably covers the lag observed live.
MERGE_HEAD_RETRY_ATTEMPTS = 5
MERGE_HEAD_RETRY_DELAY_S = 0.1

# Set by `.githooks/pre-merge-commit` before it `exec`s `pre-commit`, naming the
# hook git actually invoked. Mirrors crates/condukt/src/maintree.rs::HOOK_ENV.
HOOK_ENV = "CONDUKT_GIT_HOOK"

# git's own record of the operation it is performing. Measured (this script's
# test suite, EnvSignalMeasuredFromARealMerge, and
# crates/condukt/tests/main_tree_guard_merge.rs): git sets this to `merge <ref>`
# while running pre-merge-commit, and leaves it unset for an ordinary `git
# commit`. Mirrors crates/condukt/src/maintree.rs::REFLOG_ACTION_ENV.
REFLOG_ACTION_ENV = "GIT_REFLOG_ACTION"


def declared_merge_integration(env: dict[str, str]) -> bool:
    """Whether the invocation context itself says a merge is under way.

    True only when BOTH halves agree: `.githooks/pre-merge-commit` declared it
    via `HOOK_ENV`, AND git's own `REFLOG_ACTION_ENV` corroborates it. The
    second half is written by git, not the caller, so exporting `HOOK_ENV` by
    hand on an ordinary commit does not exclude it — `REFLOG_ACTION_ENV` stays
    unset in that case. See the module docstring and
    crates/condukt/src/maintree.rs::declared_integration, whose contract this
    mirrors for the python gate.
    """
    hook = env.get(HOOK_ENV)
    if hook is None or hook.strip() != "pre-merge-commit":
        return False
    action = env.get(REFLOG_ACTION_ENV)
    if action is None:
        return False
    action = action.strip()
    if not action:
        return False
    verb = action.split(None, 1)[0]
    # `git pull` that merges reports "pull"; `git merge` reports "merge".
    return verb in ("merge", "pull")


def _merge_head_visible(git_dir: str) -> bool:
    """MERGE_HEAD existing in the git dir, with a bounded retry to absorb the
    filesystem-visibility lag between git writing it and this script observing
    it (see module docstring). Finite and short: giving up still answers
    False, never True."""
    marker = os.path.join(git_dir, "MERGE_HEAD")
    for attempt in range(MERGE_HEAD_RETRY_ATTEMPTS):
        if os.path.exists(marker):
            return True
        if attempt < MERGE_HEAD_RETRY_ATTEMPTS - 1:
            time.sleep(MERGE_HEAD_RETRY_DELAY_S)
    return False


BLOCK = """Refused: this commit is being made directly in the MAIN working tree.

CLAUDE.md 最上位の方針 8: the main working tree is for INTEGRATION only (merge /
conflict-resolution). A change is authored in a `git worktree` and reaches main
through a merge — never by committing on main itself. Another session is always
assumed to be live sharing this index.

    git worktree add -b <branch> <path> {head}
    # ... make and commit the change inside that worktree ...
    git merge <branch>        # integrate it onto main

There is no bypass flag for this on purpose. If the change genuinely belongs on
main right now, it still goes through a worktree and a merge.
"""

UNDETERMINED = """Refused: could not determine whether this commit is in the main tree or a worktree.

`git rev-parse` did not answer, so this resolves to a block (CLAUDE.md 3:
cannot-determine takes the restricted side). Re-run from a valid git checkout.
"""


def main() -> int:
    git_dir = _git("rev-parse", "--absolute-git-dir")
    common = _git("rev-parse", "--path-format=absolute", "--git-common-dir")
    if git_dir is None or common is None:
        sys.stderr.write(UNDETERMINED)
        return 1

    if os.path.realpath(git_dir) != os.path.realpath(common):
        # A worktree — the sanctioned place to commit.
        return 0

    # Main checkout. A merge (or a conflicted merge being concluded) is the one
    # allowed operation. Checked two ways (see module docstring): the
    # invocation context (race-free) first, then the on-disk marker with a
    # bounded retry (absorbs the visibility race; never unbounded, never
    # allow-on-giving-up).
    if declared_merge_integration(os.environ):
        return 0
    if _merge_head_visible(git_dir):
        return 0

    head = _git("rev-parse", "--short", "HEAD") or "<base>"
    sys.stderr.write(BLOCK.format(head=head))
    return 1


if __name__ == "__main__":
    sys.exit(main())
