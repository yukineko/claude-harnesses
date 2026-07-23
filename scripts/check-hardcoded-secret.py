#!/usr/bin/env python3
"""Block a hardcoded secret from landing in a commit's ADDED lines.

Gap this closes (backlog aa74be67): `.githooks/pre-commit`'s six python3 gates
(injectguard / fail-open-guard / doc-claims / test-weakening / version-lockstep
/ bump-on-change — see that file's header) have no check equivalent to
`precommit-audit`'s `check_hardcoded_secret`
(`crates/precommit-audit/src/checks/mod.rs:175-199`). A contributor who has not
installed precommit-audit at user scope gets NO git-commit-time secret scan on
this repo at all (recorded as a known gap in
`crates/precommit-audit/README.ja.md`'s "記録したギャップ" paragraph). This
script is that missing gate, written in the same stdlib-only python3 style as
this repo's other `.githooks/pre-commit` scanners rather than by wiring
precommit-audit itself in — the 2026-07-23 self-apply decision in that same
README explicitly chose NOT to wholesale-integrate precommit-audit here
(reconciling two independently-evolved policy sets was judged not worth the
cost absent an observed failure); adding one more independent, narrowly-scoped
script is consistent with that decision, not a reversal of it.

Detection mirrors precommit-audit's `check_hardcoded_secret` almost verbatim
(same key names, same shape requirement, same allowlist-by-substring idea) so
the two independent implementations agree on what counts as a hit:
`(password|passwd|secret|api[_-]?key|token|private[_-]?key) = "<4+ non-trivial
chars>"`. Scope is ADDED lines only (`git diff <base>`, base=HEAD by default —
the same "working tree vs last commit" convention check-version-bumped.py uses
for pre-commit), not the whole tracked tree: re-scanning every already-committed
line on every future commit would force allowlisting the entire pre-existing
history before this gate could ship at all. Only NEW secret-shaped lines block.

Scope carve-out (false-positive discipline, load-bearing): `.py` files are
NEVER scanned by the real CLI path. This repo's own test suite for check-*.py
gates embeds literal attack/violation-shaped fixture strings directly in
`scripts/test_check_*.py` (see e.g. `test_check_fail_open.py`'s literal
`read_dir` swallow fixtures) — `check-fail-open.py` made the identical choice
for the identical reason (its `iter_target_files` globs `*.sh` and crate `.rs`,
never `*.py`). Without this carve-out, committing this gate's own test fixtures
would trip the gate on its own introduction.

Fail-closed contract (repo doctrine): a git-diff failure or an unresolvable
base ref is UNDETERMINED, not clean — exit 2, distinct from exit 1 (a genuine
finding), mirroring check-test-weakening.py's Undetermined -> exit 2 channel.

Usage:
  python3 scripts/check-hardcoded-secret.py             # base = HEAD
  python3 scripts/check-hardcoded-secret.py --base <ref> # explicit base
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Mirrors crates/precommit-audit/src/checks/mod.rs:176-177's `secret_re`
# verbatim (same key alternation, same "non-trivial quoted RHS >= 4 chars"
# shape) so the two independent scanners agree on what a secret-shaped line is.
SECRET_RE = re.compile(
    r'''(?i)(password|passwd|secret|api[_-]?key|token|private[_-]?key)\s*=\s*["'][^"'`${}\s][^"']{4,}["']'''
)

# A '+' diff line whose first non-blank character (after the leading '+' is
# stripped by the caller) is a comment marker. Mirrors precommit-audit's
# `comment_re` (`^\+\s*(#|//)`), applied here to the already-stripped text.
COMMENT_RE = re.compile(r"^\s*(#|//)")

# File extensions never scanned by the real CLI path. See the module docstring
# ("Scope carve-out") for why `.py` is excluded — this is not a loophole, it is
# what keeps this gate's own test fixtures from tripping it.
EXCLUDED_SUFFIXES = {".py"}

# Genuine, reviewed exceptions: a hit is suppressed when one of these
# substrings appears in the offending line. Empty until a real one is filed —
# unlike check-fail-open.py's ALLOWLIST, there is no known exception yet.
ALLOWLIST: list[str] = []

# git's fixed empty-tree object, used as the diff base on an unborn branch
# (the very first commit in a repo) so "no HEAD yet" reads as "everything in
# this commit is added," never as an error.
_EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"


class Undetermined(Exception):
    """git plumbing failed or the base ref would not resolve. Exit 2, never a pass."""


def scan_line(line: str) -> bool:
    """True if `line` (an added line's text, no leading '+') looks like a
    hardcoded secret assignment and is not allowlisted or a comment."""
    if COMMENT_RE.match(line):
        return False
    if not SECRET_RE.search(line):
        return False
    if any(a in line for a in ALLOWLIST):
        return False
    return True


_FILE_RE = re.compile(r"^\+\+\+ b/(.*)$")
_HUNK_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@")


def parse_diff_added(diff_text: str) -> list[tuple[str, int, str]]:
    """Pure parser: `git diff --unified=0` text -> [(path, new_lineno, text)].

    Unit-tested directly against synthetic diff text (no git/filesystem
    needed), so the parser's line-number tracking is pinned without ever
    committing a real secret-shaped string anywhere. With `-U0` a hunk holds
    only '+'/'-' lines (no context), so tracking is: a '+' line is recorded at
    the running new-lineno and advances it; a '-' line does not exist in the
    new file and does not advance it.
    """
    out: list[tuple[str, int, str]] = []
    path: str | None = None
    lineno: int | None = None
    for raw in diff_text.splitlines():
        m = _FILE_RE.match(raw)
        if m:
            path = m.group(1)
            lineno = None
            continue
        m = _HUNK_RE.match(raw)
        if m:
            lineno = int(m.group(1))
            continue
        if raw.startswith("+++") or raw.startswith("---"):
            continue
        if raw.startswith("+"):
            if path is not None and lineno is not None:
                out.append((path, lineno, raw[1:]))
                lineno += 1
        # '-' lines and anything else (diff --git, index, etc.) do not
        # advance the new-file line counter.
    return out


def _git(repo: Path, *args: str) -> str:
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), *args], capture_output=True, text=True,
        )
    except OSError as e:
        raise Undetermined(f"could not run git: {e}") from e
    if proc.returncode != 0:
        raise Undetermined(
            f"git {' '.join(args)} exited {proc.returncode}: {proc.stderr.strip()}"
        )
    return proc.stdout


def resolve_base(repo: Path, base: str) -> str:
    """`base` if it resolves; the empty-tree object if `base` is HEAD on an
    unborn branch (no commits yet, so "added lines" is the whole tree)."""
    probe = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "--verify", "--quiet", f"{base}^{{commit}}"],
        capture_output=True, text=True,
    )
    if probe.returncode == 0:
        return base
    if base == "HEAD":
        return _EMPTY_TREE
    raise Undetermined(f"base ref does not resolve: {base}")


def iter_target_lines(repo: Path, base: str) -> list[tuple[str, int, str]]:
    resolved = resolve_base(repo, base)
    diff_text = _git(repo, "diff", "--unified=0", "--no-color", resolved)
    added = parse_diff_added(diff_text)
    return [(p, n, t) for (p, n, t) in added if Path(p).suffix not in EXCLUDED_SUFFIXES]


def main(argv: list[str]) -> int:
    args = argv[1:]
    base = "HEAD"
    if "--base" in args:
        i = args.index("--base")
        if i + 1 >= len(args):
            print("hardcoded-secret-guard: --base requires a value", file=sys.stderr)
            return 2
        base = args[i + 1]

    try:
        lines = iter_target_lines(REPO, base)
    except Undetermined as e:
        print(
            f"hardcoded-secret-guard: cannot determine ({e}) — failing closed "
            f"(exit 2). A diff we cannot compute is NOT a clean diff.",
            file=sys.stderr,
        )
        return 2

    hits = [(p, n, t) for (p, n, t) in lines if scan_line(t)]
    for p, n, t in hits:
        print(f"{p}:{n}: [possible-hardcoded-secret] {t.strip()}")
    if hits:
        print(
            f"\nhardcoded-secret-guard: {len(hits)} possible hardcoded secret(s) "
            f"in added lines. Remove the literal value (use an env var / secret "
            f"store reference instead), or, if this is a reviewed exception, add "
            f"a distinguishing substring to ALLOWLIST with a reason.",
            file=sys.stderr,
        )
        return 1
    print("hardcoded-secret-guard: clean (no hardcoded secret in added lines).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
