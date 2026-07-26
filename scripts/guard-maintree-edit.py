#!/usr/bin/env python3
"""PreToolUse hook: refuse an Edit/Write to THIS project's MAIN working tree.

CLAUDE.md 最上位の方針 8 forbids editing the shared main working tree directly —
every edit must happen in a `git worktree`, because two sessions sharing one
index is the single conflict git cannot resolve. That rule used to live only in
prose, and prose is a request, not an enforcement: the implementer (§6, and an
LLM most of all) drifts to "just this one small edit on main is fine". This hook
makes the drift impossible for the common edit path — the tool call passes
through this process before any file is touched, so a main-tree edit can be
REFUSED outright rather than merely discouraged.

Scope, stated precisely so the ALLOW paths are not accidents:

  * Only the EDIT tools are inspected — Edit / Write / MultiEdit / NotebookEdit.
    Every other tool exits 0 (allow).
  * Only THIS project's main checkout is guarded. "This project" is
    realpath($CLAUDE_PROJECT_DIR); "main checkout" is where the target file's
    `--git-dir` equals its `--git-common-dir` (a linked worktree's git-dir lives
    under `<common>/worktrees/<name>`, so they differ there). A file in a
    worktree — nested inside the repo or outside it — is therefore ALLOWED, which
    is the whole point: work in a worktree and this hook is silent.
  * Files OUTSIDE the project main tree are allowed: the memory dir under
    ~/.claude, the session scratchpad under /tmp, any other repo. Those are not
    the shared index this rule protects.
  * git-ignored paths under the main tree are allowed (personal, uncommitted
    scratch such as settings.local.json). They never enter the shared history,
    so editing them on main creates none of the index-sharing harm.

Fail-closed (CLAUDE.md 3): a target that IS under the project main tree but whose
worktree status cannot be determined (a git call errors unexpectedly) resolves to
DENY, not allow. "Cannot determine whether this is main" is not "this is safe".

Protocol (same as deny-no-verify.py): reads the PreToolUse JSON payload on
stdin.

    exit 0   allow
    exit 2   deny; stderr is shown to the model as the reason
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

EDIT_TOOLS = ("Edit", "Write", "MultiEdit", "NotebookEdit")


def _target_path(payload: dict) -> str | None:
    """The absolute filesystem path an edit tool would write, or None."""
    ti = payload.get("tool_input") or {}
    # Edit/Write/MultiEdit use file_path; NotebookEdit uses notebook_path.
    for key in ("file_path", "notebook_path"):
        val = ti.get(key)
        if isinstance(val, str) and val:
            return val
    return None


def _nearest_existing_dir(path: str) -> str:
    """Walk up to the nearest existing ancestor dir (a new file's parent may not
    exist yet). Absolute input guaranteed by the caller."""
    d = os.path.dirname(path) or path
    while d and d != os.path.dirname(d) and not os.path.isdir(d):
        d = os.path.dirname(d)
    return d or "/"


def _git(cwd: str, *args: str) -> str | None:
    """Run a git query in cwd; return stripped stdout, or None on any failure."""
    try:
        out = subprocess.run(
            ("git", *args),
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    return out.stdout.strip()


def _under(child: str, parent: str) -> bool:
    parent = parent.rstrip("/")
    return child == parent or child.startswith(parent + "/")


def decide(payload: dict) -> tuple[int, str]:
    """(exit_code, stderr). 0 allow, 2 deny."""
    if payload.get("tool_name") not in EDIT_TOOLS:
        return 0, ""

    raw = _target_path(payload)
    if raw is None:
        # An edit tool with no resolvable path: cannot locate it, cannot clear
        # it. This hook is the edit-time gate, so cannot-determine -> deny (3.).
        return 2, DENY_UNDETERMINED

    target = os.path.realpath(raw)

    proj_env = os.environ.get("CLAUDE_PROJECT_DIR")
    if not proj_env:
        # Without the project anchor we cannot scope "this project's main tree".
        # Fall back to the intrinsic test (git-dir == git-common-dir) so the hook
        # still enforces, rather than silently allowing everything.
        proj = None
    else:
        proj = os.path.realpath(proj_env)
        if not _under(target, proj):
            # Outside this project entirely (memory, scratchpad, other repo).
            return 0, ""

    query_dir = _nearest_existing_dir(target)
    git_dir = _git(query_dir, "rev-parse", "--absolute-git-dir")
    common = _git(query_dir, "rev-parse", "--path-format=absolute", "--git-common-dir")

    if git_dir is None or common is None:
        # The file is not inside any git repo (or git is unusable here). If it is
        # nonetheless under the project root, we cannot confirm it is a worktree
        # -> fail closed. If proj is unknown, a non-repo path is genuinely
        # outside our concern -> allow.
        if proj is not None and _under(target, proj):
            return 2, DENY_UNDETERMINED
        return 0, ""

    is_main_checkout = os.path.realpath(git_dir) == os.path.realpath(common)
    if not is_main_checkout:
        # Linked worktree — exactly where edits are supposed to happen.
        return 0, ""

    # Main checkout. When we have a project anchor, only guard THIS project's
    # main tree; a different repo's main tree is not ours to police here.
    if proj is not None:
        toplevel = _git(query_dir, "rev-parse", "--show-toplevel")
        if toplevel is not None and os.path.realpath(toplevel) != proj:
            return 0, ""

    # git-ignored scratch under the main tree never enters shared history.
    ignored = subprocess.run(
        ("git", "check-ignore", "-q", target),
        cwd=query_dir,
        capture_output=True,
    )
    if ignored.returncode == 0:
        return 0, ""

    return 2, DENY_MAINTREE.format(path=raw)


DENY_MAINTREE = """Refused: editing `{path}` writes to this project's MAIN working tree.

CLAUDE.md 最上位の方針 8: the main working tree is never edited directly — every
edit goes through a `git worktree`, because a shared index is the one conflict
git cannot merge, and another session is ALWAYS assumed to be live. This is not a
suggestion the hook is reminding you of; it is enforced here in code.

Do this instead:

    git worktree add -b <branch> <path-outside-or-nested> <base>

then make the edit against the file inside that worktree, commit there, and merge
into main. Merge / conflict-resolution are the ONLY operations allowed on the
main tree.
"""

DENY_UNDETERMINED = """Refused: could not determine whether this edit targets the main working tree.

A check that could not run has not passed. Under CLAUDE.md 3 (cannot-determine
resolves to the restricted side), an edit whose worktree status is unknown is
refused rather than allowed. Make the edit inside a git worktree and it will be
permitted.
"""


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, UnicodeDecodeError, ValueError):
        # A payload that will not decode is a harness fault, not an author edit,
        # and this hook has a commit-time backstop (check-worktree-isolation.py)
        # that catches any edit which actually reaches a commit. Taking the whole
        # session down on an undecodable payload is not warranted; allow, and let
        # the backstop hold the line. (Same reasoning as deny-no-verify.py.)
        return 0
    if not isinstance(payload, dict):
        return 0
    code, reason = decide(payload)
    if code != 0:
        sys.stderr.write(reason)
    return code


if __name__ == "__main__":
    sys.exit(main())
