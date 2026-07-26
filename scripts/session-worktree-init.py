#!/usr/bin/env python3
"""SessionStart hook: if the session is starting in the MAIN checkout, create a
session worktree and direct the agent into it.

CLAUDE.md 最上位の方針 8: no work happens on the main tree. This hook makes the
worktree exist at session start so the agent has somewhere to go — it checks the
starting checkout and, when it is the main tree (git-dir == git-common-dir),
creates `session-<id>` off HEAD in a repo-external directory and prints the
instruction to `cd` into it. (The Bash tool's cwd persists across calls, so a
single `cd` moves the whole session in.) The edit/commit guards do the actual
enforcing; this hook removes the excuse of "there was no worktree".

Fail-soft: this is a setup hook, not a gate. Any error prints a best-effort note
and exits 0 — the edit-time guards and the pre-commit check still block the main
tree regardless of whether this hook managed to pre-make the worktree.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys


def _git(cwd: str, *args: str) -> str | None:
    try:
        out = subprocess.run(
            ("git", *args), cwd=cwd, capture_output=True, text=True, timeout=15
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.strip() if out.returncode == 0 else None


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        payload = {}
    cwd = (payload.get("cwd") if isinstance(payload, dict) else None) or os.getcwd()
    sid = (payload.get("session_id") if isinstance(payload, dict) else None) or "adhoc"
    short = str(sid).split("-")[0][:8]

    git_dir = _git(cwd, "rev-parse", "--absolute-git-dir")
    common = _git(cwd, "rev-parse", "--path-format=absolute", "--git-common-dir")
    if git_dir is None or common is None:
        return 0  # not a git repo — nothing to do
    if os.path.realpath(git_dir) != os.path.realpath(common):
        # Already in a worktree — the session is where it should be.
        print("[worktree-init] session is in a worktree ✓")
        return 0

    root = _git(cwd, "rev-parse", "--show-toplevel") or cwd
    base = os.path.basename(root.rstrip("/"))
    wt_parent = os.path.join(os.path.dirname(root.rstrip("/")), f".{base}-worktrees")
    branch = f"session-{short}"
    wt_path = os.path.join(wt_parent, branch)

    try:
        os.makedirs(wt_parent, exist_ok=True)
    except OSError:
        pass

    if not os.path.isdir(wt_path):
        # Fresh worktree off current HEAD. If the branch name is taken, fall back
        # to attaching without -b so we still land in a worktree.
        made = subprocess.run(
            ("git", "worktree", "add", "-b", branch, wt_path, "HEAD"),
            cwd=root, capture_output=True, text=True,
        )
        if made.returncode != 0:
            subprocess.run(
                ("git", "worktree", "add", wt_path, "HEAD"),
                cwd=root, capture_output=True, text=True,
            )

    if os.path.isdir(wt_path):
        print(
            "⚠ You started on the MAIN working tree. Direct edits/commits to it "
            "are BLOCKED (CLAUDE.md 8).\n"
            f"A session worktree is ready at:\n    {wt_path}\n"
            f"Run this before any edit — the Bash cwd persists across calls:\n"
            f"    cd {wt_path}\n"
            "Author everything there, commit in the worktree, then merge onto main."
        )
    else:
        print(
            "⚠ You are on the MAIN working tree and a session worktree could not "
            "be auto-created. Create one yourself before editing:\n"
            f"    git worktree add -b {branch} <path-outside-repo> HEAD && cd <path>"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
