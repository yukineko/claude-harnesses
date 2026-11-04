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
                      untracked path, HEAD, and whether MERGE_HEAD exists;
                      while it exists, also the oids it names and the staged
                      index entries (`git diff-index --cached HEAD`). Stored under <git-common-dir>/maintree-guard/ keyed by
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
                      HEAD moved, tree clean apart from paths (untracked or
                      tracked) that were already dirty at Pre with the same
                      status record and content hash, and exactly the same
                      set of dirty paths as at Pre, and the new HEAD is a merge
                      commit, or is contained in a ref other than the branch
                      main has checked out, or that branch's reflog has an
                      entry for exactly this old -> new move whose message
                      starts with "merge " (written by `git merge`; covers a
                      fast-forward whose source branch is already deleted)
                                                            -> 0
                      HEAD moved by integration (same evidence as above)
                      and the dirty tree is a LATER merge of this command
                      caught in its pre-merge-commit window: ORIG_HEAD
                      rewritten to the current HEAD, no SQUASH_MSG,
                      index == AUTO_MERGE tree == working tree, and that tree
                      is the clean `git merge-tree` of HEAD with a local
                      branch not yet in HEAD (backlog 2dee8b0e; see
                      in_flight_merge_window)               -> 0
                      MERGE_HEAD existed before and is gone after, and the new
                      HEAD's parents are exactly [Pre HEAD] + all Pre MERGE_HEAD
                      oids in order, and the new HEAD's tree is exactly the
                      Pre index (every path the commit changes vs Pre HEAD was
                      staged at Pre with that same mode and blob, and every
                      path staged at Pre is in the commit), and the paths
                      dirty after are exactly the unstaged / untracked paths
                      dirty before, with the same content hash (a `git commit`
                      concluding a merge staged by an earlier call; backlog
                      c181dd41; see merge_concluded_by_commit. An unreadable
                      Pre MERGE_HEAD or Pre staged index -> not covered)
                                                            -> 0
                      anything else                        -> 2, naming the paths

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
(.git/config, .git/hooks) and gitignored files. After an integration HEAD
move, a command that stages exactly the clean merge of a local branch without
committing it by non-merge means and with ORIG_HEAD reset to HEAD (observed:
`git reset -q HEAD` then `git cherry-pick -n` of a one-commit branch) is
on-disk identical to a later merge's pre-merge-commit window and is allowed
(in_flight_merge_window). Changes by ANOTHER session
between this call's Pre and Post are still REFUSED (they cannot be told apart
from this call's own writes) and the message says so. When the Post lands in
another session's `git merge` window (merged tree already in main's index and
working tree, HEAD not moved, no MERGE_HEAD yet because git is running
pre-merge-commit) and ORIG_HEAD was rewritten between Pre and Post with HEAD
unchanged (observed: git writes ORIG_HEAD at merge start, MERGE_HEAD only after
pre-merge-commit), the refusal reports that evidence as AMBIGUOUS: a concurrent
merge in main is a possible cause, but this command's own reset/merge/rebase
rewrites ORIG_HEAD too, so the change is not asserted to be this command's (and
is still refused). Evidence = ORIG_HEAD differs between Pre and Post (including
absent OR unreadable at Pre, both stored as None, and readable at Post), HEAD unchanged, no MERGE_HEAD. With HEAD unchanged that evidence changes WORDING only:
it is recorded in the snapshot as `orig_head` but is excluded from the
before==after comparison and from every verdict branch except one: when HEAD
DID move by integration, an ORIG_HEAD rewritten to the current HEAD is one of
several required (never sufficient alone) pieces of in_flight_merge_window
evidence; a Pre snapshot without
the field (key missing, not merely None), or an ORIG_HEAD that is absent or unreadable at
Post, means no evidence (existing wording).
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


def status_entries(porcelain: str) -> list[tuple[str, list[str]]]:
    """(record, paths it names) for each `git status --porcelain=v2 -z` record.

    A rename/copy record ("2 ...") is followed by a SEPARATE NUL field holding
    the source path; it is consumed here as part of that record, never parsed
    as a record of its own (GH #1520). An unrecognized record is Undetermined.
    """
    fields = porcelain.split("\0")
    entries = []
    i = 0
    while i < len(fields):
        rec = fields[i]
        i += 1
        if not rec:
            continue
        kind = rec[0]
        if kind == "1":
            paths = [rec.split(" ", 8)[8]]
        elif kind == "2":
            paths = [rec.split(" ", 9)[9]]
            if i < len(fields):
                paths.append(fields[i])  # the rename/copy source
                i += 1
        elif kind == "u":
            paths = [rec.split(" ", 10)[10]]
        elif kind in "?!":
            paths = [rec[2:]]
        else:
            raise Undetermined("unrecognized git status record %r" % rec[:40])
        entries.append((rec, paths))
    return entries


def status_paths(porcelain: str) -> list[str]:
    """Every path named by `git status --porcelain=v2 -z` output."""
    return [p for _, paths in status_entries(porcelain) for p in paths]


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
        "orig_head": orig_head_identity(common),
        "porcelain": porcelain,
        "hashes": hashes,
        "head": git(main, "rev-parse", "HEAD").strip(),
        "merge_head": merge_head,
        "merge_head_oid": merge_head_oid(common) if merge_head else None,
        "merge_head_oids": merge_head_oids(common) if merge_head else None,
        "merge_staged": staged_index_entries(main) if merge_head else None,
    }


def orig_head_identity(common: Path) -> list | None:
    """[content, mtime_ns, inode] of ORIG_HEAD, or None when absent/unreadable.

    Wording evidence only (see post()): None means "no evidence", never a verdict.
    git rewrites ORIG_HEAD via a lockfile rename, so the inode/mtime change even
    when the oid is the same as before.
    """
    try:
        path = common / "ORIG_HEAD"
        st = path.stat()
        return [path.read_text().strip(), st.st_mtime_ns, st.st_ino]
    except (OSError, UnicodeDecodeError):
        return None


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


def merge_head_oids(common: Path) -> list[str] | None:
    """EVERY oid MERGE_HEAD names, in order, or None when any of it cannot be read
    as an oid (an octopus merge names several). None is never evidence."""
    try:
        words = (common / "MERGE_HEAD").read_text().split()
    except (OSError, UnicodeDecodeError):
        return None
    if not words:
        return None
    for w in words:
        if len(w) not in (40, 64) or not all(c in "0123456789abcdef" for c in w):
            return None
    return words


ZERO_OIDS = ("0" * 40, "0" * 64)


def raw_diff_entries(out: str) -> dict[str, list[str]] | None:
    """path -> [dst mode, dst oid] from `git diff-index` / `git diff-tree` raw
    `-z --no-renames` output (":srcmode dstmode srcoid dstoid STATUS\\0path\\0").

    None when the output cannot be read exactly that way: an unmerged entry
    ("U", conflicts not resolved), a rename/copy record, a malformed header, a
    path named twice, or a dst oid that is not a full hex oid. None is never
    evidence (CLAUDE.md 3).
    """
    fields = out.split("\0")
    if fields and fields[-1] == "":
        fields.pop()
    if len(fields) % 2:
        return None
    entries: dict[str, list[str]] = {}
    for i in range(0, len(fields), 2):
        head, path = fields[i], fields[i + 1]
        parts = head.split(" ")
        if len(parts) != 5 or not head.startswith(":") or not path:
            return None
        dst_mode, dst_oid, status = parts[1], parts[3], parts[4]
        if status not in ("A", "D", "M", "T"):
            return None
        if len(dst_oid) not in (40, 64) or not all(c in "0123456789abcdef" for c in dst_oid):
            return None
        if path in entries:
            return None
        entries[path] = [dst_mode, dst_oid]
    return entries


def staged_index_entries(main: Path) -> dict[str, list[str]] | None:
    """Every path whose INDEX entry differs from HEAD, as path -> [index mode,
    index oid] (a staged deletion is [000000, zero oid]), from the read-only
    `git diff-index --cached -r -z --no-renames HEAD`. Recorded at Pre only
    while MERGE_HEAD exists. None when git fails or the output is not readable
    (see raw_diff_entries); post() then has no staged merge set to compare with
    and refuses (CLAUDE.md 3).
    """
    try:
        out = git(main, "diff-index", "--cached", "-r", "-z", "--no-renames", "HEAD")
    except Undetermined:
        return None
    return raw_diff_entries(out)


def worktree_dirty_paths(porcelain: str) -> set[str] | None:
    """Paths whose WORKING TREE differs from the index at Pre: the primary path
    of a "1"/"2" record whose Y (worktree) column is not ".", and every
    untracked ("?") path. None when the status holds a conflict ("u") record
    or a record whose XY cannot be read (CLAUDE.md 3)."""
    out = set()
    for rec, paths in status_entries(porcelain):
        kind = rec[0]
        if kind in "12":
            if len(rec) < 4 or rec[1] != " ":
                return None
            if rec[3] != ".":
                out.add(paths[0])
        elif kind == "?":
            out.add(paths[0])
        elif kind == "!":
            continue
        else:
            return None
    return out


def merge_concluded_by_commit(main: Path, before: dict, after: dict) -> bool:
    """True if this call only concluded an in-progress merge with `git commit`
    (backlog c181dd41), i.e. it committed EXACTLY the index that was staged
    at Pre and changed nothing else. ALL of:
      - MERGE_HEAD was present at Pre and is gone at Post; HEAD moved;
      - the new HEAD's parents are exactly [Pre HEAD] + the Pre MERGE_HEAD
        oids, all of them, in order (`git rev-list --parents -n1`);
      - the new HEAD's tree is the Pre INDEX tree: `git diff-tree -r -z
        --no-renames <Pre HEAD> <new HEAD>` names exactly the paths whose
        index entry differed from HEAD at Pre (staged_index_entries), each
        with the same mode and blob oid as the Pre index entry. A path staged
        or re-staged with other content in this call, or an untracked /
        unstaged-only path committed in this call, fails this;
      - the Pre status had no conflict record, and the paths dirty at Post
        are exactly the Pre worktree-dirty paths (worktree_dirty_paths:
        unstaged edits and untracked files), each with the same content hash;
        one whose index was NOT staged at Pre also keeps a byte-identical
        status record, and one that was ALSO staged at Pre now has a "1"
        record with "." in the X (index) column. A Pre worktree-dirty path
        deleted, edited or staged in this call, or a path newly dirty, fails
        this; a staged-only Pre path must be clean at Post.
    Anything missing or unreadable (Pre oids, Pre staged entries, git failure,
    parse failure) is False -> the caller refuses (CLAUDE.md 3).
    """
    if not before.get("merge_head") or after["merge_head"]:
        return False
    oids = before.get("merge_head_oids")
    if not isinstance(oids, list) or not oids or not all(isinstance(o, str) and o for o in oids):
        return False
    staged = before.get("merge_staged")
    if not isinstance(staged, dict):
        return False
    if after["head"] == before["head"]:
        return False
    parents = git(main, "rev-list", "--parents", "-n1", after["head"]).split()
    if not parents or parents[0] != after["head"]:
        return False
    if parents[1:] != [before["head"]] + oids:
        return False
    committed = raw_diff_entries(
        git(main, "diff-tree", "-r", "-z", "--no-renames", before["head"], after["head"])
    )
    if committed is None or committed != staged:
        return False
    dirty_pre = worktree_dirty_paths(before["porcelain"])
    if dirty_pre is None:
        return False
    ra, rb = status_records(before["porcelain"]), status_records(after["porcelain"])
    if set(rb) != dirty_pre:
        return False
    a, b = before["hashes"], after["hashes"]
    for p in dirty_pre:
        if p not in a or p not in b or a[p] != b[p]:
            return False
        rec = rb[p]
        if p in staged:
            if not rec.startswith("1 .") or rec[3:4] == ".":
                return False
        elif ra.get(p) != rec:
            return False
    return True


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


def git_equal(cwd: Path, *args: str) -> bool:
    """Run a `--quiet` comparison: exit 0 -> True, exit 1 -> False, else Undetermined."""
    try:
        proc = subprocess.run(
            ["git", "-C", str(cwd), *args], capture_output=True, text=True
        )
    except OSError as exc:
        raise Undetermined("git could not be executed: %s" % exc) from exc
    if proc.returncode not in (0, 1):
        raise Undetermined(
            "`git %s` exited %d: %s"
            % (" ".join(args), proc.returncode, proc.stderr.strip() or "(no stderr)")
        )
    return proc.returncode == 0


def _hex_oid(text: str) -> str | None:
    first = text.split()
    if len(first) != 1 or len(first[0]) not in (40, 64) or not all(c in "0123456789abcdef" for c in first[0]):
        return None
    return first[0]


def in_flight_merge_window(
    main: Path, common: Path, before: dict, after: dict, has_orig: bool, before_orig, after_orig
) -> bool:
    """True if the Post state is a LATER `git merge` of this same command caught
    inside its pre-merge-commit window (backlog 2dee8b0e): the merged tree is
    already in main's index and working tree but neither MERGE_HEAD nor the merge
    commit exists yet (observed: git writes AUTO_MERGE and ORIG_HEAD before
    pre-merge-commit, MERGE_HEAD and the commit after it).

    Every piece of evidence is required; any missing one is False (-> refuse):
      - ORIG_HEAD rewritten between Pre and Post and naming the current HEAD
        (`git merge` sets ORIG_HEAD = HEAD when it starts),
      - no SQUASH_MSG (an uncommitted `git merge --squash` leaves the same
        index/AUTO_MERGE state but never becomes a merge commit),
      - AUTO_MERGE names a tree, the index equals it and the working tree equals
        the index; no unmerged path; untracked paths only the pre-existing,
        unchanged ones,
      - that tree is exactly the clean `git merge-tree --write-tree HEAD <b>` of
        some local branch <b> other than main's branch and not yet in HEAD.
    The caller still requires the HEAD move itself to be integration.
    Known residual: git's on-disk state in this window is the same as a command
    that stages exactly a clean merge of a branch without committing it by other
    means (e.g. `git reset -q HEAD` then `git cherry-pick -n` of a one-commit
    branch); such content is what merging that branch would bring in.
    """
    if not has_orig or after_orig is None or before_orig == after_orig:
        return False
    if not isinstance(after_orig, list) or not after_orig or after_orig[0] != after["head"]:
        return False
    if (common / "SQUASH_MSG").exists():
        return False
    try:
        auto = _hex_oid((common / "AUTO_MERGE").read_text())
    except (OSError, UnicodeDecodeError):
        return False
    if auto is None:
        return False
    ra, rb = status_records(before["porcelain"]), status_records(after["porcelain"])
    for p, rec in rb.items():
        if rec.startswith("u "):
            return False
        if rec.startswith("? "):
            if ra.get(p) != rec or before["hashes"].get(p) is None or before["hashes"].get(p) != after["hashes"].get(p):
                return False
    if not git_equal(main, "diff-index", "--cached", "--quiet", auto, "--"):
        return False
    if not git_equal(main, "diff", "--quiet", "--"):
        return False
    branch = checked_out_branch(main)
    candidates = git(
        main, "for-each-ref", "--no-merged", after["head"], "--format=%(refname)", "refs/heads/"
    ).split()
    for ref in candidates:
        if ref == branch:
            continue
        try:
            proc = subprocess.run(
                ["git", "-C", str(main), "merge-tree", "--write-tree", after["head"], ref],
                capture_output=True,
                text=True,
            )
        except OSError as exc:
            raise Undetermined("git could not be executed: %s" % exc) from exc
        if proc.returncode == 1:
            continue  # conflicted merge: not a clean merge result
        if proc.returncode != 0:
            raise Undetermined(
                "`git merge-tree --write-tree %s %s` exited %d" % (after["head"][:12], ref, proc.returncode)
            )
        lines = proc.stdout.splitlines()
        if lines and lines[0].strip() == auto:
            return True
    return False


def changed_paths(before: dict, after: dict) -> list[str]:
    """Paths whose content hash or status record (index state) differs."""
    a, b = before["hashes"], after["hashes"]
    ra, rb = status_records(before["porcelain"]), status_records(after["porcelain"])
    paths = set(a) | set(b) | set(ra) | set(rb)
    return sorted(p for p in paths if a.get(p) != b.get(p) or ra.get(p) != rb.get(p))


def only_unchanged_preexisting(before: dict, after: dict) -> bool:
    """True if `git status` names exactly the same paths at Pre and Post, each
    with a byte-identical status record and the same content hash (backlog
    58bc638d for untracked paths, fbc2d42e / GH 1512 for tracked ones).

    Such a path -- a stray untracked `bare.git/` at the root, or a tracked file
    another session had already modified before this call -- was not changed
    by this call, so it must not hide an integration HEAD move. A tracked
    path's status record carries its HEAD/index/worktree modes and its HEAD
    and index object ids, so an identical record means the call changed
    neither that path's HEAD blob nor its index entry; the content hash covers
    the working-tree bytes.

    Anything else means the tree is not "unchanged apart from pre-existing
    dirty paths" and the caller refuses: a path dirty at Post but not at Pre,
    a path dirty at Pre but not at Post (a pre-existing modification that was
    reverted or committed is a change too), a record or hash that differs, a
    hash missing on either side, or a conflict record. An empty porcelain on
    both sides is trivially True (the clean-tree case).
    """
    ra, rb = status_records(before["porcelain"]), status_records(after["porcelain"])
    if set(ra) != set(rb):
        return False
    a, b = before["hashes"], after["hashes"]
    for p, rec in rb.items():
        if rec.startswith("u "):
            return False
        if ra[p] != rec:
            return False
        if p not in a or p not in b:
            return False
        if a[p] != b[p]:
            return False
    return True


def status_records(porcelain: str) -> dict[str, str]:
    """path -> its status record line, for naming index-only changes."""
    out = {}
    for rec, paths in status_entries(porcelain):
        for p in paths:  # a rename's source maps to the rename record too
            if p:
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
    # Wording evidence only: stripped so it never enters the verdict logic.
    # A MISSING key (old-format Pre snapshot) is no evidence; a present None
    # (ORIG_HEAD absent at Pre) is a real value.
    has_orig_field = "orig_head" in before
    before_orig = before.pop("orig_head", None)
    after_orig = after.pop("orig_head", None)
    concurrent_merge = (
        has_orig_field
        and after_orig is not None
        and before_orig != after_orig
        and after["head"] == before["head"]
        and not after["merge_head"]
    )

    verdict = ALLOW
    # Whether the HEAD move itself was checked for integration evidence and
    # found to have none. Only then may the refusal say "not a merge"; when the
    # tree check failed first, the HEAD move was never classified (GH 1512).
    head_not_integration = False
    if before == after:
        pass
    elif after["merge_head"]:
        pass  # a merge / conflict resolution is in progress
    elif merge_abort_back_to_head(main, before, after):
        pass  # aborting one's own in-progress merge back to HEAD
    elif merge_concluded_by_commit(main, before, after):
        pass  # `git commit` concluding a merge staged by an earlier call
    elif after["head"] != before["head"] and (
        only_unchanged_preexisting(before, after)
        or in_flight_merge_window(main, common, before, after, has_orig_field, before_orig, after_orig)
    ):
        branch = checked_out_branch(main)
        if not integration_head(main, after["head"], branch) and not merge_reflog_entry(
            common, branch, before["head"], after["head"]
        ):
            verdict = REFUSE
            head_not_integration = True
    else:
        verdict = REFUSE

    if verdict == REFUSE:
        changed = changed_paths(before, after)
        if after["head"] != before["head"]:
            if head_not_integration:
                why = "not a merge / fast-forward"
            else:
                why = (
                    "not classified as merge or fast-forward: the working tree / "
                    "index also changed during this call (paths below), so the HEAD "
                    "move was refused without checking it"
                )
            changed.insert(0, "HEAD %s -> %s (%s)" % (before["head"][:12], after["head"][:12], why))
        if concurrent_merge:
            msg = (
                "Refused: ORIG_HEAD was rewritten during this call while HEAD did "
                "not move and there is no MERGE_HEAD. That is what another "
                "session's in-flight `git merge` in this project's MAIN working "
                "tree looks like (a concurrent merge in main is one possible "
                "cause), but this command's own reset / merge / "
                "rebase produces it too. The change below is therefore NOT "
                "asserted to be this command's; it is still refused because a "
                "verdict that cannot be determined resolves to refuse "
                "(CLAUDE.md 3).\n"
                "Changed in %s:\n  %s\n"
                "Check `git -C %s status` / `git log` after the other session's "
                "merge would have finished; if this command did write into main, "
                "revert it or move the work into a worktree."
            )
        else:
            msg = (
                "Refused: this command modified this project's MAIN working tree "
                "outside integration (CLAUDE.md 8: main only receives merges).\n"
                "Changed in %s:\n  %s\n"
                "Revert it, or redo the work in a worktree and merge. If you did not "
                "touch main, another session may have modified it during this call — "
                "check `git -C %s status` before reverting anything."
            )
        print(msg % (main, "\n  ".join(changed), main), file=sys.stderr)
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
