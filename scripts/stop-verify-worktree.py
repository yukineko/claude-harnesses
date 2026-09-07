#!/usr/bin/env python3
"""Stop hook: refuse to stop while sitting on the MAIN checkout with uncommitted
changes.

CLAUDE.md 最上位の方針 8 + the Stop-gate invariant (判定不能は block へ): the
session's work belongs in a worktree, so ending a turn with dirty, uncommitted
changes on the MAIN tree is the failure this catches. If the session is in a
worktree, or the main tree is clean, the stop is allowed. If it is on the main
tree AND dirty, the stop is BLOCKED so the changes get moved into a worktree
before the turn ends.

Fail-closed throughout (CLAUDE.md 3, 判定不能は制限側へ). Exactly ONE git outcome
allows the stop without an inspection: git itself reporting that this is not a
git repository, which means there is no main tree here to be dirty. Every other
failure of every probe — git not runnable, a timeout, a non-zero exit for any
other reason (dubious ownership, a corrupt repo, an unsupported rev-parse flag) —
leaves this hook unable to tell a worktree from a dirty main tree, and resolves
to a block. A probe that could not run has not passed.

That distinction is the whole point: the earlier version collapsed "git said no
repo" and "git could not answer" into one `return 0`, so any git malfunction
silently waved through the exact case this gate exists to catch.

Bounded allow: `stop_hook_active` (a re-entrant stop after this hook already
fired) resolves to allow, so a genuinely stuck state cannot trap the turn — the
same bounded-allow the repo's other Stop gates use.

    exit 0   allow the stop
    exit 2   block the stop; stderr is the reason shown to the model
"""

from __future__ import annotations

import json
import os
import subprocess
import sys


def _git(cwd: str, *args: str):
    try:
        return subprocess.run(
            ("git", *args), cwd=cwd, capture_output=True, text=True, timeout=15
        )
    except (OSError, subprocess.SubprocessError):
        return None


# Probe outcomes. Three answers, not two — the same tri-state the rest of this
# repo's gates use (crates/blastguard/src/model.rs, harness_core::verdict).
OK = "ok"
NO_REPO = "no-repo"
UNDET = "undetermined"

# git's own wording for "there is no repository here", which is the ONLY
# non-zero exit that legitimately means "nothing to verify".
_NO_REPO_MARKERS = (
    "not a git repository",
    "not a git repo",  # older/localised phrasings that still name the class
)


def _probe(cwd: str, *args: str) -> tuple[str, str | None]:
    """Run a git query and classify the outcome as OK / NO_REPO / UNDET."""
    r = _git(cwd, *args)
    if r is None:
        return UNDET, None  # git not runnable, or it timed out
    if r.returncode == 0:
        value = r.stdout.strip()
        # A zero exit with no answer is still no answer.
        return (OK, value) if value else (UNDET, None)
    stderr = (r.stderr or "").lower()
    if any(marker in stderr for marker in _NO_REPO_MARKERS):
        return NO_REPO, None
    return UNDET, None


BLOCK = """Do not stop yet: this turn is ending on the MAIN working tree with uncommitted changes.

CLAUDE.md 最上位の方針 8: work is authored in a worktree, never left dirty on
main. Move the changes into a worktree and commit them there:

    git worktree add -b <branch> <path-outside-repo> HEAD
    git -C <path> ...    # or: stash, then apply inside the worktree
    # commit in the worktree, then merge onto main

Then it is safe to stop.
"""

UNDETERMINED = """Do not stop yet: could not determine whether this turn is ending dirty on main.

Failed probe: {what}

A check that could not run has not passed (CLAUDE.md 3), so this resolves to a
block rather than an allow. Note this is NOT the same as "there is no git
repository here" — git said something else, or could not be run at all. Re-run
from a valid checkout, or fix what is stopping git from answering.
"""


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        payload = {}
    if isinstance(payload, dict) and payload.get("stop_hook_active"):
        return 0  # bounded allow — never trap the turn

    cwd = (payload.get("cwd") if isinstance(payload, dict) else None) or os.getcwd()

    st_git, git_dir = _probe(cwd, "rev-parse", "--absolute-git-dir")
    st_common, common = _probe(
        cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"
    )

    # Undetermined FIRST: if either probe could not answer, the NO_REPO reading
    # of the other one proves nothing, so it must not be allowed to win.
    if st_git == UNDET or st_common == UNDET:
        which = "git rev-parse --absolute-git-dir" if st_git == UNDET else (
            "git rev-parse --git-common-dir"
        )
        sys.stderr.write(UNDETERMINED.format(what=which))
        return 2
    if st_git == NO_REPO or st_common == NO_REPO:
        return 0  # git says there is no repository here — nothing to verify
    assert git_dir is not None and common is not None  # OK implies a value
    if os.path.realpath(git_dir) != os.path.realpath(common):
        return 0  # in a worktree — the sanctioned place

    # Main checkout: allowed only if clean.
    status = _git(cwd, "status", "--porcelain")
    if status is None or status.returncode != 0:
        sys.stderr.write(UNDETERMINED.format(what="git status --porcelain"))
        return 2
    if status.stdout.strip():
        sys.stderr.write(BLOCK)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
