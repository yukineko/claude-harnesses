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
                      MERGE_HEAD existed before and is gone after, HEAD
                      unchanged, and every changed path is one the merge
                      brought in (`git diff HEAD...<MERGE_HEAD oid>`):
                      `git merge --abort` of an in-progress merge
                                                            -> 0
                      (an unreadable MERGE_HEAD oid at Pre -> not covered)
                      HEAD moved, tree clean, and the new HEAD is a merge
                      commit, or is contained in a ref other than the branch
                      main has checked out, or that branch's reflog has an
                      entry for exactly this old -> new move whose message
                      starts with "merge " (written by `git merge`; covers a
                      fast-forward whose source branch is already deleted)
                                                            -> 0
                      anything else                         -> 2, naming the paths

Exit 2 at Post cannot undo the command — the write has happened. It stops the
turn from proceeding as if main were untouched and names what changed, so the
change is reverted or moved into a worktree openly.

Cannot determine -> 2, never 0 (CLAUDE.md 3): a malformed payload, git failing,
CLAUDE_PROJECT_DIR not in a repository, a snapshot that cannot be written, read
or decoded, a Pre snapshot that is missing at Post, a dirty path that cannot be
hashed, a failing merge/containment check, a reflog that is missing or
unreadable when it is needed as integration evidence, or any uncaught exception.

An uncaught exception (e.g. RecursionError decoding a deeply nested payload,
AttributeError on a snapshot with wrong field types) exits 2 (blocked), never 1
(which Claude Code treats as allow). This mirrors deny-no-verify.py and guard-maintree-edit.py: a check
that crashed has not passed (CLAUDE.md 3), so it resolves to the restricted side.

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
    merge_head = (common / "MERGE_HEAD").exists()
    return {
        "porcelain": porcelain,
        "hashes": hashes,
        "head": git(main, "rev-parse", "HEAD").strip(),
        "merge_head": merge_head,
        "merge_head_oid": merge_head_oid(common) if merge_head else None,
    }


def merge_head_oid(common: Path) -> str | None:
    """The first oid MERGE_HEAD names, or None when it cannot be read as one.

    None is not evidence of anything: post() then has no merge to attribute an
    abort to and refuses (CLAUDE.md 3).
    """
    try:
        first = (common / "MERGE_HEAD").read_text().split()
    except (OSError, UnicodeDecodeError):
        return None
    if not first or len(first[0]) not in (40, 64) or not all(c in "0123456789abcdef" for c in first[0]):
        return None
    return first[0]


def merge_abort_back_to_head(main: Path, before: dict, after: dict) -> bool:
    """True if this call ended an in-progress merge back to the same HEAD and
    every path it changed is one the merge itself brought in (backlog 6b5971e4).

    `git merge --abort` of one's own conflicted merge is integration (CLAUDE.md
    8): it restores main to HEAD. The paths are bounded by
    `git diff --name-only HEAD...<MERGE_HEAD>` (what the incoming side changed
    since the merge base), so a call that also touched an unrelated path is not
    covered. No recorded MERGE_HEAD oid -> False (refuse), never a guess.
    """
    if not before.get("merge_head") or after["merge_head"]:
        return False
    if after["head"] != before["head"]:
        return False
    oid = before.get("merge_head_oid")
    if not isinstance(oid, str) or not oid:
        return False
    merged = set(
        git(main, "diff", "--name-only", "-z", "%s...%s" % (before["head"], oid)).split("\0")
    ) - {""}
    return all(p in merged for p in changed_paths(before, after))


def state_file(common: Path, payload: dict) -> Path:
    sid = payload.get("session_id")
    tid = payload.get("tool_use_id")
    if not isinstance(sid, str) or not sid or not isinstance(tid, str) or not tid:
        raise Undetermined("payload has no session_id / tool_use_id to key the snapshot")
    key = hashlib.sha256(("%s\0%s" % (sid, tid)).encode()).hexdigest()[:32]
    return common / "maintree-guard" / (key + ".json")


def checked_out_branch(main: Path) -> str:
    """Full ref name of the branch main has checked out, or "" when detached."""
    try:
        proc = subprocess.run(
            ["git", "-C", str(main), "symbolic-ref", "-q", "HEAD"],
            capture_output=True,
            text=True,
        )
    except OSError as exc:
        raise Undetermined("git could not be executed: %s" % exc) from exc
    if proc.returncode not in (0, 1):
        raise Undetermined("`git symbolic-ref -q HEAD` exited %d" % proc.returncode)
    return proc.stdout.strip()


def integration_head(main: Path, head: str, branch: str) -> bool:
    """True if `head` is a merge commit or lives on a ref other than main's branch."""
    parents = git(main, "rev-list", "--parents", "-n1", head).split()
    if not parents or parents[0] != head:
        raise Undetermined("`git rev-list --parents` did not describe %s" % head)
    if len(parents) >= 3:
        return True
    refs = git(main, "for-each-ref", "--contains", head, "--format=%(refname)").split()
    return any(r != branch for r in refs)


# The reflog message `git merge` writes when it moves a branch. It is matched
# as a PREFIX of the message field only: a direct commit whose subject imitates
# a merge is logged as "commit: merge ...", which does not start with this.
MERGE_REFLOG_PREFIX = "merge "


def merge_reflog_entry(common: Path, branch: str, old: str, new: str) -> bool:
    """True if `branch`'s reflog records the EXACT transition old -> new as written
    by `git merge` (message starting with "merge ", e.g. "merge x: Fast-forward").

    This is the integration evidence for a fast-forward whose source branch is
    already gone (user ruling 2026-10-16, backlog 86e21645). An entry for any
    other transition (e.g. a ff followed by a direct commit in the same call)
    does not count. No branch, or a reflog that is missing or unreadable, is
    undetermined, never evidence (CLAUDE.md 3).
    """
    if not branch.startswith("refs/heads/"):
        raise Undetermined("main is not on a branch; no reflog to confirm the HEAD move")
    log = common / "logs" / branch
    try:
        raw = log.read_bytes()
    except OSError as exc:
        raise Undetermined(
            "reflog %s unreadable, cannot confirm HEAD %s -> %s is a merge: %s"
            % (log, old[:12], new[:12], exc)
        ) from exc
    for line in raw.decode("utf-8", "replace").splitlines():
        head_part, sep, message = line.partition("\t")
        if not sep:
            continue
        fields = head_part.split(" ", 2)
        if len(fields) < 2:
            continue
        if fields[0] == old and fields[1] == new and message.startswith(MERGE_REFLOG_PREFIX):
            return True
    return False


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
    elif merge_abort_back_to_head(main, before, after):
        pass  # aborting one's own in-progress merge back to HEAD
    elif after["head"] != before["head"] and not after["porcelain"]:
        branch = checked_out_branch(main)
        if not integration_head(main, after["head"], branch) and not merge_reflog_entry(
            common, branch, before["head"], after["head"]
        ):
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
    try:
        _rc = main()
    except Exception as _crash:  # noqa: BLE001
        # An uncaught exception (e.g. RecursionError decoding a deeply nested
        # payload, or AttributeError when a snapshot field has an unexpected type)
        # exits 1, which Claude Code treats as a non-blocking error, i.e. the call
        # proceeds. A check that crashed has not passed (CLAUDE.md 3), so it
        # resolves to exit 2 (issue 1235; mirrors deny-no-verify.py and
        # guard-maintree-edit.py).
        sys.stderr.write(
            f"Refused: guard-maintree-bash.py crashed ({type(_crash).__name__}: {_crash}); "
            "a check that could not run has not passed.\n")
        _rc = 2
    sys.exit(_rc)
