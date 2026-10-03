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
the user). For every in-scope file the commit changes, it counts the hits in
the HEAD blob and in the staged (index) blob — both read out of git, so the
content judged is exactly the content being committed, not the working tree —
and blocks if any file's count ROSE. A pre-existing swallow in an untouched
file, or one carried unchanged through an edit, does not fire.

Merge commits. While a merge is in progress (MERGE_HEAD exists in the git dir
found by `git rev-parse --git-dir`, so linked worktrees work), the commit has
several parents: HEAD plus every commit named in MERGE_HEAD (one per line;
several for an octopus merge). "Before" for a path is then the MAXIMUM hit
count over all parents' blobs of that path (a parent lacking the path counts
0), and the gate blocks only when the staged blob holds MORE hits than that.
So a file or hit that arrives unchanged from the other side is not
attributed to the merge (measured 2026-10-03: crates/jev/src/client.rs
existed only on MERGE_HEAD's side and was blamed on the merge). A MERGE_HEAD
that cannot be read, is empty, or names something that does not resolve to
a commit is UNDETERMINED (exit 2), never "not a merge".

What it cannot see (a counting limit, for merges and single-parent commits
alike): the rule compares per-file COUNTS, not identities. A commit that
removes one swallow and adds a different one in the same file nets out and
does not fire; in a merge, likewise, dropping one side's hit while adding a
new one in that file nets out against the max. Hits in a file the commit
does not stage are never read.

Scope and patterns are check-fail-open.py's `--all` surface: `crates/*/src/**`
`.rs` and `scripts/*.sh`, with every pattern (blocking and advisory classes
alike, like `--ratchet`) and the same ALLOWLIST.

Exit codes: 0 no file's count rose / 1 a staged file adds a swallow /
2 cannot determine (no HEAD, git failure, unreadable blob). Exit 2 blocks in
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


def staged_changes(repo: Path) -> list[str]:
    """In-scope paths the commit adds or modifies (deletions cannot add a hit)."""
    head = _git(repo, "rev-parse", "--verify", "-q", "HEAD")
    if head.returncode != 0:
        raise Undetermined(
            "HEAD does not resolve (unborn branch or broken repo) — there is "
            "no committed side to compare the staged content against"
        )
    diff = _git(repo, "diff", "--cached", "--name-only", "-z",
                "--diff-filter=ACMR", "HEAD")
    if diff.returncode != 0:
        raise Undetermined(
            f"git diff --cached failed (exit {diff.returncode}): "
            f"{diff.stderr.strip()}"
        )
    return [p for p in diff.stdout.split("\0") if p and in_scope(p)]


def _blob(repo: Path, spec: str) -> str | None:
    """Text of `spec` (`HEAD:path` or `:path`), None when the path is absent
    on that side. Any other failure is Undetermined, not absent."""
    path = spec.split(":", 1)[1]
    treeish = spec.split(":", 1)[0]
    if treeish:
        ls = _git(repo, "ls-tree", "--name-only", "-z", treeish, "--", path)
        if ls.returncode != 0:
            raise Undetermined(f"git ls-tree {treeish} {path} failed: "
                               f"{ls.stderr.strip()}")
        if not [p for p in ls.stdout.split("\0") if p]:
            return None
    try:
        out = subprocess.run(["git", "-C", str(repo), "show", spec],
                             capture_output=True)
    except OSError as e:
        raise Undetermined(f"cannot run git: {e}") from e
    if out.returncode != 0:
        raise Undetermined(f"git show {spec} failed: "
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
    to the HEAD-only comparison the merge is known to mis-attribute."""
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


def evaluate(repo: Path) -> tuple[int, list[Rise]]:
    """(0, []) when no staged file's hit count rose above EVERY parent's
    count for that path, else (1, rises). Parents are HEAD plus, during a
    merge, every MERGE_HEAD commit. Raises Undetermined when the comparison
    cannot be made."""
    rises: list[Rise] = []
    changed = staged_changes(repo)
    parents = ["HEAD", *merge_parents(repo)]
    for rel in changed:
        after_text = _blob(repo, f":{rel}")
        if after_text is None:
            raise Undetermined(f"{rel} is listed as staged but has no index blob")
        before = 0
        for parent in parents:
            text = _blob(repo, f"{parent}:{rel}")
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
              f"(before = max over parents)",
              file=sys.stderr)
        for lineno, text, name in r.hits:
            print(f"  {r.path}:{lineno}: [{name}] {text.strip()}", file=sys.stderr)
    print(
        "\nfail-open-diff: this commit ADDS a fail-open swallow (the listed "
        "file's count rose above its count in HEAD — and, for a merge, above "
        "its count in every parent, so the rise is the merge's own; which of "
        "its hits is the new one is "
        "for you to read — all current hits are listed). Fix it by failing "
        "closed (propagate the error / name the undetermined state), or, if it "
        "is a reviewed exception, add it to check-fail-open.py's ALLOWLIST "
        "with a reason.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
