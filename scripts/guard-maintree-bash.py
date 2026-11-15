#!/usr/bin/env python3
"""Pre/PostToolUse hook: detect a Bash command that MODIFIED this project's main tree.

CLAUDE.md 8: only integration (merge, conflict resolution) may change the main
(primary) working tree; another session is always assumed to share its index.

This guard OBSERVES the effect instead of predicting it from command syntax.
Its predecessor (2739 lines, replaced 2026-10-03 by user ruling) tried to decide
from the command text whether python / node / perl / awk / shell payloads would
write into main. That is undecidable; every closed gap opened another false
positive or false negative (4a64e6ff and ~30 sibling tickets).

OWN TREE ONLY (user ruling 2026-11-08, verbatim: "donegateそのものが他のworktree
を見たり検証したりテストしたりすることは禁止とする。他のgateでも禁止とする。合理的
理由があって確認するなら、それを提示すること。基本的に認めない" / "おそらく他の
worktreeへの干渉をやめないと並行性があがるほど、処理する時間が膨大になる、そこに
合理的理由はない。定期的にまとまったauditの実施での担保とする。"). The tree a Bash
call runs in is the toplevel of its PRE payload `cwd`. Only when that tree IS
this project's main tree is main snapshotted and compared. When the cwd is a
linked worktree (or in no git tree at all) Pre stores only a small marker under
<git-common-dir>/maintree-guard/ and Post allows WITHOUT reading main, whatever
happened there.

DROPPED, with no compensating mechanism (the periodic audit only runs the gate
tests on main; user, 2026-11-08: "定期auditはmainだけgateについてのテストを
行うだけでいいはず"):
  * a call whose cwd is a linked worktree but which writes into main
    (`cd <main> && ...`, `git -C <main> ...`, a script that chdirs) is not
    observed;
  * the attribution of a foreign `git merge` window in main to another
    process (formerly command_cannot_target_main) — it only ever applied to a
    worktree cwd, which is no longer judged at all;
  * backlog 2dee8b0e's in_flight_merge_window allow (a later merge of this
    command caught in its pre-merge-commit window after an integration HEAD
    move). Its evidence needed a `git merge-tree` of HEAD with every local
    branch, i.e. reading other sessions' branches; without it the evidence is
    incomplete, so that state is now REFUSED (the restrictive side);
  * integration_head's `git for-each-ref --contains` over every ref (user
    ruling 2026-11-08: read no other branch's refs). A HEAD move is now judged
    only from main's own branch reflog and object store (integration_move).
    Newly REFUSED as a result: a move onto a commit that lives on another
    branch without a merge / ff-pull reflog entry (`git reset --hard <branch>`,
    `git checkout -B main <x>`, `git update-ref`), a reset onto any merge
    commit not made on main, a non-fast-forward `git pull` ("pull: Merge
    made by ..."), `git pull --rebase`, and any HEAD move (even a real merge
    commit) when main's reflog is missing or core.logAllRefUpdates=false.

WRITE RECORD. At Post of a main-cwd call, every path whose state changed
between Pre and Post (allowed or refused) is appended to this session's write
record (deny_ledger.record_writes); the Stop gate blocks only on dirt in that
record. A record that cannot be written refuses the call.

  PreToolUse (Bash, cwd main)
                      snapshot main: `git status --porcelain=v2 -z
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
                      set of dirty paths as at Pre, and main's OWN branch
                      reflog explains the move: an unbroken chain of entries
                      from Pre HEAD to the new HEAD, each one either a
                      fast-forward ("merge ...: Fast-forward" or
                      "pull...: Fast-forward") that descends from its old
                      value, or a merge commit made by `git merge` ("merge ...")
                      or by a merge-concluding `git commit`
                      ("commit (merge): ...") whose FIRST parent is its old
                      value (see integration_move; no other branch's refs are
                      read, user ruling 2026-11-08)
                                                            -> 0
                      HEAD moved, `git status` names no path after, every Pre
                      dirty path's Pre worktree content AND index entry equal
                      the new HEAD's (pre_dirt_landed_in_head: another
                      session's merge committing what it wrote before this
                      call's Pre), and integration_move explains the move
                                                            -> 0, nothing recorded
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
                      MERGE_HEAD existed before and is gone after, the new
                      HEAD's parents are exactly [Pre HEAD] + all Pre MERGE_HEAD
                      oids in order, and `git status` names no path after
                      (resolving a conflicted merge, staging and committing it
                      in one call: what the call changed is inside the merge
                      commit; backlog 47592729; see merge_concluded_clean. An
                      unreadable Pre MERGE_HEAD -> not covered)
                                                            -> 0
                      anything else                        -> 2, naming the paths
  Pre / Post (Bash, cwd a linked worktree or no git tree)
                      Pre stores the marker; Post consumes it -> 0. main is
                      not read.

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
(.git/config, .git/hooks) and gitignored files. Changes by ANOTHER session in
main between a main-cwd call's Pre and Post are REFUSED (they cannot be told
apart from this call's own writes) and recorded in THIS session's write record
(over-attribution, the restrictive side); the message says so. One exception
(pre_dirt_landed_in_head): HEAD moved, `git status` names no path at Post,
every path dirty at Pre already held at Pre -- in both working tree and index
-- exactly what the new HEAD holds, and integration_move explains the HEAD
move. That is a Pre taken inside another session's `git merge`
pre-merge-commit window followed by a Post after the merge committed; this
call changed nothing itself, so it is allowed and NOTHING is recorded. Pre
dirt that was discarded, stashed or re-staged by this call, or a tracked Pre
modification it committed, does not match the new HEAD and is still refused.
NOT covered: an UNTRACKED Pre path that this call itself commits, unchanged,
into its own merge commit (`git merge --no-commit` + `git add` + `git commit`)
passes, since an untracked path has no Pre index entry to compare -- the same
class as any content a call adds to its own merge commit. When ORIG_HEAD
was rewritten between Pre and Post with HEAD unchanged and no MERGE_HEAD (what
another session's in-flight `git merge` in main looks like), the refusal
reports that evidence as AMBIGUOUS: this command's own reset/merge/rebase
rewrites ORIG_HEAD too, so the change is not asserted to be this command's (and
is still refused, CLAUDE.md 3). ORIG_HEAD is recorded in the snapshot as
`orig_head`, excluded from the before==after comparison, and changes WORDING
only; a Pre snapshot without the field, or an ORIG_HEAD absent or unreadable at
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

try:
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import deny_ledger  # noqa: E402  (only its per-session write record)
    _LEDGER_IMPORT_ERROR = ""
except Exception as _e:  # noqa: BLE001 — resolves to a refusal when needed
    deny_ledger = None
    _LEDGER_IMPORT_ERROR = "%s: %s" % (type(_e).__name__, _e)

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

    It changes the refusal's WORDING only (see post()); it never allows.
    None means "no evidence", never a verdict.
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


def merge_concluded_clean(main: Path, before: dict, after: dict) -> bool:
    """True if this call concluded the merge in progress at Pre into a commit
    and left NOTHING uncommitted (backlog 47592729: resolve a conflicted merge,
    `git add`, `git commit` in one call). ALL of:
      - MERGE_HEAD was present at Pre and is gone at Post; HEAD moved;
      - the new HEAD's parents are exactly [Pre HEAD] + the Pre MERGE_HEAD
        oids, all of them, in order (so no other commit was made on the way);
      - `git status` at Post names no path at all (ignored files aside).
    Whatever the call staged or edited is then inside that merge commit, i.e.
    it is the merge's resolution (CLAUDE.md 8: conflict resolution is
    integration). Unlike merge_concluded_by_commit this does not need the Pre
    index (which holds unmerged entries during a conflict) and does not allow
    any dirty path at Post. Unreadable Pre oids -> False; a git failure raises
    Undetermined -> the caller refuses (CLAUDE.md 3).
    """
    if not before.get("merge_head") or after["merge_head"]:
        return False
    if after["head"] == before["head"]:
        return False
    oids = before.get("merge_head_oids")
    if not isinstance(oids, list) or not oids or not all(isinstance(o, str) and o for o in oids):
        return False
    if any(rec[0] != "!" for rec, _ in status_entries(after["porcelain"])):
        return False
    parents = git(main, "rev-list", "--parents", "-n1", after["head"]).split()
    if not parents or parents[0] != after["head"]:
        return False
    return parents[1:] == [before["head"]] + oids


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


def commit_parents(main: Path, oid: str) -> list[str]:
    """The parents of `oid` (`git rev-list --parents -n1`), read from main's own
    object store. A git failure or an answer that does not describe `oid` is
    Undetermined (CLAUDE.md 3)."""
    parents = git(main, "rev-list", "--parents", "-n1", oid).split()
    if not parents or parents[0] != oid:
        raise Undetermined("`git rev-list --parents` did not describe %s" % oid)
    return parents[1:]


def is_ancestor(main: Path, old: str, new: str) -> bool:
    """`git merge-base --is-ancestor old new`: exit 0 = yes, 1 = no, anything
    else (or git failing to run) is Undetermined."""
    try:
        proc = subprocess.run(
            ["git", "-C", str(main), "merge-base", "--is-ancestor", old, new],
            capture_output=True,
            text=True,
        )
    except OSError as exc:
        raise Undetermined("git could not be executed: %s" % exc) from exc
    if proc.returncode == 0:
        return True
    if proc.returncode == 1:
        return False
    raise Undetermined(
        "`git merge-base --is-ancestor %s %s` exited %d: %s"
        % (old[:12], new[:12], proc.returncode, proc.stderr.strip() or "(no stderr)")
    )


# Reflog messages that record an integration move of main's branch. Matched as
# a PREFIX of the message field only: a direct commit whose subject imitates a
# merge is logged as "commit: merge ...", which starts with neither.
#   "merge "          -- written by `git merge` (fast-forward or merge commit)
#   "commit (merge): " -- written by `git commit` concluding a merge in progress
#   "pull" ... ": Fast-forward" -- written by a `git pull` that fast-forwarded
MERGE_REFLOG_PREFIX = "merge "
COMMIT_MERGE_REFLOG_PREFIX = "commit (merge): "
PULL_REFLOG_PREFIX = "pull"
FAST_FORWARD_SUFFIX = ": Fast-forward"
# `git merge -m <msg>` that fast-forwards logs this instead (the -m is unused);
# condukt's worktree merge passes -m, so its fast-forwards end this way.
FAST_FORWARD_WITH_M_SUFFIX = ": Fast-forward (no commit created; -m option ignored)"


def integration_step(main: Path, old: str, new: str, message: str) -> bool:
    """True if ONE reflog entry old -> new of main's branch is an integration
    step, judged from main's own object store:
      - "merge ...: Fast-forward" (also with git's "(no commit created; -m
        option ignored)" suffix) or "pull...: Fast-forward": `new` descends from
        `old` (the tip may itself be a merge commit; a fast-forward does not
        create one, so its parents are not constrained);
      - any other "merge ..." entry (a merge commit `git merge` made) and
        "commit (merge): ..." (`git commit` concluding a merge): `new` has >= 2
        parents and its FIRST parent is `old`.
    Every other message (commit, reset, checkout, rebase, `pull --rebase`, a
    non-fast-forward pull, update-ref without -m, ...) is False."""
    fast_forward = (
        message.endswith(FAST_FORWARD_SUFFIX)
        and (message.startswith(MERGE_REFLOG_PREFIX) or message.startswith(PULL_REFLOG_PREFIX))
    ) or (message.startswith(MERGE_REFLOG_PREFIX) and message.endswith(FAST_FORWARD_WITH_M_SUFFIX))
    if fast_forward:
        return is_ancestor(main, old, new)
    if message.startswith(MERGE_REFLOG_PREFIX) or message.startswith(COMMIT_MERGE_REFLOG_PREFIX):
        parents = commit_parents(main, new)
        return len(parents) >= 2 and parents[0] == old
    return False


def reflog_entries(common: Path, branch: str) -> list[tuple[str, str, str]]:
    """(old, new, message) for every entry of main's branch reflog, oldest first.
    Main's own state only. No branch, or a reflog that is missing or unreadable,
    is Undetermined, never evidence (CLAUDE.md 3)."""
    if not branch.startswith("refs/heads/"):
        raise Undetermined("main is not on a branch; no reflog to confirm the HEAD move")
    log = common / "logs" / branch
    try:
        raw = log.read_bytes()
    except OSError as exc:
        raise Undetermined(
            "reflog %s unreadable, cannot confirm the HEAD move is a merge: %s" % (log, exc)
        ) from exc
    out = []
    for line in raw.decode("utf-8", "replace").splitlines():
        head_part, sep, message = line.partition("\t")
        if not sep:
            continue
        fields = head_part.split(" ", 2)
        if len(fields) < 2:
            continue
        out.append((fields[0], fields[1], message))
    return out


def integration_move(main: Path, common: Path, branch: str, old: str, new: str) -> bool:
    """True if main's OWN state shows the HEAD move old -> new is integration.

    User ruling 2026-11-08 (own tree only): the guard reads no other branch's
    refs. It replaced `integration_head`, which ran `git for-each-ref
    --contains` over every ref (other sessions' branches) and allowed any merge
    commit or any HEAD that lived on another ref.

    Evidence, all from main's own branch reflog and object store: the newest
    reflog entry whose new value is `new` starts an unbroken chain of
    consecutive entries back to an entry whose old value is `old`, and EVERY
    entry of that chain is an integration_step (a `git merge` / `git pull`
    fast-forward that descends from the entry's old value, or a merge commit
    made by `git merge` / a merge-concluding `git commit` whose FIRST parent is
    the entry's old value).
    Several merges in one call (sequential `git merge`s) are therefore one
    chain. Any other entry in the chain (commit, reset, rebase, pull --rebase,
    checkout, update-ref, ...), a gap in the chain, or no entry ending at `new` is False
    -> the caller refuses. A missing / unreadable reflog or a git failure is
    Undetermined -> the caller refuses (CLAUDE.md 3).
    """
    entries = reflog_entries(common, branch)
    i = len(entries) - 1
    while i >= 0 and entries[i][1] != new:
        i -= 1
    if i < 0:
        return False
    target = new
    while i >= 0:
        e_old, e_new, message = entries[i]
        if e_new != target or e_old == e_new:
            return False
        if not integration_step(main, e_old, e_new, message):
            return False
        if e_old == old:
            return True
        target = e_old
        i -= 1
    return False


def cwd_is_main(payload: dict, common: Path, main: Path) -> bool:
    """True when the payload cwd is inside this project's MAIN tree — the only
    case in which this guard reads main (user ruling 2026-11-08).

    cwd is a string naming an existing directory, else Undetermined. git
    answering "not a git repository" there -> False (outside any tree). Any
    other git failure -> Undetermined. Otherwise True iff cwd's git dir IS its
    common dir (a primary checkout, not a linked worktree), that common dir is
    this project's, and its toplevel is main. Only rev-parse runs at cwd —
    i.e. in the cwd's own tree."""
    cwd = payload.get("cwd")
    if not isinstance(cwd, str) or not cwd or not os.path.isdir(cwd):
        raise Undetermined("payload cwd %r is not an existing directory" % (cwd,))
    try:
        proc = subprocess.run(
            ["git", "-C", cwd, "rev-parse", "--path-format=absolute",
             "--show-toplevel", "--absolute-git-dir", "--git-common-dir"],
            capture_output=True, text=True, env={**os.environ, "LC_ALL": "C"},
        )
    except OSError as exc:
        raise Undetermined("git could not be executed: %s" % exc) from exc
    if proc.returncode != 0:
        if "not a git repository" in proc.stderr:
            return False
        raise Undetermined(
            "`git rev-parse` at cwd %s exited %d: %s"
            % (cwd, proc.returncode, proc.stderr.strip() or "(no stderr)"))
    lines = proc.stdout.splitlines()
    if len(lines) != 3 or not all(lines):
        raise Undetermined("`git rev-parse` at cwd %s printed %r" % (cwd, proc.stdout))
    top, gitdir, cdir = (os.path.realpath(x) for x in lines)
    return (gitdir == cdir and cdir == os.path.realpath(common)
            and top == os.path.realpath(main))


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


def pre_dirt_landed_in_head(main: Path, before: dict, after: dict) -> bool:
    """True if `git status` names NO path at Post and every path dirty at Pre
    already held, at Pre, exactly what the new HEAD holds -- in the working
    tree AND in the index. Then this call changed none of those paths: at Post
    the worktree and index equal the new HEAD, which equals their Pre state.

    This is what a Pre snapshot taken inside ANOTHER session's `git merge`
    pre-merge-commit window looks like (git has written the merged tree into
    main's index + worktree, HEAD not yet moved), followed by a Post after that
    merge committed. The caller still requires integration_move for the HEAD
    move; this predicate only says the Pre dirt is not a change of this call.

    Per Pre path (status_entries of the Pre porcelain):
      - the Pre working-tree content hash equals the path's hash now (Post is
        clean, so "now" is the new HEAD's content, or "absent");
      - a "1" record (ordinary change): its index mode/oid (mI/hI) equal the
        new HEAD's entry for the path (`git ls-tree -r` of the new HEAD), or
        mI is 000000 and the new HEAD has no such path;
      - a "2" record (rename/copy): the same for its destination; its source
        path must be absent from the new HEAD;
      - a "?" record (untracked): only the content hash check; an untracked
        directory (path ending in "/") is not covered.
    A conflict ("u") record, a submodule record, a Pre path without a hash, an
    unreadable record, or any Post path -> False. Discarding, stashing or
    re-staging pre-existing dirt, or committing a tracked Pre modification (its
    Pre index or worktree state is not what the new HEAD holds) fails this, so
    it is still refused. An untracked Pre path committed unchanged into this
    call's own merge commit is NOT caught (no Pre index entry to compare).
    A git failure raises Undetermined -> the caller refuses (CLAUDE.md 3).
    """
    if status_entries(after["porcelain"]):
        return False
    entries = status_entries(before["porcelain"])
    if not entries:
        return False
    hashes = before["hashes"]
    index_expect: dict[str, tuple[str, str] | None] = {}
    for rec, paths in entries:
        kind = rec[0]
        if kind == "1":
            fields = rec.split(" ", 8)
        elif kind == "2":
            fields = rec.split(" ", 9)
        elif kind == "?":
            if paths[0].endswith("/"):
                return False
            continue
        else:
            return False
        if len(fields) < 9 or fields[2] != "N...":
            return False
        m_index, h_index = fields[4], fields[7]
        index_expect[paths[0]] = None if m_index == "000000" else (m_index, h_index)
        if kind == "2":
            if len(paths) != 2 or not paths[1]:
                return False
            index_expect[paths[1]] = None
    for p in status_paths(before["porcelain"]):
        if p not in hashes:
            return False
        try:
            now = hash_path(main / p)
        except OSError as exc:
            raise Undetermined("cannot hash path %s: %s" % (p, exc)) from exc
        if now != hashes[p]:
            return False
    if not index_expect:
        return True
    out = git(main, "--literal-pathspecs", "ls-tree", "-r", "-z", "--full-tree",
              after["head"], "--", *sorted(index_expect))
    in_head: dict[str, tuple[str, str]] = {}
    for item in out.split("\0"):
        if not item:
            continue
        meta, sep, path = item.partition("\t")
        parts = meta.split(" ")
        if not sep or len(parts) != 3:
            raise Undetermined("unreadable `git ls-tree` entry %r" % item[:80])
        in_head[path] = (parts[0], parts[2])
    for p, want in index_expect.items():
        if in_head.get(p) != want:
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


LINKED_MARKER = {"scope": "not-main"}


def pre(payload: dict, common: Path, main: Path) -> int:
    if cwd_is_main(payload, common, main):
        snap = snapshot(main, common)
        snap["scope"] = "main"
    else:
        # The call runs in a linked worktree (or outside any tree): main is
        # not its tree, so main is not read (user ruling 2026-11-08). Only
        # this marker is stored so that Post can tell "Pre ran" from "Pre
        # never ran" (the latter still refuses).
        snap = dict(LINKED_MARKER)
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
    if before == LINKED_MARKER:
        # Pre found the call running outside main: nothing of main is read.
        try:
            path.unlink()
        except OSError:
            pass
        return ALLOW
    if (not isinstance(before, dict) or before.get("scope") != "main"
            or not {"porcelain", "hashes", "head", "merge_head"} <= set(before)):
        raise Undetermined("Pre snapshot %s is malformed" % path)
    before.pop("scope")
    after = snapshot(main, common)
    # Stripped so it never enters the before==after comparison; it reaches a
    # verdict only as one required piece of the window evidence below.
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
    # True only when the call changed nothing itself: the Pre dirt was already
    # the new HEAD's content and the HEAD move is integration (another
    # session's merge committing what it had written before this call's Pre).
    # Then nothing is recorded in this session's write record either.
    nothing_of_this_call = False
    if before == after:
        pass
    elif after["merge_head"]:
        pass  # a merge / conflict resolution is in progress
    elif merge_abort_back_to_head(main, before, after):
        pass  # aborting one's own in-progress merge back to HEAD
    elif merge_concluded_by_commit(main, before, after):
        pass  # `git commit` concluding a merge staged by an earlier call
    elif merge_concluded_clean(main, before, after):
        pass  # resolving + committing the in-progress merge, tree clean after
    elif after["head"] != before["head"] and only_unchanged_preexisting(before, after):
        branch = checked_out_branch(main)
        if not integration_move(main, common, branch, before["head"], after["head"]):
            verdict = REFUSE
            head_not_integration = True
    elif after["head"] != before["head"] and pre_dirt_landed_in_head(main, before, after):
        branch = checked_out_branch(main)
        if integration_move(main, common, branch, before["head"], after["head"]):
            nothing_of_this_call = True
        else:
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
    # Every path this call changed in main (its own tree) goes into the
    # session's write record, whatever the verdict: the Stop gate blocks only
    # on dirt it can attribute to this session through that record. A record
    # that cannot be written makes the write unattributable -> refuse.
    # A call that changed nothing itself (nothing_of_this_call) records nothing.
    touched = changed_paths(before, after) if before != after and not nothing_of_this_call else []
    if touched:
        if deny_ledger is None:
            err = "scripts/deny_ledger.py could not be loaded (%s)" % _LEDGER_IMPORT_ERROR
        else:
            err = deny_ledger.record_writes(
                payload, str(main), [str(main / p) for p in touched], "guard-maintree-bash.py")
        if err is not None:
            print(
                "Refused: the main-tree paths this call changed could not be recorded "
                "in this session's write record (%s); the Stop gate could not "
                "attribute them (CLAUDE.md 3).\n  %s" % (err, "\n  ".join(touched)),
                file=sys.stderr,
            )
            verdict = REFUSE
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
