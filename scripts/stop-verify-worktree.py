#!/usr/bin/env python3
"""Stop hook: refuse to stop while sitting on the MAIN checkout with uncommitted
changes.

CLAUDE.md 最上位の方針 8 + the Stop-gate invariant (判定不能は block へ): the
session's work belongs in a worktree, so ending a turn with dirty, uncommitted
changes on the MAIN tree is the failure this catches. If the session is in a
worktree, or the main tree is clean, the stop is allowed. If it is on the main
tree AND dirty, the stop is BLOCKED so the changes get moved into a worktree
before the turn ends.

Fail-closed: if it looks like the main tree but git cannot report its status, the
stop is blocked (cannot-determine → restricted side).

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


def _out(cwd: str, *args: str) -> str | None:
    r = _git(cwd, *args)
    if r is None or r.returncode != 0:
        return None
    return r.stdout.strip()


BLOCK = """Do not stop yet: this turn is ending on the MAIN working tree with uncommitted changes.

CLAUDE.md 最上位の方針 8: work is authored in a worktree, never left dirty on
main. Move the changes into a worktree and commit them there:

    git worktree add -b <branch> <path-outside-repo> HEAD
    git -C <path> ...    # or: stash, then apply inside the worktree
    # commit in the worktree, then merge onto main

Then it is safe to stop.
"""

UNDETERMINED = """Do not stop yet: could not read the main tree's status to confirm it is clean.

A check that could not run has not passed (CLAUDE.md 3), so this resolves to a
block. Re-run from a valid checkout.
"""


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        payload = {}
    if isinstance(payload, dict) and payload.get("stop_hook_active"):
        return 0  # bounded allow — never trap the turn

    cwd = (payload.get("cwd") if isinstance(payload, dict) else None) or os.getcwd()

    git_dir = _out(cwd, "rev-parse", "--absolute-git-dir")
    common = _out(cwd, "rev-parse", "--path-format=absolute", "--git-common-dir")
    if git_dir is None or common is None:
        return 0  # not a git repo — nothing to verify
    if os.path.realpath(git_dir) != os.path.realpath(common):
        return 0  # in a worktree — the sanctioned place

    # Main checkout: allowed only if clean.
    status = _git(cwd, "status", "--porcelain")
    if status is None or status.returncode != 0:
        sys.stderr.write(UNDETERMINED)
        return 2
    if status.stdout.strip():
        sys.stderr.write(BLOCK)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
