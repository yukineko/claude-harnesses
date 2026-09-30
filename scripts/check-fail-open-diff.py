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


def evaluate(repo: Path) -> tuple[int, list[Rise]]:
    """(0, []) when no staged file's hit count rose, else (1, rises).
    Raises Undetermined when the comparison cannot be made."""
    rises: list[Rise] = []
    for rel in staged_changes(repo):
        before_text = _blob(repo, f"HEAD:{rel}")
        after_text = _blob(repo, f":{rel}")
        if after_text is None:
            raise Undetermined(f"{rel} is listed as staged but has no index blob")
        before = len(_hits(rel, before_text)) if before_text is not None else 0
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
        print(f"{r.path}: fail-open hits {r.before} -> {r.after} in this commit",
              file=sys.stderr)
        for lineno, text, name in r.hits:
            print(f"  {r.path}:{lineno}: [{name}] {text.strip()}", file=sys.stderr)
    print(
        "\nfail-open-diff: this commit ADDS a fail-open swallow (the listed "
        "file's count rose versus HEAD; which of its hits is the new one is "
        "for you to read — all current hits are listed). Fix it by failing "
        "closed (propagate the error / name the undetermined state), or, if it "
        "is a reviewed exception, add it to check-fail-open.py's ALLOWLIST "
        "with a reason.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
