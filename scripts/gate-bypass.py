#!/usr/bin/env python3
"""Report commits that were created without passing the local gate.

`git commit --no-verify` cannot be prevented by a pre-commit hook — git skips
the hook entirely. What CAN be done is refuse to let the bypass stay invisible.
`.githooks/post-commit` runs on every commit-creating path (observed, see that
file's header) and appends any commit whose tree pre-commit never certified to a
ledger in the common git dir. This script is the ledger's consumer.

    python3 scripts/gate-bypass.py            # human-readable status
    python3 scripts/gate-bypass.py --json
    python3 scripts/gate-bypass.py --push LOCAL REMOTE [--push ...]   # pre-push

With `--push` (one per pre-push stdin line), only entries whose commit THIS
push carries are outstanding: reachable from a pushed LOCAL tip and not from
its REMOTE sha (an all-zero REMOTE, a new remote ref, excludes nothing). Other
present entries are listed as "not in this push" and do not block it (user
ruling 2026-10-22 「ゲートを直す」: a841ad3c, ungated and living only on another
session's branch, blocked `git push origin main`). A REMOTE object unknown to
this repository, or any failing ancestry check, is undetermined (exit 2).
Without `--push` the status form below applies.

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

In the status form, an entry whose commit is reachable from no ref and no
worktree HEAD (e.g. the pre-rebase original that survives only in a reflog)
cannot leave this machine, and no pre-commit run can ever clear it. It is
listed as "unreachable" and does not count as outstanding; it is never removed
from the ledger, and it blocks again the moment anything makes it reachable.
An entry that cannot be placed (malformed or abbreviated id, a non-commit
object, a failing git call) is undetermined, never unreachable (backlog
a7bf3cca).

One exception to that a7bf3cca rule, by user ruling 2026-10-20 (「ゲートを直す」,
backlog 860c43db): an entry whose id is a well-formed FULL-LENGTH object id
and whose object `git cat-file --batch-check` definitively reports as
"missing" is listed as unreachable too. A commit with no content in this
repository cannot leave this machine, so for "can it be pushed" it is the same
as an unreachable commit (observed: b77286119dc3, pruned after the same fix
re-landed gated as 33a6c46b, blocked every push forever). It is never removed
from the ledger either. "Missing" must be the answer git gave, not the absence
of an answer: if the lookup itself fails, or the id is abbreviated (a prefix
lookup cannot prove absence), the entry stays undetermined.
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


def _git(
    repo: Path | None, args: list[str], stdin: str | None = None
) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(
            ["git", *args],
            cwd=str(repo) if repo else None,
            input=stdin,
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


HASH_HEX_LEN = {"sha1": 40, "sha256": 64}


def object_id_length(repo: Path | None) -> int:
    """Hex length of a full object id in this repository; Undetermined if unknown."""
    proc = _git(repo, ["rev-parse", "--show-object-format"])
    fmt = proc.stdout.strip()
    if proc.returncode != 0 or fmt not in HASH_HEX_LEN:
        raise Undetermined(
            "`git rev-parse --show-object-format` exited %d with %r: %s"
            % (proc.returncode, fmt, proc.stderr.strip() or "(no stderr)")
        )
    return HASH_HEX_LEN[fmt]


def object_is_missing(repo: Path | None, sha: str, full_len: int) -> bool:
    """True if `sha` names a commit that is definitively ABSENT; False if it
    names a commit that is present.

    Raises Undetermined for everything else: a malformed or abbreviated id, a
    non-commit object, a lookup that fails or answers unexpectedly. Only a
    full-length id can be proven missing (a prefix lookup cannot), and only the
    literal "missing" answer from a cat-file that exited 0 counts as absence.
    """
    if not (4 <= len(sha) <= 64) or not set(sha) <= SHA_CHARS:
        raise Undetermined("ledger entry %r is not a commit id" % sha)
    obj = _git(repo, ["cat-file", "--batch-check=%(objecttype)"], stdin=sha + "\n")
    answer = obj.stdout.strip()
    if obj.returncode == 0 and answer == "commit":
        return False
    if obj.returncode == 0 and answer == sha + " missing" and len(sha) == full_len:
        return True
    raise Undetermined(
        "ledger entry %s is not a commit in this repository (%s)"
        % (sha, (obj.stderr.strip() or answer or "no output"))
    )


def is_reachable(repo: Path | None, sha: str, tips: list[str]) -> bool:
    """True if the PRESENT commit `sha` is contained in some ref or one of
    `tips` (the worktree HEADs; call `object_is_missing` first).

    Raises Undetermined when that cannot be established (a git call that
    fails): an entry we cannot place is never treated as unreachable (backlog
    a7bf3cca).
    """
    refs = _git(repo, ["for-each-ref", "--contains", sha, "--format=%(refname)"])
    if refs.returncode != 0:
        raise Undetermined(
            "`git for-each-ref --contains %s` exited %d: %s"
            % (sha, refs.returncode, refs.stderr.strip() or "(no stderr)")
        )
    if refs.stdout.strip():
        return True
    return any(_is_ancestor(repo, sha, tip) for tip in tips)


ZERO_OID_CHARS = frozenset("0")


def _is_ancestor(repo: Path | None, sha: str, tip: str) -> bool:
    """`git merge-base --is-ancestor`: exit 0 is yes, 1 is no, anything else
    (an unknown object, a failing git) is Undetermined."""
    anc = _git(repo, ["merge-base", "--is-ancestor", sha, tip])
    if anc.returncode == 0:
        return True
    if anc.returncode == 1:
        return False
    raise Undetermined(
        "`git merge-base --is-ancestor %s %s` exited %d: %s"
        % (sha, tip, anc.returncode, anc.stderr.strip() or "(no stderr)")
    )


def in_pushed_range(repo: Path | None, sha: str, local: str, remote: str) -> bool:
    """True if the push of `local` over `remote` carries commit `sha`: `sha` is
    reachable from `local` and not from `remote`.

    An all-zero `remote` (the remote ref does not exist yet) excludes nothing,
    so every ancestor of `local` counts as carried (the restrictive reading).
    A `remote` object this repository does not have, or any failing git call,
    is Undetermined — never "not carried".
    """
    if not _is_ancestor(repo, sha, local):
        return False
    if not remote or set(remote) <= ZERO_OID_CHARS:
        return True
    return not _is_ancestor(repo, sha, remote)


def split_for_push(
    repo: Path | None, entries: list[dict[str, str]], pushes: list[list[str]]
) -> tuple[list[dict[str, str]], list[dict[str, str]], list[dict[str, str]]]:
    """Partition entries into (outstanding, unreachable, not_in_push) for a
    push of the given (local, remote) pairs.

    Only an entry whose commit this push carries is outstanding (user ruling
    2026-10-22: an ungated commit on another branch, not in the pushed range,
    blocked `git push origin main`). A definitively missing object is
    unreachable, as in `split_unreachable`. Anything that cannot be placed
    raises Undetermined.
    """
    if not entries:
        return [], [], []
    full_len = object_id_length(repo)
    outstanding, unreachable, not_in_push = [], [], []
    for e in entries:
        sha = e["commit"]
        if object_is_missing(repo, sha, full_len):
            unreachable.append(e)
        elif any(in_pushed_range(repo, sha, lo, rm) for lo, rm in pushes):
            outstanding.append(e)
        else:
            not_in_push.append(e)
    return outstanding, unreachable, not_in_push


def split_unreachable(
    repo: Path | None, entries: list[dict[str, str]]
) -> tuple[list[dict[str, str]], list[dict[str, str]]]:
    """Partition entries into (outstanding, unreachable) when no push is being
    judged (the status form, invoked without `--push`).

    A commit reachable from no ref and no worktree HEAD
    cannot leave this machine, and no pre-commit run can ever clear it (it is
    an ancestor of no HEAD). It is reported, never deleted, and does not block.
    A commit whose object is definitively missing is unreachable too (user
    ruling 2026-10-20, backlog 860c43db): it has no content to push.
    """
    if not entries:
        return [], []
    full_len = object_id_length(repo)
    tips = head_tips(repo)
    outstanding, unreachable = [], []
    for e in entries:
        sha = e["commit"]
        if object_is_missing(repo, sha, full_len) or not is_reachable(repo, sha, tips):
            unreachable.append(e)
        else:
            outstanding.append(e)
    return outstanding, unreachable


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", type=Path, default=None)
    ap.add_argument("--json", action="store_true")
    ap.add_argument(
        "--push",
        nargs=2,
        action="append",
        default=[],
        metavar=("LOCAL", "REMOTE"),
        help="a pushed (local sha, remote sha) pair from pre-push stdin; when "
        "given, only entries this push carries are outstanding",
    )
    args = ap.parse_args(argv[1:])

    not_in_push: list[dict[str, str]] = []
    try:
        ledger = common_git_dir(args.repo) / "gate-bypassed"
        entries = read_ledger(ledger)
        if args.push:
            entries, unreachable, not_in_push = split_for_push(
                args.repo, entries, args.push
            )
        else:
            entries, unreachable = split_unreachable(args.repo, entries)
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
                    "not_in_push": not_in_push,
                },
                ensure_ascii=False,
            )
        )
        return EXIT_OUTSTANDING if entries else EXIT_CLEAN

    if unreachable:
        print(
            "gate-bypass: %d ledger entr%s for commits reachable from no ref\n"
            "or worktree HEAD, or absent from the object store\n"
            "(unreachable; cannot be pushed, kept in the ledger, not blocking):" % (len(unreachable), "y" if len(unreachable) == 1 else "ies"),
            file=sys.stderr,
        )
        for e in unreachable:
            print("  %s  %s" % (e["commit"][:12], e["subject"]), file=sys.stderr)
        print("", file=sys.stderr)

    if not_in_push:
        print(
            "gate-bypass: %d ledger entr%s for commits this push does not carry\n"
            "(not reachable from a pushed tip, or already on the remote ref;\n"
            "kept in the ledger, not blocking this push):"
            % (len(not_in_push), "y" if len(not_in_push) == 1 else "ies"),
            file=sys.stderr,
        )
        for e in not_in_push:
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
