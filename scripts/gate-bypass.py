#!/usr/bin/env python3
"""Report commits that were created without passing the local gate.

`git commit --no-verify` cannot be prevented by a pre-commit hook — git skips
the hook entirely. What CAN be done is refuse to let the bypass stay invisible.
`.githooks/post-commit` runs on every commit-creating path (observed, see that
file's header) and appends any commit whose tree pre-commit never certified to a
ledger in the common git dir. This script is the ledger's consumer.

    python3 scripts/gate-bypass.py            # human-readable status
    python3 scripts/gate-bypass.py --json

Exit codes (fail-closed: "cannot determine" resolves to the restricted side)

    0  the ledger is empty — no ungated commit outstanding
    1  at least one ungated commit is outstanding
    2  the verdict could not be determined at all

Exit 2 covers: git missing or erroring, not being inside a repository, and a
ledger file that exists but cannot be read or decoded. None of these may collapse
into 0 — "the ledger could not be read" is not "the ledger is empty", and the
whole point of this file is that an unexamined commit must not read as examined.

The ledger is cleared by a pre-commit run that goes green, not by this script —
and only by a run over a non-empty staged diff, and only for entries whose
commit is an ancestor of the HEAD that run judged (backlog c767cb47).

An entry whose commit is reachable from no ref, no worktree HEAD and no
`--pushing` tip (e.g. the pre-rebase original that survives only in a reflog)
cannot leave this machine, and no pre-commit run can ever clear it. It is
listed as "unreachable" and does not count as outstanding; it is never removed
from the ledger, and it blocks again the moment anything makes it reachable.
An entry that cannot be placed (malformed id, missing or non-commit object, a
failing git call) is undetermined, never unreachable (backlog a7bf3cca).
That ordering is deliberate: clearing must be a side effect of actually
inspecting the content, never an operation a caller can request on its own. A
`--clear` flag here would be a one-command bypass of the bypass detector.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

EXIT_CLEAN = 0
EXIT_OUTSTANDING = 1
EXIT_UNDETERMINED = 2


class Undetermined(Exception):
    """The verdict could not be reached. Never collapses into a clean result."""


def common_git_dir(repo: Path | None) -> Path:
    cmd = ["git", "rev-parse", "--git-common-dir"]
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(repo) if repo else None,
            capture_output=True,
            text=True,
        )
    except OSError as exc:
        raise Undetermined("git could not be executed: %s" % exc) from exc
    if proc.returncode != 0:
        raise Undetermined(
            "`git rev-parse --git-common-dir` exited %d: %s"
            % (proc.returncode, proc.stderr.strip() or "(no stderr)")
        )
    out = proc.stdout.strip()
    if not out:
        raise Undetermined("`git rev-parse --git-common-dir` printed nothing")
    path = Path(out)
    if not path.is_absolute():
        path = (repo or Path.cwd()) / path
    return path


def read_ledger(path: Path) -> list[dict[str, str]]:
    # `Path.exists()` answers False for BOTH "absent" and "cannot be stat'd"
    # (an unreadable parent directory, for instance), so using it here would
    # quietly turn "cannot determine" into "clean" — the exact collapse this
    # file exists to prevent. Found in review of my own code, before any test
    # existed. Stat explicitly so the two answers stay separate.
    try:
        path.stat()
    except FileNotFoundError:
        return []  # genuinely absent, therefore genuinely empty
    except OSError as exc:
        raise Undetermined("ledger %s could not be stat'd: %s" % (path, exc)) from exc

    try:
        raw = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        # An unreadable ledger is NOT an empty ledger.
        raise Undetermined("ledger %s could not be read: %s" % (path, exc)) from exc

    entries = []
    for lineno, line in enumerate(raw.splitlines(), 1):
        line = line.strip()
        if not line:
            continue
        sha, _, subject = line.partition("\t")
        if not sha:
            raise Undetermined("ledger %s line %d has no commit id" % (path, lineno))
        entries.append({"commit": sha, "subject": subject})
    return entries


SHA_CHARS = frozenset("0123456789abcdef")


def _git(repo: Path | None, args: list[str]) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(
            ["git", *args],
            cwd=str(repo) if repo else None,
            capture_output=True,
            text=True,
        )
    except OSError as exc:
        raise Undetermined("git could not be executed: %s" % exc) from exc


def head_tips(repo: Path | None) -> list[str]:
    """The HEAD commit of every worktree, detached or not.

    `for-each-ref` does not list HEAD, but a detached HEAD can still be pushed
    (`git push origin HEAD:main`), so a commit reachable from one is live.
    """
    proc = _git(repo, ["worktree", "list", "--porcelain"])
    if proc.returncode != 0:
        raise Undetermined(
            "`git worktree list` exited %d: %s"
            % (proc.returncode, proc.stderr.strip() or "(no stderr)")
        )
    return [
        line.split(" ", 1)[1]
        for line in proc.stdout.splitlines()
        if line.startswith("HEAD ")
    ]


def is_reachable(repo: Path | None, sha: str, tips: list[str]) -> bool:
    """True if `sha` is contained in some ref, a worktree HEAD, or a pushed tip.

    Raises Undetermined when that cannot be established (malformed sha, missing
    or non-commit object, a git call that fails): an entry we cannot place is
    never treated as unreachable (backlog a7bf3cca).
    """
    if not (4 <= len(sha) <= 64) or not set(sha) <= SHA_CHARS:
        raise Undetermined("ledger entry %r is not a commit id" % sha)
    obj = _git(repo, ["cat-file", "-t", sha])
    if obj.returncode != 0 or obj.stdout.strip() != "commit":
        raise Undetermined(
            "ledger entry %s is not a commit in this repository (%s)"
            % (sha, (obj.stderr.strip() or obj.stdout.strip() or "no output"))
        )
    refs = _git(repo, ["for-each-ref", "--contains", sha, "--format=%(refname)"])
    if refs.returncode != 0:
        raise Undetermined(
            "`git for-each-ref --contains %s` exited %d: %s"
            % (sha, refs.returncode, refs.stderr.strip() or "(no stderr)")
        )
    if refs.stdout.strip():
        return True
    for tip in tips:
        anc = _git(repo, ["merge-base", "--is-ancestor", sha, tip])
        if anc.returncode == 0:
            return True
        if anc.returncode != 1:
            raise Undetermined(
                "`git merge-base --is-ancestor %s %s` exited %d: %s"
                % (sha, tip, anc.returncode, anc.stderr.strip() or "(no stderr)")
            )
    return False


def split_unreachable(
    repo: Path | None, entries: list[dict[str, str]], pushing: list[str]
) -> tuple[list[dict[str, str]], list[dict[str, str]]]:
    """Partition entries into (outstanding, unreachable).

    A commit reachable from no ref, no worktree HEAD and no tip being pushed
    cannot leave this machine, and no pre-commit run can ever clear it (it is
    an ancestor of no HEAD). It is reported, never deleted, and does not block.
    """
    if not entries:
        return [], []
    tips = head_tips(repo) + list(pushing)
    outstanding, unreachable = [], []
    for e in entries:
        (outstanding if is_reachable(repo, e["commit"], tips) else unreachable).append(e)
    return outstanding, unreachable


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", type=Path, default=None)
    ap.add_argument("--json", action="store_true")
    ap.add_argument(
        "--pushing",
        action="append",
        default=[],
        help="a local sha being pushed; an entry it contains is outstanding even if no ref holds it",
    )
    args = ap.parse_args(argv[1:])

    try:
        ledger = common_git_dir(args.repo) / "gate-bypassed"
        entries = read_ledger(ledger)
        entries, unreachable = split_unreachable(args.repo, entries, args.pushing)
    except Undetermined as exc:
        if args.json:
            print(json.dumps({"verdict": "undetermined", "reason": str(exc)}))
        else:
            print("gate-bypass: UNDETERMINED — %s" % exc, file=sys.stderr)
            print(
                "  Blocking rather than reporting clean: an unreadable ledger is\n"
                "  not an empty one.",
                file=sys.stderr,
            )
        return EXIT_UNDETERMINED

    if args.json:
        print(
            json.dumps(
                {
                    "verdict": "outstanding" if entries else "clean",
                    "ledger": str(ledger),
                    "entries": entries,
                    "unreachable": unreachable,
                },
                ensure_ascii=False,
            )
        )
        return EXIT_OUTSTANDING if entries else EXIT_CLEAN

    if unreachable:
        print(
            "gate-bypass: %d ledger entr%s for commits reachable from no ref,\n"
            "worktree HEAD or pushed tip (unreachable; cannot be pushed, kept in the\n"
            "ledger, not blocking):" % (len(unreachable), "y" if len(unreachable) == 1 else "ies"),
            file=sys.stderr,
        )
        for e in unreachable:
            print("  %s  %s" % (e["commit"][:12], e["subject"]), file=sys.stderr)
        print("", file=sys.stderr)

    if not entries:
        print("gate-bypass: no ungated commit outstanding.")
        return EXIT_CLEAN

    print(
        "gate-bypass: %d ungated commit(s) outstanding — content that the local\n"
        "gate never inspected (--no-verify, cherry-pick or rebase):\n" % len(entries),
        file=sys.stderr,
    )
    for e in entries:
        print("  %s  %s" % (e["commit"][:12], e["subject"]), file=sys.stderr)
    print(
        "\nRun the gate over the current tree and let it go green — the next\n"
        "successful pre-commit over a non-empty staged diff clears the entries\n"
        "contained in its HEAD. There is deliberately no\n"
        "--clear flag: clearing is a side effect of inspection, not a request.",
        file=sys.stderr,
    )
    return EXIT_OUTSTANDING


if __name__ == "__main__":
    sys.exit(main(sys.argv))
