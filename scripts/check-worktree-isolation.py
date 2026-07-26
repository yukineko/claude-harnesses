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

A merge commit is the ALLOWED operation on main, and it is recognised by
`MERGE_HEAD` existing in the git dir at commit time. Nothing else is exempt —
there is deliberately no environment-variable escape hatch, because a correctly
scoped gate makes a bypass flag unnecessary (a change that belongs on main can
always be made in a worktree and merged).

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
    # allowed operation; MERGE_HEAD marks it.
    if os.path.exists(os.path.join(git_dir, "MERGE_HEAD")):
        return 0

    head = _git("rev-parse", "--short", "HEAD") or "<base>"
    sys.stderr.write(BLOCK.format(head=head))
    return 1


if __name__ == "__main__":
    sys.exit(main())
