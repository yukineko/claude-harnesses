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

The ledger is cleared by a pre-commit run that goes green, not by this script.
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


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", type=Path, default=None)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv[1:])

    try:
        ledger = common_git_dir(args.repo) / "gate-bypassed"
        entries = read_ledger(ledger)
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
                },
                ensure_ascii=False,
            )
        )
        return EXIT_OUTSTANDING if entries else EXIT_CLEAN

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
        "successful pre-commit clears the ledger. There is deliberately no\n"
        "--clear flag: clearing is a side effect of inspection, not a request.",
        file=sys.stderr,
    )
    return EXIT_OUTSTANDING


if __name__ == "__main__":
    sys.exit(main(sys.argv))
