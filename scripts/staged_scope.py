"""Shared `--staged` scoping for the pre-commit scanners (user ruling 2026-11-08).

pre-commit checks only the change's own range: the staged diff. Each scanner
keeps its full sweep as the default (no flag), and `.githooks/pre-commit`
passes `--staged`. In that mode a scanner either
  * judges only the staged files in its target set (per-file scanners), or
  * runs its whole-repo check only when the staged diff touches one of its
    inputs (scanners that judge a global property: a count against a
    baseline, or consistency across files).

Fail-closed: when the staged set cannot be read, the scanner exits 2
(undetermined) and blocks. It never falls back to "nothing staged".
"""

from __future__ import annotations

import fnmatch
import subprocess
import sys
from pathlib import Path

FLAG = "--staged"
EXIT_UNDETERMINED = 2


class StagedSetUnavailable(Exception):
    pass


def pop_flag(argv: list[str]) -> tuple[bool, list[str]]:
    """(was `--staged` given, argv without it)."""
    rest = [a for a in argv if a != FLAG]
    return len(rest) != len(argv), rest


def staged_paths(repo: Path) -> set[str]:
    """Repo-relative paths in `git diff --cached --name-only --no-renames`
    (index vs HEAD; on an unborn HEAD, every indexed path). Raises
    StagedSetUnavailable on any git failure or undecodable output."""
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), "diff", "--cached", "--name-only", "-z", "--no-renames"],
            capture_output=True,
        )
    except OSError as e:
        raise StagedSetUnavailable(f"git could not be executed: {e}") from e
    if proc.returncode != 0:
        raise StagedSetUnavailable(
            f"`git diff --cached` exited {proc.returncode}: "
            f"{proc.stderr.decode('utf-8', 'replace').strip() or '(no stderr)'}"
        )
    try:
        out = proc.stdout.decode("utf-8")
    except UnicodeDecodeError as e:
        raise StagedSetUnavailable(f"staged path list is not UTF-8: {e}") from e
    return {p for p in out.split("\0") if p}


def staged_paths_or_exit(repo: Path, label: str) -> set[str]:
    try:
        return staged_paths(repo)
    except StagedSetUnavailable as e:
        print(
            f"{label}: UNDETERMINED — the staged set could not be read ({e}); "
            "blocking rather than treating it as empty.",
            file=sys.stderr,
        )
        sys.exit(EXIT_UNDETERMINED)


def rel(path: Path, repo: Path) -> str:
    try:
        return path.resolve().relative_to(repo.resolve()).as_posix()
    except ValueError:
        return path.as_posix()


def touches(staged: set[str], patterns: list[str]) -> bool:
    """True when some staged path matches a pattern: an exact path, a `dir/`
    prefix, or an fnmatch glob (`*` also matches `/`)."""
    for p in staged:
        for pat in patterns:
            if pat.endswith("/"):
                if p.startswith(pat):
                    return True
            elif p == pat or fnmatch.fnmatchcase(p, pat):
                return True
    return False


def gate_on_inputs(repo, label: str, argv: list[str], patterns: list[str]) -> list[str]:
    """For a whole-repo scanner: strip `--staged` from argv and, when it was
    given and no staged path matches `patterns` (see `touches`), print the
    skip line and exit 0. An unreadable staged set exits 2. Returns argv
    without the flag; without the flag nothing else happens."""
    given, rest = pop_flag(argv)
    if given:
        staged = staged_paths_or_exit(Path(repo), label)
        if not touches(staged, patterns):
            print_skipped(label)
            sys.exit(0)
    return rest


def print_nothing_in_scope(label: str) -> None:
    print(f"{label}: nothing staged is in scope — no file to judge.")


def print_skipped(label: str) -> None:
    print(f"{label}: skipped — the staged diff touches none of its inputs")
