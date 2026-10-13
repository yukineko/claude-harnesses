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

Refused-then-changed (e033c406, signal 2 of scripts/deny_ledger.py). BEFORE the
cwd checks above — so it applies whether the session sits on main or in a
worktree — every target that guard-maintree-edit.py refused in this session
(guard-maintree-bash.py records nothing in the ledger since the 2026-10-04 ruling) (the per-session ledger
`~/.claude/state/maintree-deny/<session_id>.jsonl` — or, when that cannot be
written, the temp-dir fallback `<tmp>/maintree-deny-<uid>/<session_id>.jsonl`
(both are always read)) is re-snapshotted. If one
that is still under main has changed since its refusal (its `git status
--porcelain` line differs, or it is dirty now and its existence / type / mtime /
size differ), the stop is BLOCKED, quoting the refusal: the refused change was
made anyway by another spelling. An unreadable or corrupt ledger, a target whose
state cannot be read, or an unusable session_id also blocks — a bad ledger file
only for 20 minutes from its mtime, after which it is renamed aside to
`<name>.corrupt-<ts>`, its valid deny lines are carried into a fresh ledger
file (and still judged here), the unparseable lines are dropped (named in a
stderr notice) and the stop proceeds. A deny whose recorded main root no longer
exists cannot be re-snapshotted and blocks as undetermined until the ledger is
cleared or pruned. A payload without a session_id key has no ledger
(deny_ledger.py, RESIDUALS).

Attribution (233f819c, scripts/edit_ledger.py). Dirt on main is not always this
session's: another concurrent session may own it (the maintree guards record
every main-tree write they ALLOW into a per-session edit ledger). A dirty path
(`git status --porcelain`) is EXCLUDED from blocking only when the ledger is
readable, the path has a recorded owner set, that set is non-empty and does NOT
contain the stopping session (payload session_id). Everything else blocks and
the block message names those paths: an unrecorded path, a path this session
wrote (alone or with others), an unreadable or corrupt ledger, an unusable
session_id, a missing edit_ledger module, or owners() raising. Attribution that
cannot be determined blocks; it is never read as "someone else's".

Bounded allow: `stop_hook_active` (a re-entrant stop after this hook already
fired) resolves to allow, so a genuinely stuck state cannot trap the turn — the
same bounded-allow the repo's other Stop gates use. It applies to the ledger
check too.

    exit 0   allow the stop
    exit 2   block the stop; stderr is the reason shown to the model
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

try:
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import deny_ledger  # noqa: E402
    _LEDGER_IMPORT_ERROR = ""
except Exception as _e:  # noqa: BLE001 — resolves to a block in main()
    deny_ledger = None
    _LEDGER_IMPORT_ERROR = f"{type(_e).__name__}: {_e}"


try:
    import edit_ledger  # noqa: E402
except Exception:  # noqa: BLE001 — attribution unavailable => nothing excluded
    edit_ledger = None


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

Dirty paths this block is on (from git status --porcelain):
{paths}
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

    if deny_ledger is None:
        sys.stderr.write(UNDETERMINED.format(
            what=f"loading scripts/deny_ledger.py ({_LEDGER_IMPORT_ERROR})"))
        return 2
    if isinstance(payload, dict):
        changed = deny_ledger.stop_check(payload)
        if changed is not None:
            sys.stderr.write(changed)
            return 2

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
        blocking = _blocking_paths(status.stdout, payload, cwd)
        if blocking:
            sys.stderr.write(BLOCK.format(
                paths="\n".join(f"  {p}" for p in blocking)))
            return 2
    return 0


def _porcelain_paths(out: str) -> list[str]:
    """Paths from `git status --porcelain` (rename "a -> b" yields both)."""
    paths: list[str] = []
    for ln in out.splitlines():
        if len(ln) < 4:
            continue
        body = ln[3:]
        parts = body.split(" -> ") if ("R" in ln[:2] or "C" in ln[:2]) else [body]
        for p in parts:
            p = p.strip()
            if len(p) >= 2 and p[0] == '"' and p[-1] == '"':
                p = p[1:-1]
            paths.append(p)
    return paths


def _blocking_paths(out: str, payload, cwd: str) -> list[str]:
    """Dirty paths that still block. Fails closed to ALL of them."""
    paths = _porcelain_paths(out)
    if not paths:
        return [ln for ln in out.splitlines() if ln.strip()]
    try:
        if edit_ledger is None or not isinstance(payload, dict):
            return paths
        sid = payload.get("session_id")
        if not isinstance(sid, str) or not sid:
            return paths
        st, top = _probe(cwd, "rev-parse", "--show-toplevel")
        if st != OK or top is None:
            return paths
        res = edit_ledger.owners(top)
        if not isinstance(res, edit_ledger.Known):
            return paths
        table = res.value
        return [p for p in paths
                if not (table.get(p) and sid not in table[p])]
    except Exception:  # noqa: BLE001 — a raising probe blocks
        return paths


if __name__ == "__main__":
    try:
        _rc = main()
    except Exception as _crash:  # noqa: BLE001
        # An uncaught exception exits 1, which Claude Code treats as a
        # non-blocking error, i.e. the call proceeds. A check that crashed has
        # not passed (CLAUDE.md 3), so it resolves to exit 2 (e033c406).
        sys.stderr.write(
            f"Do not stop yet: stop-verify-worktree.py crashed ({type(_crash).__name__}: {_crash}); "
            "a check that could not run has not passed.\n")
        _rc = 2
    sys.exit(_rc)
