#!/usr/bin/env python3
"""Pre/PostToolUse hook: detect a Bash command that MODIFIED this project's main tree.

CLAUDE.md 8: only integration (merge, conflict resolution) may change the main
(primary) working tree; another session is always assumed to share its index.

This guard OBSERVES the effect instead of predicting it from command syntax.
Its predecessor (2739 lines, replaced 2026-10-03 by user ruling) tried to decide
from the command text whether python / node / perl / awk / shell payloads would
write into main. That is undecidable; every closed gap opened another false
positive or false negative (4a64e6ff and ~30 sibling tickets).

  PreToolUse (Bash)   snapshot main: `git status --porcelain=v2 -z
                      --untracked-files=all`, a content hash of every dirty or
                      untracked path, HEAD, and whether MERGE_HEAD exists.
                      Stored under <git-common-dir>/maintree-guard/ keyed by
                      session_id + tool_use_id. Any command text is allowed.
  PostToolUse / PostToolUseFailure (Bash)
                      re-snapshot and compare with the stored Pre snapshot:
                      identical                             -> 0
                      MERGE_HEAD exists after               -> 0 (resolving a merge)
                      HEAD moved, tree clean, and the new HEAD is a merge
                      commit or is contained in a ref other than the branch
                      main has checked out (a fast-forward integration) -> 0
                      anything else                         -> 2, naming the paths

Exit 2 at Post cannot undo the command — the write has happened. It stops the
turn from proceeding as if main were untouched and names what changed, so the
change is reverted or moved into a worktree openly.

Cannot determine -> 2, never 0 (CLAUDE.md 3): a malformed payload, git failing,
CLAUDE_PROJECT_DIR not in a repository, a snapshot that cannot be written, read
or decoded, a Pre snapshot that is missing at Post, a dirty path that cannot be
hashed, or a failing merge/containment check.

Known blind spots (not visible to `git status`): files under .git itself
(.git/config, .git/hooks) and gitignored files. Changes by ANOTHER session
between this call's Pre and Post are attributed to this call; the message says so.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

REFUSE = 2
ALLOW = 0


class Undetermined(Exception):
    pass


def git(cwd: Path, *args: str) -> str:
    try:
        proc = subprocess.run(
            ["git", "-C", str(cwd), *args], capture_output=True, text=True
        )
    except OSError as exc:
        raise Undetermined("git could not be executed: %s" % exc) from exc
    if proc.returncode != 0:
        raise Undetermined(
            "`git %s` exited %d: %s"
            % (" ".join(args), proc.returncode, proc.stderr.strip() or "(no stderr)")
        )
    return proc.stdout


def locate(project: str) -> tuple[Path, Path]:
    """(common git dir, primary worktree) of the repo containing `project`."""
    if not project:
        raise Undetermined("CLAUDE_PROJECT_DIR is not set")
    common = git(
        Path(project), "rev-parse", "--path-format=absolute", "--git-common-dir"
    ).strip()
    if not common:
        raise Undetermined("`git rev-parse --git-common-dir` printed nothing")
    common_dir = Path(common)
    return common_dir, common_dir.parent


def hash_path(path: Path) -> str:
    if path.is_symlink():
        return "link:" + os.readlink(path)
    if path.is_dir():
        # porcelain lists an untracked nested repo / submodule as a directory
        return "dir"
    if not path.exists():
        return "absent"
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def status_paths(porcelain: str) -> list[str]:
    """Every path named by `git status --porcelain=v2 -z` output."""
    fields = porcelain.split("\0")
    paths = []
    i = 0
    while i < len(fields):
        rec = fields[i]
        i += 1
        if not rec:
            continue
        kind = rec[0]
        if kind == "1":
            paths.append(rec.split(" ", 8)[8])
        elif kind == "2":
            paths.append(rec.split(" ", 9)[9])
            if i < len(fields):
                paths.append(fields[i])  # the rename/copy source
                i += 1
        elif kind == "u":
            paths.append(rec.split(" ", 10)[10])
        elif kind in "?!":
            paths.append(rec[2:])
        else:
            raise Undetermined("unrecognized git status record %r" % rec[:40])
    return paths


def snapshot(main: Path, common: Path) -> dict:
    porcelain = git(main, "status", "--porcelain=v2", "-z", "--untracked-files=all")
    hashes = {}
    for rel in status_paths(porcelain):
        try:
            hashes[rel] = hash_path(main / rel)
        except OSError as exc:
            raise Undetermined("cannot hash dirty path %s: %s" % (rel, exc)) from exc
    return {
        "porcelain": porcelain,
        "hashes": hashes,
        "head": git(main, "rev-parse", "HEAD").strip(),
        "merge_head": (common / "MERGE_HEAD").exists(),
    }


def state_file(common: Path, payload: dict) -> Path:
    sid = payload.get("session_id")
    tid = payload.get("tool_use_id")
    if not isinstance(sid, str) or not sid or not isinstance(tid, str) or not tid:
        raise Undetermined("payload has no session_id / tool_use_id to key the snapshot")
    key = hashlib.sha256(("%s\0%s" % (sid, tid)).encode()).hexdigest()[:32]
    return common / "maintree-guard" / (key + ".json")


def integration_head(main: Path, head: str) -> bool:
    """True if `head` is a merge commit or lives on a ref other than main's branch."""
    parents = git(main, "rev-list", "--parents", "-n1", head).split()
    if not parents or parents[0] != head:
        raise Undetermined("`git rev-list --parents` did not describe %s" % head)
    if len(parents) >= 3:
        return True
    proc = subprocess.run(
        ["git", "-C", str(main), "symbolic-ref", "-q", "HEAD"],
        capture_output=True,
        text=True,
    )
    if proc.returncode not in (0, 1):
        raise Undetermined("`git symbolic-ref -q HEAD` exited %d" % proc.returncode)
    branch = proc.stdout.strip()
    refs = git(main, "for-each-ref", "--contains", head, "--format=%(refname)").split()
    return any(r != branch for r in refs)


def changed_paths(before: dict, after: dict) -> list[str]:
    """Paths whose content hash or status record (index state) differs."""
    a, b = before["hashes"], after["hashes"]
    ra, rb = status_records(before["porcelain"]), status_records(after["porcelain"])
    paths = set(a) | set(b) | set(ra) | set(rb)
    return sorted(p for p in paths if a.get(p) != b.get(p) or ra.get(p) != rb.get(p))


def status_records(porcelain: str) -> dict[str, str]:
    """path -> its status record line, for naming index-only changes."""
    out = {}
    for rec in porcelain.split("\0"):
        if rec[:1] in ("1", "2", "u", "?", "!"):
            for p in status_paths(rec + "\0"):
                if p:  # a rename record's source field is a separate entry
                    out[p] = rec
    return out


ORPHAN_AGE_SECS = 24 * 3600


def sweep_orphans(state_dir: Path) -> None:
    """Remove snapshots older than ORPHAN_AGE_SECS.

    A call denied by another PreToolUse hook never gets a Post event, so its
    snapshot is orphaned. Younger files may belong to an in-flight call (of
    this or another session) and are kept. Best-effort: a failed sweep only
    leaves files behind; it judges nothing.
    """
    try:
        entries = list(state_dir.iterdir())
    except OSError:
        return
    cutoff = time.time() - ORPHAN_AGE_SECS
    for f in entries:
        try:
            if f.is_file() and f.stat().st_mtime < cutoff:
                f.unlink()
        except OSError:
            pass


def pre(payload: dict, common: Path, main: Path) -> int:
    snap = snapshot(main, common)
    path = state_file(common, payload)
    sweep_orphans(path.parent)
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps(snap))
        tmp.replace(path)
    except OSError as exc:
        raise Undetermined("cannot store the snapshot %s: %s" % (path, exc)) from exc
    return ALLOW


def post(payload: dict, common: Path, main: Path) -> int:
    path = state_file(common, payload)
    try:
        before = json.loads(path.read_text())
    except FileNotFoundError as exc:
        raise Undetermined("no Pre snapshot %s for this call" % path) from exc
    except (OSError, ValueError) as exc:
        raise Undetermined("Pre snapshot %s unreadable: %s" % (path, exc)) from exc
    if not isinstance(before, dict) or not {"porcelain", "hashes", "head", "merge_head"} <= set(before):
        raise Undetermined("Pre snapshot %s is malformed" % path)
    after = snapshot(main, common)

    verdict = ALLOW
    if before == after:
        pass
    elif after["merge_head"]:
        pass  # a merge / conflict resolution is in progress
    elif after["head"] != before["head"] and not after["porcelain"]:
        if not integration_head(main, after["head"]):
            verdict = REFUSE
    else:
        verdict = REFUSE

    if verdict == REFUSE:
        changed = changed_paths(before, after)
        if after["head"] != before["head"]:
            changed.insert(0, "HEAD %s -> %s (not a merge / fast-forward)" % (before["head"][:12], after["head"][:12]))
        print(
            "Refused: this command modified this project's MAIN working tree "
            "outside integration (CLAUDE.md 8: main only receives merges).\n"
            "Changed in %s:\n  %s\n"
            "Revert it, or redo the work in a worktree and merge. If you did not "
            "touch main, another session may have modified it during this call — "
            "check `git -C %s status` before reverting anything."
            % (main, "\n  ".join(changed), main),
            file=sys.stderr,
        )
    try:
        path.unlink()
    except OSError:
        pass  # a leftover snapshot only costs disk; the verdict above stands
    return verdict


def main() -> int:
    try:
        payload = json.loads(sys.stdin.read())
    except ValueError as exc:
        print("Refused: hook payload is not JSON (%s)" % exc, file=sys.stderr)
        return REFUSE
    if not isinstance(payload, dict):
        print("Refused: hook payload is not a JSON object", file=sys.stderr)
        return REFUSE
    if payload.get("tool_name") != "Bash":
        return ALLOW
    event = payload.get("hook_event_name")
    try:
        common, main_tree = locate(os.environ.get("CLAUDE_PROJECT_DIR", ""))
        if event == "PreToolUse":
            return pre(payload, common, main_tree)
        # A Bash call that exits non-zero (or is interrupted) is followed by
        # PostToolUseFailure, not PostToolUse (observed 2026-10-03 on a real
        # run); a command that wrote into main and then failed is judged too.
        if event in ("PostToolUse", "PostToolUseFailure"):
            return post(payload, common, main_tree)
        raise Undetermined("unexpected hook_event_name %r" % event)
    except Undetermined as exc:
        print(
            "Refused: cannot determine whether this command modified the MAIN "
            "working tree (CLAUDE.md 3: undetermined is not clean): %s" % exc,
            file=sys.stderr,
        )
        return REFUSE


if __name__ == "__main__":
    sys.exit(main())
