#!/usr/bin/env python3
"""Diff-scoped fail-open ratchet: block a commit that ADDS a fail-open swallow.

`check-fail-open.py --ratchet` compares ONE whole-workspace count against a
pinned baseline, and no hook ever ran it (backlog 2133b6fe): the count had
drifted from the pin (63 pinned, 43 live at 74aad11f) without anyone noticing.
It also cannot simply be wired into pre-commit. Under CLAUDE.md §8 other
sessions are always committing in parallel, so a whole-workspace number moves
under a commit that did not cause the move, and that commit would be blocked
for someone else's change.

This gate scopes the same detector to the commit itself (option (b), chosen by
the user). The rule, exactly:

  1. Parent set P. Without MERGE_HEAD, P = {HEAD}. While a merge is in
     progress (MERGE_HEAD exists in the git dir found by `git rev-parse
     --git-dir`, so linked worktrees work), P is the output of
     `git merge-base --independent HEAD <every MERGE_HEAD line>`: the
     parents that are not ancestors of another parent, deduplicated.
  2. Candidate paths = the union over every p in P of
     `git diff --cached --name-only -z --diff-filter=ACMRT p`, kept only when
     in scope (deletions cannot add a hit). T (type change) is included: a
     gitlink or symlink replaced by a regular file is listed as T, not M, and
     without it that file's hits were never read (observed 2026-10-03).
  3. For each candidate, before = the MAXIMUM hit count over the blobs of that
     path in every p in P (a p lacking the path, or holding a directory
     there, counts 0); after = the hit count of the staged (index) blob. Both
     sides are read out of git, so the content judged is exactly the content
     being committed, not the working tree.
  4. Block when after > before for any candidate.

So a pre-existing swallow in an untouched file, one carried unchanged through
an edit, or one that arrives unchanged from the other side of a merge
(measured 2026-10-03: crates/jev/src/client.rs existed only on MERGE_HEAD's
side and was blamed on the merge) does not fire.

Why the INDEPENDENT heads (observed with git 2.50.1 in a scratch repo,
2026-10-03, by writing MERGE_HEAD/MERGE_MODE by hand and running `git
commit`): without MERGE_MODE=`no-ff`, `git commit` records exactly the
`merge-base --independent` output, in that order. A MERGE_HEAD naming an
ancestor of HEAD therefore yields a single-parent commit on HEAD, and one
naming a descendant D yields a single-parent commit on D — so crediting the
ancestor would launder a swallow HEAD had removed, and diffing only against
HEAD would miss a file that equals HEAD but re-adds a swallow D removed.
Limit: with MERGE_MODE exactly `no-ff` (what `git merge --no-ff` writes),
`git commit` records HEAD plus every MERGE_HEAD line verbatim, ancestors and
duplicates included. The gate does NOT follow that: it still judges against
the independent heads only. That set is a subset of what is recorded, so
the gate can only be stricter than a judgment over the recorded set: the max
is taken over fewer parents, and a path dropped from the candidates (equal to
its blob in every independent head) has after = some head's count <= before,
so it could not have fired anyway. A hit present only in a reachable ancestor
is history the mainline already removed, not something "from the other side".

A MERGE_HEAD that cannot be read, is empty, or names something that does not
resolve to a commit, and any git failure while computing P or the
candidates, is UNDETERMINED (exit 2), never "not a merge".

Object types. Each side is checked with `git cat-file -t` before it is read.
A blob is scanned. A tree in a parent counts as absent (0) — see `_blob` for
why that can only make the gate stricter. Anything else (a gitlink/submodule
entry at an in-scope path, whose `git show` would print a commit and its
patch, or an object git cannot describe) is UNDETERMINED rather than
scanned or silently counted.

What it cannot see (a counting limit, for merges and single-parent commits
alike): the rule compares per-file COUNTS, not identities. A commit that
removes one swallow and adds a different one in the same file nets out and
does not fire; in a merge, likewise, dropping one side's hit while adding a
new one in that file nets out against the max. Hits in a path that equals
its blob in every parent of P are never read.

Scope and patterns are check-fail-open.py's `--all` surface: `crates/*/src/**`
`.rs` and `scripts/*.sh`, with every pattern (blocking and advisory classes
alike, like `--ratchet`) and the same ALLOWLIST.

Exit codes: 0 no file's count rose / 1 a staged file adds a swallow /
2 cannot determine (no HEAD, git failure, unreadable MERGE_HEAD, a non-blob
non-tree object at a candidate path). Exit 2 blocks in
the pre-commit runner: a commit we could not compare is not a clean commit.

Usage:
  python3 scripts/check-fail-open-diff.py [--repo <path>]
"""

from __future__ import annotations

import fnmatch
import importlib.util
import subprocess
import sys
from pathlib import Path
from typing import NamedTuple

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_fail_open", _HERE / "check-fail-open.py"
)
fo = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(fo)

# check-fail-open.py's `--all` surface, as repo-relative globs.
SCOPE_GLOBS = ("crates/*/src/*.rs", "crates/*/src/**/*.rs", "scripts/*.sh")


class Undetermined(Exception):
    """The comparison could not be made. Maps to exit 2 (block)."""


class Rise(NamedTuple):
    path: str
    before: int
    after: int
    hits: list


def _git(repo: Path, *args: str) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(
            ["git", "-C", str(repo), *args], capture_output=True, text=True
        )
    except OSError as e:
        raise Undetermined(f"cannot run git: {e}") from e


def in_scope(rel: str) -> bool:
    if rel.startswith("scripts/") and rel.count("/") != 1:
        return False
    return any(fnmatch.fnmatchcase(rel, g) for g in SCOPE_GLOBS)


def staged_changes(repo: Path, parents: list[str]) -> list[str]:
    """In-scope paths the commit adds or modifies relative to ANY of
    `parents` (the union of `git diff --cached ... <p>` over p), in first-seen
    order. Deletions cannot add a hit, so --diff-filter=ACMRT (T: a gitlink
    or symlink that became a regular file is a type change, not an M; without
    T its content is never judged). Any git failure
    is Undetermined."""
    seen: dict[str, None] = {}
    for p in parents:
        diff = _git(repo, "diff", "--cached", "--name-only", "-z",
                    "--diff-filter=ACMRT", p)
        if diff.returncode != 0:
            raise Undetermined(
                f"git diff --cached {p} failed (exit {diff.returncode}): "
                f"{diff.stderr.strip()}"
            )
        for path in diff.stdout.split("\0"):
            if path and in_scope(path):
                seen.setdefault(path, None)
    return list(seen)


def _blob(repo: Path, treeish: str, path: str) -> str | None:
    """Text of `path` in `treeish` (a commit id, or "" for the index).

    None when the path is absent on that side, or when a parent holds a TREE
    there. A tree counts as absent (0 hits) because 0 is the minimum count:
    it can only lower `before`, so it can only make the gate stricter, never
    let a rise through. It also loses no text — a directory's files are
    separate paths, each listed and judged under its own name when in scope.

    The object type is checked with `git cat-file -t` before reading; a
    gitlink (submodule commit — `git show` would print the commit and its
    patch, which would then be scanned as if it were source), or any other
    non-blob, or an object git cannot describe, is Undetermined. Reading uses
    `git cat-file blob`, so no textconv/filters apply. Any other failure is
    Undetermined, not absent."""
    spec = f"{treeish}:{path}"
    if treeish:
        ls = _git(repo, "ls-tree", "--full-tree", "--name-only", "-z",
                  treeish, "--", path)
        if ls.returncode != 0:
            raise Undetermined(f"git ls-tree {treeish} {path} failed: "
                               f"{ls.stderr.strip()}")
        if path not in [p for p in ls.stdout.split("\0") if p]:
            return None
    kind = _git(repo, "cat-file", "-t", spec)
    if kind.returncode != 0:
        raise Undetermined(
            f"git cat-file -t {spec} failed (exit {kind.returncode}): "
            f"{kind.stderr.strip()} — a gitlink's commit is normally not in "
            f"this repo's object store; an object that cannot be typed is "
            f"not a blob we can count")
    otype = kind.stdout.strip()
    if otype == "tree" and treeish:
        return None
    if otype != "blob":
        raise Undetermined(f"{spec} is a {otype or 'untyped object'}, not a "
                           f"blob — its hits cannot be counted")
    try:
        out = subprocess.run(["git", "-C", str(repo), "cat-file", "blob",
                              spec], capture_output=True)
    except OSError as e:
        raise Undetermined(f"cannot run git: {e}") from e
    if out.returncode != 0:
        raise Undetermined(f"git cat-file blob {spec} failed: "
                           f"{out.stderr.decode('utf-8', 'replace').strip()}")
    return out.stdout.decode("utf-8", errors="replace")


def _hits(rel: str, text: str) -> list:
    lines = text.splitlines()
    if rel.endswith(".rs"):
        raw = fo.scan_rust(lines)
    elif rel.endswith(".sh"):
        raw = fo.scan_shell(lines)
    else:
        raw = []
    return [h for h in raw if not fo._allowlisted(rel, h[2], h[1])]


def merge_parents(repo: Path) -> list[str]:
    """Commit ids in MERGE_HEAD when a merge is in progress, else [].

    "In progress" means MERGE_HEAD exists in the git dir (`git rev-parse
    --git-dir`, which resolves linked worktrees too). One line per merged
    head (several for an octopus merge). A MERGE_HEAD that exists but cannot
    be read, is empty, or holds a line that does not resolve to a commit is
    Undetermined — never "treat as not a merge", because that would fall back
    to the HEAD-only comparison the merge is known to mis-attribute.

    These are the heads NAMED, not the parents recorded; see
    `recorded_parents`."""
    gd = _git(repo, "rev-parse", "--git-dir")
    if gd.returncode != 0:
        raise Undetermined(f"git rev-parse --git-dir failed (exit "
                           f"{gd.returncode}): {gd.stderr.strip()}")
    git_dir = Path(gd.stdout.strip())
    if not git_dir.is_absolute():
        git_dir = repo / git_dir
    merge_head = git_dir / "MERGE_HEAD"
    try:
        text = merge_head.read_text(encoding="utf-8")
    except FileNotFoundError:
        return []
    except (OSError, UnicodeDecodeError) as e:
        raise Undetermined(f"a merge is in progress but {merge_head} cannot "
                           f"be read: {e}") from e
    lines = [ln.strip() for ln in text.splitlines() if ln.strip()]
    if not lines:
        raise Undetermined(f"{merge_head} exists but names no commit")
    parents = []
    for ln in lines:
        rv = _git(repo, "rev-parse", "--verify", "-q", f"{ln}^{{commit}}")
        sha = rv.stdout.strip()
        if rv.returncode != 0 or not sha:
            raise Undetermined(f"MERGE_HEAD line {ln!r} does not resolve to "
                               f"a commit")
        parents.append(sha)
    return parents


def recorded_parents(repo: Path) -> list[str]:
    """The parent set P the gate judges against (module docstring, rule 1).

    Without MERGE_HEAD: [HEAD's commit id]. During a merge: the output of
    `git merge-base --independent HEAD <MERGE_HEAD ids...>` — what `git
    commit` records unless MERGE_MODE is `no-ff`; under `no-ff` git records
    a superset, and judging the independent subset is the stricter choice
    (module docstring). An unresolvable HEAD, a failing merge-base, or an
    empty / unparsable result is Undetermined."""
    head = _git(repo, "rev-parse", "--verify", "-q", "HEAD^{commit}")
    head_sha = head.stdout.strip()
    if head.returncode != 0 or not head_sha:
        raise Undetermined(
            "HEAD does not resolve (unborn branch or broken repo) — there is "
            "no committed side to compare the staged content against"
        )
    named = merge_parents(repo)
    if not named:
        return [head_sha]
    mb = _git(repo, "merge-base", "--independent", head_sha, *named)
    if mb.returncode != 0:
        raise Undetermined(
            f"git merge-base --independent failed (exit {mb.returncode}): "
            f"{mb.stderr.strip()} — cannot tell which parents the merge "
            f"commit will record"
        )
    out: list[str] = []
    for ln in mb.stdout.splitlines():
        sha = ln.strip()
        if not sha:
            continue
        if len(sha) < 40 or any(c not in "0123456789abcdef" for c in sha):
            raise Undetermined(f"git merge-base --independent printed "
                               f"{sha!r}, not a commit id")
        if sha not in out:
            out.append(sha)
    if not out:
        raise Undetermined("git merge-base --independent printed no commit "
                           "— a merge with no parent cannot be judged")
    return out


def evaluate(repo: Path) -> tuple[int, list[Rise]]:
    """(0, []) when no staged file's hit count rose above the MAX of its
    counts over the recorded parent set P (see `recorded_parents`), else
    (1, rises). Raises Undetermined when the comparison cannot be made."""
    rises: list[Rise] = []
    parents = recorded_parents(repo)
    changed = staged_changes(repo, parents)
    for rel in changed:
        after_text = _blob(repo, "", rel)
        if after_text is None:
            raise Undetermined(f"{rel} is listed as staged but has no index blob")
        before = 0
        for parent in parents:
            text = _blob(repo, parent, rel)
            if text is not None:
                before = max(before, len(_hits(rel, text)))
        after_hits = _hits(rel, after_text)
        if len(after_hits) > before:
            rises.append(Rise(rel, before, len(after_hits), after_hits))
    return (1 if rises else 0), rises


def main(argv: list[str]) -> int:
    args = argv[1:]
    repo = fo.REPO
    if "--repo" in args:
        i = args.index("--repo")
        if i + 1 >= len(args):
            print("fail-open-diff: --repo needs a path", file=sys.stderr)
            return 2
        repo = Path(args[i + 1])
    try:
        code, rises = evaluate(repo)
    except Undetermined as e:
        print(f"fail-open-diff: UNDETERMINED — {e}. A commit that could not be "
              f"compared is not a clean commit; blocking (exit 2).",
              file=sys.stderr)
        return 2
    if code == 0:
        print("fail-open-diff: no staged file adds a fail-open swallow.")
        return 0
    for r in rises:
        print(f"{r.path}: fail-open hits {r.before} -> {r.after} in this commit "
              f"(before = max over the judged parents)",
              file=sys.stderr)
        for lineno, text, name in r.hits:
            print(f"  {r.path}:{lineno}: [{name}] {text.strip()}", file=sys.stderr)
    print(
        "\nfail-open-diff: this commit ADDS a fail-open swallow: the listed "
        "file's staged count exceeds its count in every judged parent. The "
        "judged parents are HEAD, or during a merge the independent heads "
        "`git merge-base --independent HEAD <MERGE_HEAD...>` (what git "
        "records; under MERGE_MODE=no-ff git also records non-independent "
        "heads, which are deliberately not credited). Counts are per file, "
        "not per hit, so which hit is the new one is for you to read — all "
        "current hits are listed. Fix it by failing "
        "closed (propagate the error / name the undetermined state), or, if it "
        "is a reviewed exception, add it to check-fail-open.py's ALLOWLIST "
        "with a reason.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
