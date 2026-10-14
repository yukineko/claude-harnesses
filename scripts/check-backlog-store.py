#!/usr/bin/env python3
"""Block a commit whose `.backlog/*.toml` carries conflict markers or is not TOML.

Why this gate exists (backlog ece51b35)
---------------------------------------
Measured 2026-09-24: the merge-completion commit 0d68e5d4 recorded a
`.backlog/tasks.toml` containing `<<<<<<<` conflict markers, and it was
fast-forwarded onto main. Nothing stopped it: post-merge only listed it as an
UNGATED COMMIT. The store is then unreadable to every `backlog` call
("failed to parse <path>") until someone repairs it by hand (24a00986).

What it checks
--------------
Every `.backlog/*.toml` blob recorded in the git INDEX — the content the commit
will actually contain, not the working tree. Each must:

  1. contain no conflict-marker line (`<<<<<<< `, `=======`, `>>>>>>> `, `|||||||`
     at the start of a line), and
  2. parse as TOML (`tomllib`).

The whole store is checked, not just the staged diff: the index IS the commit,
and a broken store is broken whether or not this commit is the one that
touched it.

Exit codes (fail-closed: "cannot determine" resolves to the restricted side)
    0  every indexed .backlog/*.toml is clean TOML with no markers
    1  at least one file has markers or does not parse
    2  undetermined: git failed, a blob could not be read, or tomllib is missing
"""
from __future__ import annotations

import re
import subprocess
import sys

MARKER_RE = re.compile(r"^(<{7} |>{7} |={7}$|\|{7} )", re.MULTILINE)


def git(*args: str) -> bytes:
    proc = subprocess.run(["git", *args], capture_output=True, timeout=60)
    if proc.returncode != 0:
        raise RuntimeError(
            f"git {' '.join(args)} exited {proc.returncode}: "
            f"{proc.stderr.decode(errors='replace').strip()}"
        )
    return proc.stdout


def main() -> int:
    try:
        import tomllib
    except ImportError:
        print("backlog-store: tomllib unavailable (python < 3.11) — cannot determine", file=sys.stderr)
        return 2
    try:
        listing = git("ls-files", "-z", "--cached", "--", ".backlog")
    except (RuntimeError, OSError, subprocess.TimeoutExpired) as e:
        print(f"backlog-store: cannot list the index: {e}", file=sys.stderr)
        return 2
    paths = [p for p in listing.decode().split("\0") if p.endswith(".toml")]

    bad = []
    for path in paths:
        try:
            text = git("show", f":{path}").decode("utf-8")
        except (RuntimeError, OSError, subprocess.TimeoutExpired, UnicodeDecodeError) as e:
            print(f"backlog-store: cannot read the indexed {path}: {e}", file=sys.stderr)
            return 2
        m = MARKER_RE.search(text)
        if m:
            line = text.count("\n", 0, m.start()) + 1
            bad.append(f"{path}:{line}: conflict marker {m.group(0).strip()!r}")
            continue
        try:
            tomllib.loads(text)
        except tomllib.TOMLDecodeError as e:
            bad.append(f"{path}: not valid TOML: {e}")

    if bad:
        print("backlog-store: the backlog store about to be committed is broken:", file=sys.stderr)
        for b in bad:
            print(f"  {b}", file=sys.stderr)
        print(
            "  Resolve the merge in the store (or re-run `backlog merge-driver`) before committing.",
            file=sys.stderr,
        )
        return 1
    print(f"backlog-store: {len(paths)} indexed .backlog/*.toml file(s) clean.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
